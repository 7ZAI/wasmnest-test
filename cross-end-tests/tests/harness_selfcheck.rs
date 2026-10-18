//! 装配自检：跨端互连的「台子」本身是否可信
//!
//! 场景测试红了的时候，第一个要排除的是**台子坏了**（服务器没起 / 移动端
//! 目标没接对 / 端口不是本进程的 / 停机没停干净），而不是协议有 bug。本文件
//! 把台子的四段接线各自钉一条强断言：
//!
//! 1. 移动端地址真源：`set_target` 后 `get_target` 回读一致
//! 2. 服务器端口已按 GUI bootstrap 同一装配面装好（票 07 的实测教训）
//! 3. 桌面真实插件在处理请求：移动端 `AuthHttpClient` 走真实 HTTP 拿到真实配对码
//! 4. 端口归属：停机后同一请求必须失败（证明刚才应答的确实是本进程服务器，
//!    也证明没有留下监听进程）
//! 5. 插件端点已登记：WS 插件端点存在（未注册端点 404 / 已注册端点可升级）
//!
//! 只跑一个 `#[tokio::test]`：桌面端 `AppContext` 与服务器端口注册表都是进程级
//! `OnceLock` 单例，场景子步骤必须串行（与 `pty_session_chain` 同口径）。

mod common;

use std::time::Duration;

use wasmnest_server_base::ports;
use common::desktop_ctx;
use common::mobile_ctx;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cross_end_rig_is_wired_end_to_end() {
    if tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::DEBUG)
        .try_init()
        .is_err()
    {
        tracing::debug!("tracing subscriber already initialized");
    }

    // ==================== 1. 桌面端无头服务 + 认证中心插件激活 ====================
    desktop_ctx::init_app_context().await;
    let (port, handle, server_task) = desktop_ctx::start_server().await;

    // ==================== 2. 装配面与 GUI bootstrap 同款（票 07） ====================
    // 判据是**注入后插件面真的认得中心已激活**，不是只查注册表非空：
    // 端口没装时网关 `ports::get() == None` 判 `PassThrough`，插件登记的宿主别名
    // 全 404，而端口占用 / WS 升级等不依赖端口的路径照常工作——症状伪装成协议 bug。
    // 这条断言把它降级成「台子接线」问题并点名根因。
    let server_ports = ports::get().expect(
        "跨端 rig 必须走宿主组合根的 `server::composition::install_server_ports()`：\
         缺端口时 HTTP 网关判 PassThrough，插件登记的宿主别名（/api/auth/* 等）全 404",
    );
    assert!(
        server_ports
            .plugin_invoker
            .is_activated(common::desktop_ctx::SESSION_PLUGIN_ID)
            .await,
        "注入的 PluginInvoker 端口必须认得已激活的认证中心（否则 rig 装了个空壳端口面）"
    );

    // ==================== 3. 移动端目标设备接线（HTTP / WS 的地址真源） ====================
    mobile_ctx::set_target(port).await;
    let target = wasmnest_mobile_lib::state::get_connection_manager()
        .get_target()
        .await
        .expect("target must round-trip through set_target");
    assert_eq!(target.address, "127.0.0.1", "移动端目标地址必须回读一致");
    assert_eq!(target.port, port, "移动端目标端口必须是本服务器实际监听端口");

    // ==================== 4. 移动端真实客户端 → 桌面真实插件 ====================
    let auth = wasmnest_mobile_lib::auth::http::AuthHttpClient::new();
    let base = mobile_ctx::base_url(port);
    let pairing = auth
        .request_pairing(&base, "rig-device", "Rig Check", "rig-fp")
        .await
        .expect("移动端真实客户端必须能从桌面真实插件拿到配对码");
    assert!(
        !pairing.pairing_code.is_empty(),
        "桌面真实插件签发的配对码不得为空（空码 = 恒真断言陷阱）"
    );
    assert!(
        pairing.expires_in > 0,
        "配对码有效期必须为正，got {}",
        pairing.expires_in
    );

    // ==================== 5. WS 插件端点已登记（认证中心激活期登记） ====================
    // 已注册端点可完成 WS 升级（认证/订阅在场景测试里做，这里只验「路由在」）
    let registered = format!("ws://127.0.0.1:{port}/ws/plugin/com.wasmnest.terminal-session/terminal");
    let upgrade = tokio_tungstenite::connect_async(&registered).await;
    assert!(
        upgrade.is_ok(),
        "认证中心已激活时终端端点必须可升级为 WS（端点未登记 = 激活未生效），got {upgrade:?}"
    );
    if let Ok((mut ws, _)) = upgrade {
        let _ = ws.close(None).await;
    }
    // 未注册端点必须 404（无 fallback）
    let unregistered = format!("ws://127.0.0.1:{port}/ws/plugin/com.wasmnest.terminal-session/no-such-endpoint");
    let err = tokio_tungstenite::connect_async(&unregistered).await;
    assert!(err.is_err(), "未注册端点必须拒绝（404 无升级），不得静默放行: {err:?}");

    // ==================== 6. 停机收尾 + 端口不再应答 ====================
    desktop_ctx::stop_server(handle, server_task).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let after_stop = auth
        .request_pairing(&base, "rig-device-2", "Rig Check 2", "rig-fp-2")
        .await;
    assert!(
        after_stop.is_err(),
        "服务器已优雅停机，同一地址不得再应答（说明刚才应答者确为本进程服务器，且无残留监听）"
    );

    mobile_ctx::clear_identity();
    desktop_ctx::cleanup_temp_dirs();
}

// ==================== 台子判据自身的纯函数自检 ====================
//
// `count_line_occurrences` 是「命令是否真的执行」的判据本身：判据坏了（恒真 / 卡死），
// 场景测试会集体伪装成通过，所以它的边界必须单独钉住。

/// 空 marker 会让 `find("")` 恒返回 0 且游标不前进 —— 未防御时本测试直接死循环卡死
#[test]
fn empty_marker_returns_zero_instead_of_hanging() {
    assert_eq!(mobile_ctx::count_line_occurrences("任意输出\n第二行\n", ""), 0);
    assert_eq!(mobile_ctx::count_line_occurrences("", ""), 0);
}

/// 独占一行的形态才算命令真实输出（PTY 回显会把输入行整行重发）
#[test]
fn only_whole_line_occurrences_are_counted() {
    let text = "prefix-MARKER-suffix\nMARKER\ntail MARKER here\n";
    assert_eq!(
        mobile_ctx::count_line_occurrences(text, "MARKER"),
        1,
        "只有独占一行的出现才算命令真实输出，其余（行内出现）必须不计"
    );
}

/// 边界：未出现返回 0；CRLF 行尾同样算行边界
#[test]
fn absent_marker_is_zero_and_crlf_counts_as_line_boundary() {
    assert_eq!(mobile_ctx::count_line_occurrences("no marker here\n", "MARKER"), 0);
    assert_eq!(mobile_ctx::count_line_occurrences("head\r\nMARKER\r\n", "MARKER"), 1);
}
