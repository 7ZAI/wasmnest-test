//! 场景（两进程版，2026-10-15）：终端流跨端闭环——**桌面真实服务器进程**
//! （`desktop-harness` bin：真认证中心插件 + 真 bash PTY）↔ **移动真插件产物**
//! （`com.wasmnest.terminal-session` 移动侧 wasm，本进程内真实运行 + 真宿主窄转发）
//! 经真实 TCP 互连。
//!
//! ## 与单进程版 `terminal_ws_flow.rs` 的关系
//!
//! 同一行为契约 C-001..C-009（判据函数逐字一致），**一处形态差异**：
//! 单进程 rig 在同一进程里装两端宿主域实现，移动真插件只能跑在桌面宿主实现上
//! （`#[ignore]`，见其头注）；本版把桌面侧拆成**独立进程**——本测试二进制只链移动侧
//! （依赖图 = 本包 `two-proc` 模式：cargo feature 把桌面 lib 出图 ⇒ server-crate 的
//! `common-host` 无人开启；spec §5.1 实测），移动宿主域实现（host-http / host-websocket /
//! terminal_stream 窄转发）无竞争安装，移动插件的 WS / 页面通道走的是**移动生产实现**。
//!
//! ## 会话生命周期（start/stop）为什么走桌面命令面而不是移动插件 HTTP
//!
//! 移动插件的 `start-session` / `stop-session` 经 **host-http**（移动 egress 闸门）。
//! 该闸门对无头 / 测试上下文**有意 fail-closed**（`plugin/host_domains/http.rs` 的
//! `headless_context_denied_by_egress` 用例钉着：弹窗事务没有 AppHandle 载体，收紧为
//! 判定失败而非放行）——headless 拿不到 `tauri::AppHandle`（Wry，`test` feature 只造
//! MockRuntime），这是**结构性边界**，不是装配问题。因此本场景的会话创建/停止改走
//! **桌面命令面** `session.ws.control`（与生产 WS 会话控制通道共用 `ws_control` 域实现，
//! 桌面 UI 同一入口），被测的跨端面——移动插件 WS 链路（prewarm/subscribe/send-input/
//! session_stopped 传播）——保持全真。移动插件 host-http 的 wire 契约另由激活场景
//! `event_channel_flow.rs`（reqwest 直打）+ 移动单端 `tests/session_http_flow.rs`
//! （就地端口 impl）承接；「L1 桌面端目标在无头 CI 下是否放行」是安全相关待裁决项
//! （见 spec §5.1），不得单方面改。
//!
//! 运行：`cargo build -p wasmnest-crossend-desktop-harness`（先建 harness）→
//! `cargo test -p cross-end-tests --no-default-features --features two-proc \
//!   --test two_proc_terminal_ws -- --nocapture`。
//!
//! 行为契约：
//! | 契约 | 行为 |
//! |---|---|
//! | C-001 | 桌面命令面 `session.ws.control` start_session → 桌面建真会话（真 PTY）→ 非空 sessionId |
//! | C-002 | 移动插件 `prewarm` 只建连：桌面端点出现已认证客户端，但**页面通道零字节** |
//! | C-003 | `subscribe` → 桌面 `subscribed` + 环窗口重放 → 页面通道收到 shell 提示符字节 |
//! | C-004 | `send-input`（文本 + enter 一次调用）→ 命令真的执行：marker 独占一行出现 |
//! | C-005 | 批量输出连续性：SEQ_1..SEQ_N 每行恰好一次且严格递增 |
//! | C-006 | 重订阅 = fresh subscribe：桌面整段重放（回放头 marker 再次出现） |
//! | C-007 | 桌面命令面 stop_session → 桌面广播 session_stopped → 移动插件链路停止，页面通道不再增长 |
//! | C-008 | 全程宿主窄转发零丢弃（`gateway_stats.dropped_*` == 0） |
//! | C-009 | 客户端不 ack 的压力路径：400 行序列仍完整递增无重复（背压逃生阀） |
//!
//! ## 判据函数
//!
//! [`seq_lines`] 按终端语义 `\r`/`\n` 都分行（bash 复位 bracketed paste 会把首行粘进
//! ANSI 前缀，只按 `\n` 分行会让首行静默丢失）——与单进程版同实现。

mod two_proc_common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use wasmnest_mobile_lib::auth::http::{AuthHttpClient, DeviceAuthContext};

use two_proc_common::desktop::{self, DesktopHarness, SESSION_PLUGIN_ID};
use two_proc_common::mobile_ctx;
use two_proc_common::mobile_plugin::MobilePluginWorld;

/// 单次等待预算（真实 wasm 组件 + 真实 PTY，CI 慢机放宽）
const WAIT: Duration = Duration::from_secs(20);

/// 页面通道字节里「独占一行」出现的序列号（按出现序，严格判序用）
///
/// 终端字节流是 CRLF + ANSI 控制序列（bash 执行命令前会复位 bracketed paste：
/// `\x1b[?2004l` 后接 `\r` 回列首，再输出首行），「独占一行」按**终端语义**提取：
/// `\r` 与 `\n` 都视为行断（CR 回列首 = 该位置开始新行），行首/行尾空白剥离。
/// 只按 `\n` 分行会让 `\x1b[?2004l\rSEQ_1` 粘成一行、行首判据落空（首行静默丢失）。
fn seq_lines(text: &str, prefix: &str) -> Vec<String> {
    text.split(['\r', '\n'])
        .map(|l| l.trim())
        .filter(|l| l.starts_with(prefix) && l.len() > prefix.len())
        .map(str::to_string)
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_pty_output_reaches_mobile_plugin_page_channel() {
    if tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::DEBUG)
        .try_init()
        .is_err()
    {
        tracing::debug!("tracing subscriber already initialized");
    }

    // ==================== 桌面进程（真实服务器 + 真实认证中心插件） ====================
    let mut desktop = DesktopHarness::spawn_and_ready().await;
    let port = desktop.port;
    mobile_ctx::set_target(port).await;

    // ==================== 认证（真实配对 → 真实 token → 移动全局） ====================
    let base = mobile_ctx::base_url(port);
    let address = format!("127.0.0.1:{port}");
    let auth = AuthHttpClient::new();
    let ctx = DeviceAuthContext {
        device_id: "crossend-term-device",
        device_name: "CrossEnd Terminal",
        fingerprint: "crossend-term-fp",
        uid_hash: None,
    };
    let pairing = auth
        .request_pairing(&base, ctx.device_id, ctx.device_name, ctx.fingerprint)
        .await
        .expect("配对码签发");
    let token = auth
        .verify_pairing_code(&base, ctx, &pairing.pairing_code, &address)
        .await
        .expect("配对码换 token")
        .token;
    mobile_ctx::remember_token(&token);

    // ==================== 移动插件世界（真产物 + 无头宿主） ====================
    let world = MobilePluginWorld::load_terminal_plugin(port, &token).await;
    let outputs = Arc::new(mobile_ctx::OutputRecorder::default());

    // ==================== C-001 启动真实会话（桌面命令面 ws_control → 真 PTY） ====================
    // 会话生命周期走桌面命令面（与生产 WS 会话控制域同一实现；移动插件 host-http 被
    // egress 闸门无头 fail-closed 挡，见文件头注）。跨端被测面是随后的 WS 终端链路。
    let config_id = desktop.seed_shell_config("cross-end-terminal").await;
    let start = desktop
        .plugin_command(
            SESSION_PLUGIN_ID,
            "session.ws.control",
            serde_json::json!({ "action": { "type": "start_session", "config_id": config_id } }),
        )
        .await;
    let session_id = start["session_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| panic!("C-001 桌面命令面 start_session 响应缺 session_id：{start}"))
        .to_string();
    assert!(!session_id.is_empty(), "C-001 sessionId 不得为空");

    // ==================== C-002 预热只建连：页面通道零字节 ====================
    // 页面通道先就位（真机是 markPageEntered 先登记、页面挂载后再 subscribe）
    world.page_subscribe(&session_id, mobile_ctx::output_channel(outputs.clone()));
    world
        .invoke(
            "terminal-session.prewarm",
            serde_json::json!({ "sessionId": session_id }),
        )
        .await
        .expect("C-002 prewarm 必须可调用");
    // 桌面端点出现已认证客户端（证明连接真的建起来了，不是「什么都没发生所以零字节」）
    // 该 helper 内部等待到「有已认证客户端」为止（等不到即 panic，不静默返回）
    desktop
        .wait_ws_endpoint_authenticated(
            SESSION_PLUGIN_ID,
            "terminal",
            "C-002 prewarm 后桌面终端端点应有已认证客户端",
        )
        .await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(
        outputs.text().len(),
        0,
        "C-002 prewarm 只建连、不得触发重放（收到字节说明 prewarm 走了 subscribe 语义）：{:?}",
        outputs.text()
    );

    // ==================== C-003 订阅 → 环窗口重放到达页面通道 ====================
    let head_marker = "CROSS_END_HEAD_7F3A";
    world
        .invoke(
            "terminal-session.subscribe",
            serde_json::json!({ "sessionId": session_id }),
        )
        .await
        .expect("C-003 subscribe 必须可调用");
    world.wait_live(&session_id).await;
    // 先让 shell 打一条独占一行的 head marker（用于后面的重放语义判据）
    world
        .invoke(
            "terminal-session.send-input",
            serde_json::json!({ "sessionId": session_id, "data": format!("echo {head_marker}"), "specialKey": "enter" }),
        )
        .await
        .expect("C-003 发送 head marker 命令必须成功");
    mobile_ctx::wait_until("C-003 head marker 到达移动端页面通道", || {
        mobile_ctx::count_line_occurrences(&outputs.text(), head_marker) >= 1
    })
    .await;

    // ==================== C-004 文本 + Enter 一次调用真的执行命令 ====================
    // （历史缺陷：互斥分支导致命令文本被丢、只剩裸回车；见 protocol.rs 注释）
    let ws_marker = "CROSS_END_WS_INPUT_4B2E";
    world
        .invoke(
            "terminal-session.send-input",
            serde_json::json!({ "sessionId": session_id, "data": format!("echo {ws_marker}"), "specialKey": "enter" }),
        )
        .await
        .expect("C-004 send-input 必须成功");
    mobile_ctx::wait_until("C-004 marker 独占一行出现在页面通道", || {
        mobile_ctx::count_line_occurrences(&outputs.text(), ws_marker) >= 1
    })
    .await;

    // ==================== C-005 批量输出连续性：无缺口 / 无重复 / 不乱序 ====================
    const SEQ_N: usize = 120;
    world
        .invoke(
            "terminal-session.send-input",
            serde_json::json!({
                "sessionId": session_id,
                "data": format!("for i in $(seq 1 {SEQ_N}); do echo SEQ_$i; done"),
                "specialKey": "enter",
            }),
        )
        .await
        .expect("C-005 批量输出命令必须成功");
    mobile_ctx::wait_until(&format!("C-005 SEQ_{SEQ_N} 到达页面通道"), || {
        mobile_ctx::count_line_occurrences(&outputs.text(), &format!("SEQ_{SEQ_N}")) >= 1
    })
    .await;
    let seen = seq_lines(&outputs.text(), "SEQ_");
    let expected: Vec<String> = (1..=SEQ_N).map(|i| format!("SEQ_{i}")).collect();
    let matched: Vec<String> = seen.iter().filter(|l| expected.contains(l)).cloned().collect();
    assert_eq!(
        matched, expected,
        "C-005 序列行必须严格递增且每行恰好一次（缺口/重复/乱序都会在此暴露）；实际={seen:?}\n原始字节前缀={:?}",
        String::from_utf8_lossy(&outputs.bytes()[..outputs.bytes().len().min(1200)])
    );

    // ==================== C-006 重订阅 = fresh subscribe（整段重放） ====================
    let bytes_before_resub = outputs.bytes().len();
    world
        .invoke(
            "terminal-session.subscribe",
            serde_json::json!({ "sessionId": session_id }),
        )
        .await
        .expect("C-006 重订阅必须可调用");
    mobile_ctx::wait_until("C-006 重放字节到达", || {
        outputs.bytes().len() > bytes_before_resub
    })
    .await;
    // 等待重放推进到 head marker（环窗口含它；环容量 10 MiB（宿主上限）≫ 本次输出量 ⇒ 必须包含）
    let deadline = Instant::now() + WAIT;
    loop {
        let delta = String::from_utf8_lossy(&outputs.bytes()[bytes_before_resub..]).to_string();
        if mobile_ctx::count_line_occurrences(&delta, head_marker) >= 1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "C-006 重订阅必须整段重放（回放应含早期 head marker）；delta_len={} 前缀={:?}",
            delta.len(),
            delta.chars().take(200).collect::<String>()
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
    }

    // ==================== C-008 窄转发零丢弃（页面订阅期间） ====================
    let stats = world.gateway_stats();
    assert_eq!(
        stats.dropped_no_channel, 0,
        "C-008 页面已订阅期间不得有无通道丢弃：{stats:?}"
    );
    assert_eq!(stats.dropped_send_failed, 0, "C-008 页面通道发送不得失败：{stats:?}");
    assert!(stats.forwarded_chunks > 0, "C-008 必须有成功转发：{stats:?}");

    // ==================== C-009 压力：客户端不 ack 也不丢字节（环 + 逃生阀） ====================
    //
    // 本 harness 的页面通道不会回发 ack（真机由前端 xterm 解析完成回调驱动）⇒ 桌面端
    // 水位永远不会回落，必然会进入驻留并在连续 4 轮抑制后走**抗死锁强制放行**。
    // 这条路径下字节仍必须完整到达（环 10 MiB 未淘汰；背压的失败模式是「多发」不是「不发」）。
    const PSEQ_N: usize = 400;
    let pseq_begin = outputs.bytes().len();
    world
        .invoke(
            "terminal-session.send-input",
            serde_json::json!({
                "sessionId": session_id,
                "data": format!("for i in $(seq 1 {PSEQ_N}); do echo PSEQ_$i; done"),
                "specialKey": "enter",
            }),
        )
        .await
        .expect("C-009 压力输出命令必须成功");
    mobile_ctx::wait_until(&format!("C-009 PSEQ_{PSEQ_N} 到达页面通道"), || {
        mobile_ctx::count_line_occurrences(&outputs.text(), &format!("PSEQ_{PSEQ_N}")) >= 1
    })
    .await;
    let delta = String::from_utf8_lossy(&outputs.bytes()[pseq_begin..]).to_string();
    let pseen: Vec<String> = seq_lines(&delta, "PSEQ_")
        .into_iter()
        .filter(|l| {
            l.strip_prefix("PSEQ_")
                .and_then(|n| n.parse::<usize>().ok())
                .is_some_and(|n| (1..=PSEQ_N).contains(&n))
        })
        .collect();
    let pexpected: Vec<String> = (1..=PSEQ_N).map(|i| format!("PSEQ_{i}")).collect();
    assert_eq!(
        pseen, pexpected,
        "C-009 无 ack 压力下序列仍必须完整、递增且不重复（缺口/重复在此暴露）"
    );
    let stats = world.gateway_stats();
    assert_eq!(stats.dropped_no_channel, 0, "C-009 压力下不得有丢弃：{stats:?}");

    // ==================== C-007 停止 → 链路终态 + 通道不再增长 ====================
    // 桌面命令面 stop_session（生产同一实现）；桌面端经 terminal WS 广播 session_stopped，
    // 移动插件链路随之停止——跨端停止传播是 C-007 的被测面。
    desktop
        .plugin_command(
            SESSION_PLUGIN_ID,
            "session.ws.control",
            serde_json::json!({ "action": { "type": "stop_session", "session_id": session_id } }),
        )
        .await;
    world
        .wait_state(&session_id, "停止后链路 stopped=true", |s| s["stopped"] == true)
        .await;
    let bytes_after_stop = outputs.bytes().len();
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(
        outputs.bytes().len(),
        bytes_after_stop,
        "C-007 停止后页面通道不得再增长（终态后仍推字节 = 状态机漏关）"
    );

    // ==================== 收尾 ====================
    world
        .invoke(
            "terminal-session.remove",
            serde_json::json!({ "sessionId": session_id }),
        )
        .await
        .expect("remove 必须可调用");
    mobile_ctx::clear_identity();
    desktop.shutdown().await;
}
