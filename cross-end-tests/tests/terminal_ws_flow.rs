//! 场景 4（2026-10-15 重写）：终端流跨端闭环——桌面**真实插件 + 真实 bash PTY**
//! ↔ 移动**真实插件产物**（wasm）+ 真实宿主页面窄转发
//!
//! ## 为什么重写
//!
//! 旧版驱动的是宿主 `wasmnest_mobile_lib::terminal_link`（1,363 行）——票 12/15 把
//! 终端订阅协议整体下沉移动 wasm 插件后，该模块已删除，本场景随之**不编译**（门禁
//! 死了一段时间，2026-10-15 修复）。新版对面是移动端**随包真产物**
//! （`com.wasmnest.bedcode` 移动侧 wasm）在无头宿主上下文里真实运行：
//! WS 连接 / jwt 首帧 / 输出门控 / 窄转发全部是真实现，桌面端每一帧都由真实插件 + 真实
//! PTY 生成。
//!
//! ## 行为契约
//!
//! | 契约 | 行为 | 场景 |
//! |---|---|---|
//! | C-001 | 移动插件 `start-session`（HTTP + 宿主代注 JWT）→ 桌面建真会话 → 非空 sessionId | 正例 |
//! | C-002 | `prewarm` 只建连：桌面端点出现已认证客户端，但**页面通道零字节**（走 subscribe 预热会白传一遍整段重放） | 正例（防回归） |
//! | C-003 | `subscribe` → 桌面 `subscribed` + 环窗口重放 → 页面通道收到 shell 提示符字节 | 正例（跨端时序） |
//! | C-004 | `send-input`（文本 + enter 一次调用）→ 命令真的执行：marker **独占一行**出现 | 正例（输入回归锁：文本曾被静默丢弃） |
//! | C-005 | 批量输出连续性：`SEQ_1..SEQ_N` 每行**恰好一次**且严格递增（无缺口 / 无重复 / 不乱序） | 正例（字节连续性主判据） |
//! | C-006 | 重订阅 = fresh subscribe：桌面**整段重放**（回放头 marker 再次出现）——前端「重放世代清屏」规则依赖这条语义 | 正例（协议语义锁） |
//! | C-007 | `stop-session` → 桌面 `session_stopped` → 移动插件链路停止（`get-state.stopped=true`），此后页面通道不再增长 | 正例（终态） |
//! | C-008 | 全程宿主窄转发零丢弃（`gateway_stats.dropped_*` == 0） | 正例（可观测闭环） |
//! | C-009 | **客户端不 ack 的压力路径**：桌面水位驻留 + 抗死锁强制放行下，400 行序列仍完整递增无重复 | 正例（背压逃生阀） |
//!
//! 旧的 `terminal_output_pressure.rs` / `terminal_resync_ordering.rs` 已删除：它们驱动的是
//! 退役的宿主 `terminal_link`，且其中「重锚信号先于重放字节」类契约在无头 harness 里
//! 不可观测（插件事件出口需真 AppHandle）。压力/连续性契约由本文件 C-005/C-009 承接；
//! 重锚消费侧契约由前端 vitest（`store.test.ts` 的世代清屏 / resync 用例）承接。

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use wasmnest_mobile_lib::auth::http::{AuthHttpClient, DeviceAuthContext};

use common::desktop_ctx::{self, SESSION_PLUGIN_ID};
use common::mobile_ctx;
use common::mobile_plugin::MobilePluginWorld;

/// 单次等待预算（真实 wasm 组件 + 真实 PTY，CI 慢机放宽）
const WAIT: Duration = Duration::from_secs(20);

/// 页面通道字节里「独占一行」出现的序列号（按出现序，严格判序用）
///
/// 终端字节流是 CRLF + ANSI 控制序列（bash 执行命令前会复位 bracketed paste：
/// `\x1b[?2004l` 后接 `\r` 回列首，再输出首行），「独占一行」按**终端语义**提取：
/// `\r` 与 `\n` 都视为行断（CR 回列首 = 该位置开始新行），行首/行尾空白剥离。
/// 只按 `\n` 分行会让 `\x1b[?2004l\rSEQ_1` 粘成一行、行首判据落空（首行静默丢失）——
/// 与两进程版 `tests/two_proc_terminal_ws.rs` 同实现（判据函数逐字一致）。
fn seq_lines(text: &str, prefix: &str) -> Vec<String> {
    text.split(['\r', '\n'])
        .map(|l| l.trim())
        .filter(|l| l.starts_with(prefix) && l.len() > prefix.len())
        .map(str::to_string)
        .collect()
}

#[ignore = "单进程 rig 装不下「两端宿主域实现」：`inventory` 是进程级注册，桌面与移动对 host-http/websocket 等端差异域各有一份实现，后装者必然冲突（现由 `HostModule::fallback` 让位）；本场景驱动的是**移动真插件**，在双链进程里只能用「桌面侧实现」，而桌面网络授权在无头环境（无 AppHandle 弹窗）一律拒绝 → 会话 HTTP 面不可用。
    需要两进程夹具（桌面侧独立进程起服务器 + 移动侧进程跑真插件）才能真正跨端；移动侧协议契约已在 `wasmnest/wasmnest-mobile/src-tauri/tests/terminal_plugin_pipe_e2e.rs`（真插件 + 假桌面端点 + 真窄转发）钉住。"]
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

    desktop_ctx::init_app_context().await;
    let (port, handle, server_task) = desktop_ctx::start_server().await;
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

    // ==================== C-001 启动真实会话（移动插件 HTTP 面 → 桌面真 PTY） ====================
    let config_id = desktop_ctx::seed_shell_config("cross-end-terminal").await;
    let start = world
        .invoke(
            "bedcode.start-session",
            serde_json::json!({ "configId": config_id, "cols": 120, "rows": 40 }),
        )
        .await
        .expect("C-001 start-session 命令必须可调用");
    assert_eq!(start["code"], 0, "C-001 启动会话业务码应为 0：{start}");
    let session_id = start["data"]["sessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("C-001 响应缺 sessionId：{start}"))
        .to_string();
    assert!(!session_id.is_empty(), "C-001 sessionId 不得为空");

    // ==================== C-002 预热只建连：页面通道零字节 ====================
    // 页面通道先就位（真机是 markPageEntered 先登记、页面挂载后再 subscribe）
    world.page_subscribe(&session_id, mobile_ctx::output_channel(outputs.clone()));
    world
        .invoke(
            "bedcode.prewarm",
            serde_json::json!({ "sessionId": session_id }),
        )
        .await
        .expect("C-002 prewarm 必须可调用");
    // 桌面端点出现已认证客户端（证明连接真的建起来了，不是「什么都没发生所以零字节」）
    // 该 helper 内部等待到「有已认证客户端」为止（等不到即 panic，不静默返回）
    desktop_ctx::wait_plugin_ws_endpoint_authenticated(
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
            "bedcode.subscribe",
            serde_json::json!({ "sessionId": session_id }),
        )
        .await
        .expect("C-003 subscribe 必须可调用");
    world.wait_live(&session_id).await;
    // 先让 shell 打一条独占一行的 head marker（用于后面的重放语义判据）
    world
        .invoke(
            "bedcode.send-input",
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
            "bedcode.send-input",
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
            "bedcode.send-input",
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
        "C-005 序列行必须严格递增且每行恰好一次（缺口/重复/乱序都会在此暴露）；实际={seen:?}"
    );

    // ==================== C-006 重订阅 = fresh subscribe（整段重放） ====================
    let bytes_before_resub = outputs.bytes().len();
    world
        .invoke(
            "bedcode.subscribe",
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
            "bedcode.send-input",
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
    let stop = world
        .invoke(
            "bedcode.stop-session",
            serde_json::json!({ "sessionId": session_id }),
        )
        .await
        .expect("C-007 stop-session 必须可调用");
    assert_eq!(stop["code"], 0, "C-007 停止会话业务码应为 0：{stop}");
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
            "bedcode.remove",
            serde_json::json!({ "sessionId": session_id }),
        )
        .await
        .expect("remove 必须可调用");
    mobile_ctx::clear_identity();
    desktop_ctx::stop_server(handle, server_task).await;
    desktop_ctx::cleanup_temp_dirs();
}
