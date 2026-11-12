//! 移动端客户端装配：目标设备 / 全局 token / 事件与终端输出记录替身
//!
//! 替身口径（**只记录、不伪造**）：移动端链路把「状态变化」与「终端输出字节」
//! 交给页面侧出口，真实形态是 Tauri WebView。跨端测试里没有 WebView，用
//! 记录替身承接**已经真实发生**的数据：
//! - [`EventRecorder`] 承接 `TerminalEventSink`（链路状态流转的事实记录）；
//! - [`OutputRecorder`] 承接页面级 `Channel<InvokeResponseBody>`（PTY 真实
//!   输出字节的落点）。
//!
//! 替身不参与任何协议应答：桌面端返回的每一帧都由移动端真实客户端解析，
//! 替身只被「读」用于断言。

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use wasmnest_mobile_lib::router::MobileEvent;
use wasmnest_mobile_lib::state::{clear_global_token, get_connection_manager, set_global_token};
use tauri::ipc::{Channel, InvokeResponseBody};

/// 单次测试的等待预算（CI 慢机放宽：spec §4 预算 5s，此处 15s）
pub const WAIT_TIMEOUT: Duration = Duration::from_secs(15);

/// 把移动端目标设备指向本进程内的桌面端无头服务器
///
/// `set_target` 是 HTTP 认证与终端链路 WS 的**地址真源**（移动端每个请求
/// 经 `resolve_base_url` / 链路 `connect_once` 从它取地址），因此这一行就是
/// 「移动端连上桌面端」的全部接线。
pub async fn set_target(port: u16) {
    set_target_at("127.0.0.1", port).await
}

/// [`set_target`] 的显式主机版（链路加密等需要**非环回**对端的用例：
/// 桌面端加密过滤器豁免环回对端，见测试内说明）
pub async fn set_target_at(address: &str, port: u16) {
    get_connection_manager()
        .set_target(address.to_string(), port, Some("cross-end-desktop".to_string()))
        .await;
}

/// 目标设备 base URL（移动端自算，与生产路径同函数）
pub fn base_url(port: u16) -> String {
    wasmnest_mobile_lib::auth::http::format_base_url("127.0.0.1", port)
}

/// 场景收尾：清全局 token（防下一场景残留已认证身份）
pub fn clear_identity() {
    clear_global_token();
}

/// 记录 JWT（生产路径由认证中心签发后写入全局，移动端后续请求与 WS 首帧共用）
pub fn remember_token(token: &str) {
    set_global_token(token);
}

/// 终端链路状态事件记录替身（`TerminalEventSink`——已随终端链路退役，见
/// 移动端票 13/15：host-terminal 整面下沉插件；本记录器保留自身快照方法供
/// 断言，sink 接线由仍在服役的终端场景重写后恢复）
#[derive(Default)]
pub struct EventRecorder {
    events: Mutex<Vec<(String, serde_json::Value)>>,
}

impl EventRecorder {
    pub fn snapshot(&self) -> Vec<(String, serde_json::Value)> {
        self.events.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// `terminal-state` 的 detail 序列（按到达序，用于断言状态流转）
    pub fn state_details(&self) -> Vec<String> {
        self.snapshot()
            .iter()
            .filter(|(e, _)| e == "terminal-state")
            .filter_map(|(_, p)| p.get("detail").and_then(|v| v.as_str()).map(str::to_string))
            .collect()
    }

    /// `terminal-state` 的 phase 序列
    pub fn state_phases(&self) -> Vec<String> {
        self.snapshot()
            .iter()
            .filter(|(e, _)| e == "terminal-state")
            .filter_map(|(_, p)| p.get("phase").and_then(|v| v.as_str()).map(str::to_string))
            .collect()
    }

    /// `terminal-resync` 事件次数（重锚信号）
    pub fn resync_count(&self) -> usize {
        self.snapshot().iter().filter(|(e, _)| e == "terminal-resync").count()
    }

    /// 最后一个 `terminal-state` 且 `detail` 匹配的载荷（无匹配则 None）
    pub fn last_state_payload(&self, detail: &str) -> Option<serde_json::Value> {
        self.snapshot()
            .into_iter()
            .rev()
            .find(|(e, p)| e == "terminal-state" && p.get("detail").and_then(|v| v.as_str()) == Some(detail))
            .map(|(_, p)| p)
    }
}

/// 页面级终端输出记录器（`Channel` 回调的落点）
#[derive(Default)]
pub struct OutputRecorder {
    chunks: Mutex<Vec<Vec<u8>>>,
}

impl OutputRecorder {
    /// 至此已推给页面的全部输出字节（按帧序拼接）
    pub fn bytes(&self) -> Vec<u8> {
        self.chunks
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .flatten()
            .copied()
            .collect()
    }

    /// 已推字节按 UTF-8 有损解码（PTY 断言用 ASCII marker）
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes()).to_string()
    }

    pub fn frame_count(&self) -> usize {
        self.chunks.lock().unwrap_or_else(|p| p.into_inner()).len()
    }

    /// 帧边界（`(该帧起始字节偏移, 该帧长度)` 列表）
    ///
    /// 取证用：把「某个字节偏移」映射回「它落在哪一帧 / 帧内第几字节」，即可判定
    /// 字节级错位发生在**帧内部**（环 / 插件拼装）还是**帧边界上**（传输 / 转发）。
    pub fn frame_spans(&self) -> Vec<(usize, usize)> {
        let mut spans = Vec::new();
        let mut offset = 0usize;
        for chunk in self.chunks.lock().unwrap_or_else(|p| p.into_inner()).iter() {
            spans.push((offset, chunk.len()));
            offset += chunk.len();
        }
        spans
    }
}

/// 造一个把移动端终端输出记到 `recorder` 的页面级 Channel
///
/// 生产形态是 Tauri `Channel<InvokeResponseBody>` 推给终端页 WebView；这里用
/// 同类型同构造（`Channel::new` 回调同步触发），把 `Raw` 裸字节原样攒起来。
/// 非 `Raw` 形态（Json 兜底）也一并记录，避免静默丢形态。
pub fn output_channel(recorder: Arc<OutputRecorder>) -> Channel<InvokeResponseBody> {
    let sink = recorder.clone();
    Channel::new(move |body| {
        match body {
            InvokeResponseBody::Raw(bytes) => {
                sink.chunks.lock().unwrap_or_else(|p| p.into_inner()).push(bytes);
            }
            other => {
                // 兜底形态：显性留痕而不是静默丢弃（终端输出恒为 Raw，
                // 出现别的形态说明链路改道了）
                sink.chunks
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push(format!("[non-raw:{other:?}]").into_bytes());
            }
        }
        Ok(())
    })
}

/// 移动端业务事件记录器（`ConnectionManager` 的 `MobileEvent` 广播落点）
///
/// 生产形态是 `router::event::start_event_forwarding` 把 `MobileEvent` 转成 Tauri
/// 事件推给页面 WebView；无头环境没有 WebView，这里只记录**已真实发生**的
/// `MobileEvent`（每一帧都由桌面真实插件广播、经移动端真实 WS 客户端与事件路由
/// 解析后才落进来），不伪造任何协议应答。
#[derive(Default)]
pub struct MobileEventRecorder {
    events: Mutex<Vec<MobileEvent>>,
}

impl MobileEventRecorder {
    /// 订阅 `ConnectionManager` 事件总线并持续落盘
    ///
    /// 必须在触发任何业务动作**之前**挂上（广播只投递给当时的订阅者）。
    pub fn attach() -> Arc<Self> {
        let recorder = Arc::new(Self::default());
        let mut rx = get_connection_manager().subscribe();
        let sink = recorder.clone();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(event) => sink.events.lock().unwrap_or_else(|p| p.into_inner()).push(event),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        // 丢帧必须留痕：否则断言可能把「记录器没收到」误读成「对端没广播」
                        tracing::warn!("cross-end: MobileEvent 广播滞后 {n} 帧（记录器丢帧）");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        recorder
    }

    /// 已落盘的事件快照（按到达序）
    pub fn snapshot(&self) -> Vec<MobileEvent> {
        self.events.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// 快照中满足谓词的条数（用于「一条都不许有」的反例断言）
    pub fn count(&self, mut pred: impl FnMut(&MobileEvent) -> bool) -> usize {
        self.snapshot().iter().filter(|e| pred(e)).count()
    }

    /// 轮询等待第一条满足谓词的 `MobileEvent`（超时 panic 并打印现场）
    ///
    /// 事件是异步广播：断言必须轮询等，不得用定值 sleep 赌时序。
    pub async fn wait_for(&self, what: &str, mut pred: impl FnMut(&MobileEvent) -> bool) -> MobileEvent {
        let deadline = Instant::now() + WAIT_TIMEOUT;
        loop {
            let snapshot = self.snapshot();
            if let Some(found) = snapshot.iter().find(|e| pred(e)) {
                return found.clone();
            }
            if Instant::now() >= deadline {
                panic!("timed out waiting for {what}; events={snapshot:#?}");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

/// 在给定预算内断言「一条满足谓词的事件都**没有**出现」（反例：断连期间不重放、
/// 认证未过时不路由）。返回是否真的保持静默，供调用方在失败时补充上下文。
pub async fn assert_no_mobile_event(
    recorder: &Arc<MobileEventRecorder>,
    what: &str,
    budget: Duration,
    mut pred: impl FnMut(&MobileEvent) -> bool,
) {
    let deadline = Instant::now() + budget;
    loop {
        let offending = recorder.snapshot().into_iter().find(|e| pred(e));
        if let Some(event) = offending {
            panic!("{what}：不该出现的事件真的出现了 = {event:?}");
        }
        if Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 轮询直到 `pred` 为真（超时 panic 并打印现场，防卡死）
pub async fn wait_until(what: &str, mut pred: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    loop {
        if pred() {
            return;
        }
        if Instant::now() >= deadline {
            panic!("timed out waiting for {what}");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 轮询等待 `recorder` 收到满足谓词的状态事件
pub async fn wait_event(
    recorder: &Arc<EventRecorder>,
    what: &str,
    mut pred: impl FnMut(&[(String, serde_json::Value)]) -> bool,
) {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    loop {
        if pred(&recorder.snapshot()) {
            return;
        }
        if Instant::now() >= deadline {
            panic!("timed out waiting for {what}; events={:#?}", recorder.snapshot());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 轮询等待输出记录器出现 `marker`
pub async fn wait_output(recorder: &Arc<OutputRecorder>, marker: &str) {
    wait_until(&format!("output marker {marker:?}"), || {
        recorder.text().contains(marker)
    })
    .await;
}

/// 统计 marker 作为**独占一行**出现的次数（行首 + 行尾均为行边界）
///
/// **为什么不能直接数 marker 出现次数**：真实 PTY 开回显，bash 每次重绘提示符
/// 都把当前输入行整行重发（`]0;title` + prompt + 已键入文本），同一个 marker
/// 会在输出流里出现多次而命令**从未执行**——变异探针实测：去掉行锚定后，
/// 「Enter 被丢弃」这个变异会伪装成通过（断言恒真）。
/// 只有「独占一行」的形态才可能来自命令的真实输出，因此判「命令是否执行」
/// 一律用本函数。
pub fn count_line_occurrences(text: &str, marker: &str) -> usize {
    // 空 marker 会让 `find("")` 恒返回 Some(0) 且 from 不前进 → 无限循环（测试进程卡死）。
    // 当前调用方都传非空常量，但本函数是 pub 通用判据，入口就拦住非法参数。
    if marker.is_empty() {
        return 0;
    }
    let bytes = text.as_bytes();
    let mut count = 0usize;
    let mut from = 0usize;
    while let Some(rel) = text.get(from..).and_then(|s| s.find(marker)) {
        let start = from + rel;
        let end = start + marker.len();
        let line_start = start == 0 || matches!(bytes[start - 1], b'\n' | b'\r');
        let line_end = end == bytes.len() || matches!(bytes[end], b'\n' | b'\r');
        if line_start && line_end {
            count += 1;
        }
        from = end;
    }
    count
}
