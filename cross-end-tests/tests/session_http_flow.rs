//! 场景 3（2026-10-15 重写）：会话控制 HTTP 面（移动**真实插件** ↔ 桌面真实插件）
//!
//! 移动端的会话 HTTP 客户端（原 `wasmnest_mobile_lib::session::http`）已随票 13
//! 下沉到移动 wasm 插件（`bedcode.*` 命令，经 `host-http` + 宿主代注
//! JWT 打桌面 `/api/sessions*`）。本文件对面是**随包真产物**在无头宿主里真实运行：
//! 请求字节由插件生成、应答字节由桌面真实插件生成。
//!
//! 信封口径（前端消费形状，与退役前 `useHttpApi` 一致）：命令返回 `{code, message, data?}`；
//! 业务码非 0 时**不得**带有效 data（fail-visible，禁「查不到就返回空」）。
//!
//! ## 行为契约
//!
//! | 契约 | 行为 | 场景 |
//! |---|---|---|
//! | C-001 | `start-session {configId,cols,rows}` → `code=0` 且 `data.sessionId` 非空 | 正例 |
//! | C-002 | `list-sessions` 里查得到该会话且 `status=running` | 正例 |
//! | C-007 | `send-http-input` 带 `specialKey` 被桌面接受（`code=0`） | 正例 |
//! | C-005 | 未知 configId → `code=1002`（业务拒绝，带 message，无 data） | 反例 |
//! | C-006 | 不存在的会话：input/stop 严格 `1002`；remove **幂等** `code=0` 且无副作用 | 反例 + 不对称 |
//! | C-003 | `stop-session` → 轮询到终态 `stopped`（pty:exit 收尾） | 正例 |
//! | C-004 | `remove-session` → 列表不再含该会话 | 正例 |

mod common;

use common::desktop_ctx;
use common::mobile_ctx;
use common::mobile_plugin::MobilePluginWorld;

#[ignore = "单进程 rig 装不下「两端宿主域实现」：`inventory` 是进程级注册，桌面与移动对 host-http/websocket 等端差异域各有一份实现，后装者必然冲突（现由 `HostModule::fallback` 让位）；本场景驱动的是**移动真插件**，在双链进程里只能用「桌面侧实现」，而桌面网络授权在无头环境（无 AppHandle 弹窗）一律拒绝 → 会话 HTTP 面不可用。
    需要两进程夹具（桌面侧独立进程起服务器 + 移动侧进程跑真插件）才能真正跨端；移动侧协议契约已在 `wasmnest/wasmnest-mobile/src-tauri/tests/terminal_plugin_pipe_e2e.rs`（真插件 + 假桌面端点 + 真窄转发）钉住。"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn session_http_surface_matches_real_desktop() {
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
    use wasmnest_mobile_lib::auth::http::{AuthHttpClient, DeviceAuthContext};
    let auth = AuthHttpClient::new();
    let ctx = DeviceAuthContext {
        device_id: "crossend-session-device",
        device_name: "CrossEnd Sessions",
        fingerprint: "crossend-session-fp",
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
    let config_id = desktop_ctx::seed_shell_config("cross-end-sessions").await;

    // ==================== C-005 未知 configId → 1002（先跑反例，避免污染后续） ====================
    let bad = world
        .invoke(
            "bedcode.start-session",
            serde_json::json!({ "configId": "config-does-not-exist" }),
        )
        .await
        .expect("C-005 启动命令本身可调用（业务拒绝走信封 code）");
    assert_eq!(bad["code"], 1002, "C-005 未知 configId 应以 1002 拒绝：{bad}");
    assert!(
        bad["message"].as_str().is_some_and(|m| !m.is_empty()),
        "C-005 业务拒绝必须带 message（禁静默空数据）：{bad}"
    );
    assert!(
        bad.get("data").is_none() || bad["data"].is_null(),
        "C-005 拒绝不得带 data：{bad}"
    );

    // ==================== C-001 建会话 ====================
    let start = world
        .invoke(
            "bedcode.start-session",
            serde_json::json!({ "configId": config_id, "cols": 100, "rows": 30 }),
        )
        .await
        .expect("C-001 启动命令可调用");
    assert_eq!(start["code"], 0, "C-001 启动应成功：{start}");
    let session_id = start["data"]["sessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("C-001 响应缺 sessionId：{start}"))
        .to_string();
    assert!(!session_id.is_empty(), "C-001 sessionId 不得为空");

    // ==================== C-002 列表可查 ====================
    let list = world
        .invoke("bedcode.list-sessions", serde_json::json!({}))
        .await
        .expect("C-002 列表命令可调用");
    assert_eq!(list["code"], 0, "C-002 列表应成功：{list}");
    let entries = list["data"].as_array().expect("C-002 data 应为数组");
    let entry = entries
        .iter()
        .find(|s| s["id"] == session_id)
        .unwrap_or_else(|| panic!("C-002 会话应出现在列表，list={list}"));
    assert_eq!(entry["status"], "running", "C-002 新建会话应为 running");

    // ==================== C-007 特殊键写入被桌面接受 ====================
    let key_input = world
        .invoke(
            "bedcode.send-http-input",
            serde_json::json!({ "sessionId": session_id, "data": "", "specialKey": "ctrl+c" }),
        )
        .await
        .expect("C-007 带 specialKey 的 HTTP 输入命令可调用");
    assert_eq!(key_input["code"], 0, "C-007 特殊键写入应被桌面接受：{key_input}");

    // ==================== C-006 不存在的会话：stop/input 严格、remove 幂等 ====================
    // 这条**不对称**契约两端必须一致（移动端重试逻辑按它写）
    let ghost = "00000000-0000-0000-0000-0000000000ff";
    let before = world
        .invoke("bedcode.list-sessions", serde_json::json!({}))
        .await
        .expect("C-006 取基线列表");
    for (cmd, args) in [
        (
            "bedcode.send-http-input",
            serde_json::json!({ "sessionId": ghost, "data": "x" }),
        ),
        (
            "bedcode.stop-session",
            serde_json::json!({ "sessionId": ghost }),
        ),
    ] {
        let denied = world.invoke(cmd, args).await.expect("C-006 命令可调用");
        assert_eq!(denied["code"], 1002, "C-006 {cmd} 对不存在会话必须严格拒绝：{denied}");
    }
    let removed = world
        .invoke(
            "bedcode.remove-session",
            serde_json::json!({ "sessionId": ghost }),
        )
        .await
        .expect("C-006 remove 命令可调用");
    assert_eq!(removed["code"], 0, "C-006 移除不存在的会话必须幂等成功：{removed}");
    let after_ghost = world
        .invoke("bedcode.list-sessions", serde_json::json!({}))
        .await
        .expect("C-006 幂等移除后读列表");
    assert_eq!(
        after_ghost["data"].as_array().map(Vec::len),
        before["data"].as_array().map(Vec::len),
        "C-006 幂等移除不得增删会话（before={before} after={after_ghost}）"
    );

    // ==================== C-003 停止 → 终态 ====================
    let stopped = world
        .invoke(
            "bedcode.stop-session",
            serde_json::json!({ "sessionId": session_id }),
        )
        .await
        .expect("C-003 停止命令可调用");
    assert_eq!(stopped["code"], 0, "C-003 停止应被接受：{stopped}");
    // 终态由 pty:exit 异步收尾：轮询列表直到 stopped/error（不能只看「不再 running」：
    // stopping 是过渡态，命中它会在 PTY 收尾前断言）
    let mut terminal_status = None;
    for _ in 0..120 {
        let list = world
            .invoke("bedcode.list-sessions", serde_json::json!({}))
            .await
            .expect("C-003 终态轮询读列表");
        if let Some(entry) = list["data"]
            .as_array()
            .and_then(|a| a.iter().find(|s| s["id"] == session_id))
        {
            let status = entry["status"].as_str().unwrap_or("").to_string();
            if status == "stopped" || status == "error" {
                terminal_status = Some(status);
                break;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(
        terminal_status.as_deref(),
        Some("stopped"),
        "C-003 停止后会话终态必须是 stopped（pty:exit 收尾）"
    );

    // ==================== C-004 移除 → 列表不再含 ====================
    let removed = world
        .invoke(
            "bedcode.remove-session",
            serde_json::json!({ "sessionId": session_id }),
        )
        .await
        .expect("C-004 移除命令可调用");
    assert_eq!(removed["code"], 0, "C-004 移除应成功：{removed}");
    let after = world
        .invoke("bedcode.list-sessions", serde_json::json!({}))
        .await
        .expect("C-004 移除后读列表");
    assert!(
        !after["data"]
            .as_array()
            .is_some_and(|a| a.iter().any(|s| s["id"] == session_id)),
        "C-004 移除后会话不得仍在列表：{after}"
    );

    // ==================== 收尾 ====================
    mobile_ctx::clear_identity();
    desktop_ctx::stop_server(handle, server_task).await;
    desktop_ctx::cleanup_temp_dirs();
}
