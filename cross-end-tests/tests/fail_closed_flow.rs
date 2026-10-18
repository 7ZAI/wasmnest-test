//! 场景 5（2026-10-15 重写）：fail-closed 失败路径（无中心在册 / 非法凭证）
//!
//! 移动端的会话 HTTP 客户端与终端链路已随票 12/13 下沉 wasm 插件：本文件对面是
//! **随包真产物**在无头宿主里运行（HTTP 出站 + WS 链路全是真实现），桌面端应答
//! 全部来自真实插件。fail-closed 语义（ADR 0031：无中心 / 不可用 / 拒绝 → 一律拒绝，
//! 无降级）在真实互连下的三条可观测契约：
//!
//! - 一律显性拒绝（`code≠0` / Err），不是「查不到就当没数据」的静默降级；
//! - 放行必须**同时**满足「中心在册 + 凭证有效」；
//! - WS 面同一闸门：无效凭证下链路重连但**永不 live**、零输出字节。
//!
//! 关于 `deny_kind` 三态（no_center / unavailable / policy）：它是宿主日志的结构化
//! 字段、不是 wire 字段（把拒绝原因分类暴露给客户端会泄露部署信息），其覆盖在宿主
//! 侧 `utils/auth/auth_center` 单测里，不在本工程重复造 mock。
//!
//! ## 行为契约
//!
//! | 契约 | 行为 | 场景 |
//! |---|---|---|
//! | F-001 | 无中心在册：带 Bearer 访问 JWT 档位端点 → 显性拒绝（401），无数据 | 反例 |
//! | F-002 | 无中心在册时认证链端点同样拒绝（不存在无认证的自助配对入口） | 反例 |
//! | F-003 | 中心在册 + 真凭证 → 放行且数据形状正确 | 正例（对照） |
//! | F-004 | 中心在册 + 伪造凭证 → 仍 401 | 反例 |
//! | F-005 | WS 面同闸门：伪造凭证下链路永不 live、页面通道零字节 | 反例 |

mod common;

use std::sync::Arc;
use std::time::Duration;

use wasmnest_mobile_lib::auth::http::{AuthHttpClient, DeviceAuthContext};
use wasmnest_mobile_lib::system::error::AppError;

use common::desktop_ctx;
use common::mobile_ctx;
use common::mobile_plugin::MobilePluginWorld;

/// 断言失败落在「传输 / 基础设施」档（HTTP 401 / 404）而非「桌面业务拒绝」档
fn assert_transport_denial(err: AppError, status: u16, what: &str) {
    match err {
        AppError::Internal(msg) => assert!(
            msg.contains(&status.to_string()),
            "{what}：应报 HTTP {status}（显性拒绝），got: {msg}"
        ),
        other => panic!(
            "{what}：跨端契约要求显性传输级拒绝，实际变体 {other:?}（若是 AppError::Auth \
             说明请求根本没被拦在网关层，认证边界被绕过）"
        ),
    }
}

/// 会话 HTTP 面的拒绝必须走信封 `code`（插件契约）且**不带 data**
fn assert_envelope_denial(frame: &serde_json::Value, what: &str) {
    assert_ne!(frame["code"], 0, "{what}：必须显性拒绝：{frame}");
    assert!(
        frame.get("data").is_none() || frame["data"].is_null(),
        "{what}：拒绝不得带 data（禁「查不到就返回空」静默降级）：{frame}"
    );
}

#[ignore = "单进程 rig 装不下「两端宿主域实现」：`inventory` 是进程级注册，桌面与移动对 host-http/websocket 等端差异域各有一份实现，后装者必然冲突（现由 `HostModule::fallback` 让位）；本场景驱动的是**移动真插件**，在双链进程里只能用「桌面侧实现」，而桌面网络授权在无头环境（无 AppHandle 弹窗）一律拒绝 → 会话 HTTP 面不可用。
    需要两进程夹具（桌面侧独立进程起服务器 + 移动侧进程跑真插件）才能真正跨端；移动侧协议契约已在 `wasmnest/wasmnest-mobile/src-tauri/tests/terminal_plugin_pipe_e2e.rs`（真插件 + 假桌面端点 + 真窄转发）钉住。"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_center_and_bad_credential_are_denied_fail_closed() {
    if tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::DEBUG)
        .try_init()
        .is_err()
    {
        tracing::debug!("tracing subscriber already initialized");
    }

    // ==================== 阶段一：认证中心【未激活】 ====================
    desktop_ctx::init_app_context_without_center().await;
    let (port, handle, server_task) = desktop_ctx::start_server().await;
    mobile_ctx::set_target(port).await;
    let base = mobile_ctx::base_url(port);

    let auth = AuthHttpClient::new();
    let ctx = DeviceAuthContext {
        device_id: "crossend-failclosed-device",
        device_name: "CrossEnd FailClosed",
        fingerprint: "crossend-failclosed-fp",
        uid_hash: None,
    };

    // F-001 保护端点：即便带了形似凭证的 Bearer，无中心在册一律 401
    mobile_ctx::remember_token("not-a-real-token");
    let world_no_center = MobilePluginWorld::load_terminal_plugin(port, "not-a-real-token").await;
    let denied = world_no_center
        .invoke("terminal-session.list-sessions", serde_json::json!({}))
        .await
        .expect("F-001 列表命令可调用（拒绝走信封 code）");
    assert_envelope_denial(&denied, "F-001 无中心在册");

    // F-002 认证链自身端点也被封死：不存在「无中心即可自助配对」的引导入口
    // （免凭证档位由插件在 activate 期登记；未激活时网关先于路由解析就拒 → 401）
    let no_bootstrap = auth
        .request_pairing(&base, ctx.device_id, ctx.device_name, ctx.fingerprint)
        .await
        .expect_err("F-002 无中心在册时认证链端点不得开放（否则等于给出无认证的自助配对入口）");
    assert_transport_denial(no_bootstrap, 401, "F-002 无中心时认证链端点");

    // ==================== 阶段二：激活中心 → F-004 伪造凭证 ====================
    desktop_ctx::activate_session_center().await;

    // F-004 中心在册但凭证无效：仍是一律拒绝（无「验签失败就放行」分支）
    mobile_ctx::remember_token("forged.jwt.token");
    let world_forged = MobilePluginWorld::load_terminal_plugin(port, "forged.jwt.token").await;
    let forged = world_forged
        .invoke("terminal-session.list-sessions", serde_json::json!({}))
        .await
        .expect("F-004 列表命令可调用");
    assert_envelope_denial(&forged, "F-004 伪造凭证");

    // ==================== F-003 正例对照：真凭证 → 放行 ====================
    // （放在最后跑：唯一的放行路径，用来证明前面的 401 不是「总是拒绝」）
    let address = format!("127.0.0.1:{port}");
    let pairing = auth
        .request_pairing(&base, ctx.device_id, ctx.device_name, ctx.fingerprint)
        .await
        .expect("F-003 中心激活后配对端点必须可达");
    let token = auth
        .verify_pairing_code(&base, ctx, &pairing.pairing_code, &address)
        .await
        .expect("F-003 配对换 token")
        .token;
    mobile_ctx::remember_token(&token);
    let world_real = MobilePluginWorld::load_terminal_plugin(port, &token).await;
    let allowed = world_real
        .invoke("terminal-session.list-sessions", serde_json::json!({}))
        .await
        .expect("F-003 真凭证下命令可调用");
    assert_eq!(allowed["code"], 0, "F-003 真凭证必须放行：{allowed}");
    assert!(
        allowed["data"].as_array().is_some_and(|a| a.is_empty()),
        "F-003 刚激活即无会话：data 应为空数组（空数组 = 放行了但无数据）：{allowed}"
    );

    // ==================== F-005 WS 面同一闸门 ====================
    // 伪造凭证：链路会带退避反复重连（证明它确实在尝试），但**永不 live**、零字节
    let outputs = Arc::new(mobile_ctx::OutputRecorder::default());
    let ghost = "crossend-failclosed-ghost-session";
    world_forged.page_subscribe(ghost, mobile_ctx::output_channel(outputs.clone()));
    world_forged
        .invoke("terminal-session.subscribe", serde_json::json!({ "sessionId": ghost }))
        .await
        .expect("F-005 subscribe 命令可调用（拒绝发生在 WS 认证闸门）");
    // 给足多轮重连的时间窗：任一轮都不许通过认证闸门
    tokio::time::sleep(Duration::from_secs(4)).await;
    let state = world_forged
        .invoke("terminal-session.get-state", serde_json::json!({ "sessionId": ghost }))
        .await
        .expect("F-005 get-state 可调用");
    assert_ne!(
        state["phase"], "live",
        "F-005 无效凭证下链路绝不能进入 live（宿主 close 4001 必须真的挡住）：{state}"
    );
    assert_ne!(state["subscribed"], true, "F-005 无效凭证下不得订阅成功：{state}");
    assert!(
        outputs.bytes().is_empty(),
        "F-005 无效凭证下不得有任何终端输出字节到达移动端页面通道，got {} 字节",
        outputs.bytes().len()
    );

    // ==================== 收尾 ====================
    mobile_ctx::clear_identity();
    desktop_ctx::stop_server(handle, server_task).await;
    desktop_ctx::cleanup_temp_dirs();
}
