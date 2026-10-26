//! 场景 6（2026-10-15 重写）：插件停用 / 激活联动（跨端生命周期）
//!
//! 移动端终端链路（原宿主 `terminal_link`）已随票 12 下沉 wasm 插件；本文件对面是
//! **随包真产物**在无头宿主里运行的真实链路。覆盖的盲区不变：**桌面端插件生命周期
//! 对移动端连接的影响**——停用 = WS 资源回收 + 端点注销 → 移动端必须**显性**看到
//! 失败（不是静默空数据）；重新激活后已配对身份不丢、链路可重建。
//!
//! ## 行为契约
//!
//! | 契约 | 行为 | 场景 |
//! |---|---|---|
//! | L-001 | 建会话 + 终端链路 live（`get-state.subscribed=true`） | 正例（前置） |
//! | L-002 | 桌面停用插件 → WS 回收 → 移动链路回落（`subscribed=false`） | 正例 |
//! | L-003 | 端点注销 → 会话 HTTP 面**显性拒绝**（`code≠0`，不是空列表） | 反例（fail-visible ①） |
//! | L-004 | 重新激活 → 同一 token 仍有效、可建新会话并重新 live | 正例 |

mod common;

use std::sync::Arc;
use std::time::Duration;

use wasmnest_mobile_lib::auth::http::{AuthHttpClient, DeviceAuthContext};

use common::desktop_ctx::{self, SESSION_PLUGIN_ID};
use common::mobile_ctx;
use common::mobile_plugin::MobilePluginWorld;

#[ignore = "单进程 rig 装不下「两端宿主域实现」：`inventory` 是进程级注册，桌面与移动对 host-http/websocket 等端差异域各有一份实现，后装者必然冲突（现由 `HostModule::fallback` 让位）；本场景驱动的是**移动真插件**，在双链进程里只能用「桌面侧实现」，而桌面网络授权在无头环境（无 AppHandle 弹窗）一律拒绝 → 会话 HTTP 面不可用。
    需要两进程夹具（桌面侧独立进程起服务器 + 移动侧进程跑真插件）才能真正跨端；移动侧协议契约已在 `wasmnest/wasmnest-mobile/src-tauri/tests/terminal_plugin_pipe_e2e.rs`（真插件 + 假桌面端点 + 真窄转发）钉住。"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plugin_deactivation_is_visible_to_the_mobile_side() {
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
    let base = mobile_ctx::base_url(port);
    let address = format!("127.0.0.1:{port}");

    // ==================== 认证（真实配对 → 真实 token） ====================
    let auth = AuthHttpClient::new();
    let ctx = DeviceAuthContext {
        device_id: "crossend-lifecycle-device",
        device_name: "CrossEnd Lifecycle",
        fingerprint: "crossend-lifecycle-fp",
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

    let world = MobilePluginWorld::load_terminal_plugin(port, &token).await;
    let outputs = Arc::new(mobile_ctx::OutputRecorder::default());

    // ==================== L-001 前置：真会话 + live 链路 ====================
    let config_id = desktop_ctx::seed_shell_config("cross-end-lifecycle").await;
    let start = world
        .invoke(
            "bedcode.start-session",
            serde_json::json!({ "configId": config_id, "cols": 90, "rows": 25 }),
        )
        .await
        .expect("L-001 启动会话命令可调用");
    assert_eq!(start["code"], 0, "L-001 启动会话应成功：{start}");
    let session_id = start["data"]["sessionId"]
        .as_str()
        .expect("L-001 sessionId")
        .to_string();

    world.page_subscribe(&session_id, mobile_ctx::output_channel(outputs.clone()));
    world
        .invoke(
            "bedcode.subscribe",
            serde_json::json!({ "sessionId": session_id }),
        )
        .await
        .expect("L-001 subscribe 可调用");
    world.wait_live(&session_id).await;

    // ==================== L-002 桌面停用插件 → 移动链路回落 ====================
    let host_ctx = wasmnest_desktop_lib::system::app_context::AppContext::global();
    host_ctx
        .plugin_host()
        .deactivate_plugin(SESSION_PLUGIN_ID, false)
        .await
        .expect("L-002 停用认证中心插件");
    // WS 资源被回收：插件实例停用即断开该端点全部连接 → 移动端 `ws:close` → 链路不再 live
    world
        .wait_state(
            &session_id,
            "L-002 停用后链路必须回落（不再 subscribed）",
            |s| s["subscribed"] == false,
        )
        .await;

    // ==================== L-003 端点注销：显性失败而非空数据 ====================
    let denied = world
        .invoke("bedcode.list-sessions", serde_json::json!({}))
        .await
        .expect("L-003 列表命令可调用（拒绝走信封 code）");
    assert_ne!(
        denied["code"], 0,
        "L-003 停用后受保护端点必须显性失败（fail-visible ①，禁「查不到就返回空」）：{denied}"
    );
    assert!(
        denied.get("data").is_none() || denied["data"].is_null(),
        "L-003 拒绝不得带 data：{denied}"
    );

    // ==================== L-004 重新激活：身份不丢、链路可重建 ====================
    desktop_ctx::activate_session_center().await;
    // 同一 token 仍被中心认（认证记录与密钥环都不在停用清理范围内）
    let reauthed = auth
        .reauth(&base, ctx.device_id, ctx.fingerprint, ctx.uid_hash, &token)
        .await
        .expect("L-004 重新激活后同一 token 必须仍可验签（停用不得丢已配对身份）");
    assert!(!reauthed.token.is_empty(), "L-004 换发 token 不得为空");

    // 端点恢复可达且停用前的会话已随实例回收（显式停用的预期语义）
    let list = world
        .invoke("bedcode.list-sessions", serde_json::json!({}))
        .await
        .expect("L-004 重新激活后端点必须恢复可达");
    assert_eq!(list["code"], 0, "L-004 端点应恢复：{list}");
    assert!(
        !list["data"]
            .as_array()
            .is_some_and(|a| a.iter().any(|s| s["id"] == session_id)),
        "L-004 停用/激活后不得凭空多出会话（停用前的会话应随实例停用消失）：{list}"
    );

    // 恢复后能建新会话并重新进入 live（链路重建能力）
    let start2 = world
        .invoke(
            "bedcode.start-session",
            serde_json::json!({ "configId": config_id, "cols": 90, "rows": 25 }),
        )
        .await
        .expect("L-004 重新激活后启动命令可调用");
    assert_eq!(start2["code"], 0, "L-004 重新激活后建会话应成功：{start2}");
    let session2 = start2["data"]["sessionId"]
        .as_str()
        .expect("L-004 新会话 id")
        .to_string();
    world.page_subscribe(&session2, mobile_ctx::output_channel(outputs.clone()));
    world
        .invoke(
            "bedcode.subscribe",
            serde_json::json!({ "sessionId": session2 }),
        )
        .await
        .expect("L-004 重新订阅可调用");
    world.wait_live(&session2).await;

    // ==================== 收尾 ====================
    world
        .invoke("bedcode.remove", serde_json::json!({ "sessionId": session2 }))
        .await
        .expect("remove 可调用");
    tokio::time::sleep(Duration::from_millis(200)).await;
    mobile_ctx::clear_identity();
    desktop_ctx::stop_server(handle, server_task).await;
    desktop_ctx::cleanup_temp_dirs();
}
