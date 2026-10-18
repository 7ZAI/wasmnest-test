//! 场景 10：HTTP 代理面 `http_request`（Egress L1 + JWT 注入 + 链路加密信封）
//!
//! 覆盖的盲区：**移动端前端的唯一 HTTP 出路**。移动端既有测试对面是假桌面服务器，
//! 桌面端既有测试对面是 reqwest / 无代理——移动端 Rust 侧 `execute_proxy`
//! （`commands/http_proxy.rs`）这一层此前**零跨端覆盖**，而它同时承担三件事：
//! Egress 门禁、JWT 注入、链路加密信封。任一件坏了，症状都是「移动端 UI 拿不到数据」，
//! 且**不会报错**（前端只是空列表）。
//!
//! 加密面尤其关键：它要求「pin 必须来自真实认证链」（认证响应里的
//! `kdPublicB64` = 桌面 Kd 公钥）。手工造假 pin 会让桌面解不开 → 假红，
//! 所以本文件先做 headless 链路加密装配（建身份），再走**真实配对**取 pin。
//!
//! ## 「加密真的发生」的判据（防「明文 200」恒真）
//!
//! 只断言 200 + 正确 JSON 是**不够**的——明文直连也会 200。判据用桌面侧自己的
//! 加密计数器（`MetricsCollector`，只读快照）+ 白名单对照：
//!
//! - 有 body 的加密往返：入站解封 +1、出站加密 +1 ⇒ `encrypted_frames` **+2**；
//! - GET（无请求体）只有协商头：仅出站 +1（入站空 body 不计数，否则高估吞吐）；
//! - `/api/auth/*` 白名单**不加密**：请求与响应都是明文 ⇒ 计数 **+0**，且响应是
//!   可解析的真实信封（若误加密，移动端不解密 → 拿到密文字符串）。
//!
//! ## 行为契约（unit-test-discipline G1：每条有来源）
//!
//! | 契约 | 来源（代码证据） | 行为 | 场景 |
//! |---|---|---|---|
//! | P-001a | `http_proxy.rs:180` L1：`is_desktop_target` | 未声明 host:port 的 `kind="desktop"` → `AppError::Egress`（`EXTERNAL_URL_NOT_DECLARED`），**不发请求** | 反例 |
//! | P-001b | `http_proxy.rs` `DESKTOP_REQUIRED_SCHEME` | `https://` + `kind="desktop"` → 拒（局域网明文端口，不得借 desktop 逃逸外网） | 反例 |
//! | P-001c | `http_proxy.rs` external 分支 + `app=None` | 未声明的 `kind="external"` → L3 需授权而 `app=None` → fail-closed 拒（无 UI 不得默认放行） | 反例 |
//! | P-001d | 同上 | 声明目标后 `kind="desktop"` → 真往返 200 | 正例 |
//! | P-002a | `should_inject_jwt` | 有效全局 token + `/api/sessions` → 200 + 真实列表 | 正例 |
//! | P-002b | 同上 | **无** token + `/api/sessions` → 401（没注入 → 网关拒绝） | 反例 |
//! | P-002c | 同上 | 伪造 token + `/api/sessions` → 401（注入了但被中心拒） | 反例 |
//! | P-003a | `state::is_http_encryption_active` = 主开关 ∧ HTTP 子开关 ∧ 已 pin | pin 来自真实认证链；开关打开后经代理发**加密信封** POST → 桌面真实解密 → 响应密文被移动端真实解密 | 正例（核心） |
//! | P-003b | `should_encrypt` 白名单 `/api/auth/*` | 加密开启时 auth 路径仍走**明文**且真实成功（计数 +0 + 响应可解析） | 正例 + 反例（防「一刀切加密」） |
//! | P-004a | `useHttpApi.ts httpResizeSession`（**移动端无 Rust 客户端**） | `POST /api/sessions/{id}/resize` 经代理真实往返，`{cols,rows,force}` 被桌面插件接受 | 正例 |
//! | P-004b | `useHttpApi.ts` 配置/快捷指令/任务队列面 | `/api/configs`、`/api/quick-actions`、`/api/plugin/<id>/task-queue/list` 真实往返，响应信封 `{code,message,data}` | 正例 |
//! | P-005 | `http_cancel`（oneshot + `tokio::select!`） | 在途请求被取消 → `REQUEST_CANCELED`；未知 `request_id` 幂等成功 | 正例 |
//! | P-006 | spec §6 决策模型第 3 条（对端按协商头参与）与实现语义段（主开关关闭则过滤器不注册）**自相矛盾** | 两端开关不一致（移动端开、桌面端关）时：带协商头的请求（POST 与 GET）一律**显性 4xx + 点名 `trafficEncryption`**，不再是 `1002 configId required` 那种误导性业务码；两端都关时明文照常可用（opt-in 基线） | 正例 + 反例 |

mod common;

use std::time::Duration;

use wasmnest_mobile_lib::commands::egress::egress_declare_desktop_target;
use wasmnest_mobile_lib::commands::http_proxy::{execute_proxy, http_cancel, HttpProxyRequest};
use wasmnest_mobile_lib::egress::ERROR_URL_NOT_DECLARED;
use wasmnest_mobile_lib::system::error::AppError;

use common::desktop_ctx;
use common::mobile_ctx;

/// 播种的会话配置名（`/api/configs` 与会话记录名都按它断言）
const CONFIG_NAME: &str = "cross-end-proxy";

/// 造一条 `kind="desktop"` 的代理请求
fn desktop_request(id: &str, method: &str, url: &str, body: Option<String>) -> HttpProxyRequest {
    HttpProxyRequest {
        request_id: id.to_string(),
        method: method.to_string(),
        url: url.to_string(),
        headers: Default::default(),
        body,
        timeout_ms: Some(15_000),
        kind: Some("desktop".to_string()),
    }
}

/// 断言 Egress 门禁拒绝（显性 `AppError::Egress` + 指定错误码）
fn assert_egress(err: AppError, code: &str, what: &str) {
    match err {
        AppError::Egress(msg) => assert!(msg.contains(code), "{what}：Egress 拒绝应点名错误码 {code}，got: {msg}"),
        other => panic!("{what}：应映射为 AppError::Egress（门禁拒绝），got {other:?}"),
    }
}

/// 解析桌面端业务信封（`{code,message,data}`）并断言 `code == 0`
fn envelope_ok(body: &str, what: &str) -> serde_json::Value {
    let v: serde_json::Value = serde_json::from_str(body).unwrap_or_else(|e| {
        panic!("{what}：响应体应可解析为 JSON 信封（若拿到密文说明加解密没对上），got {body:?}（{e}）")
    });
    assert_eq!(v["code"], 0, "{what}：业务信封 code 应为 0，got {v:#?}");
    v
}

/// panic 路径也要清临时目录（实测失败一轮会在 /tmp 留残留）
struct TempDirGuard;

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        desktop_ctx::cleanup_temp_dirs();
    }
}

/// 本机**非环回** IPv4（链路加密用例的前提）
///
/// 桌面端链路加密过滤器对**环回对端显式豁免**（`link_crypto::is_exempt`：hook 脚本 /
/// 本机工具直连 REST 不加密）。而 rig 的服务器与客户端同进程同主机，环回连过去必然
/// 命中豁免 ⇒ 加密分支永不执行，P-003 会退化成「明文 200」恒真。
///
/// 解法：连本机自己的 LAN IP（内核 `ip route get <本机 IP>` = `local … src <同 IP>`）
/// ——服务器看到的对端地址就不是环回，加密分支真实生效。取不到非环回 IPv4 时
/// **显性失败**（不静默 skip：否则加密面会被当成已验证）。
fn local_non_loopback_ipv4() -> std::net::Ipv4Addr {
    let out = std::process::Command::new("ip")
        .args(["-4", "-o", "addr", "show", "scope", "global"])
        .output()
        .unwrap_or_else(|e| panic!("执行 `ip -4 -o addr show scope global` 失败（无 ip 工具？）：{e}"));
    let stdout = String::from_utf8_lossy(&out.stdout);
    stdout
        .lines()
        .filter_map(|line| line.split_whitespace().nth(3))
        .filter_map(|cidr| cidr.split('/').next())
        .filter_map(|addr| addr.parse::<std::net::Ipv4Addr>().ok())
        .find(|ip| !ip.is_loopback())
        .unwrap_or_else(|| {
            panic!(
                "本机没有非环回 IPv4，链路加密用例无法验证（桌面端过滤器对环回对端豁免，\
                 且不允许静默 skip）。`ip -4 -o addr show scope global` 输出：{stdout:?}"
            )
        })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mobile_http_proxy_surface_matches_real_desktop() {
    if tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::DEBUG)
        .try_init()
        .is_err()
    {
        tracing::debug!("tracing subscriber already initialized");
    }

    desktop_ctx::init_app_context().await;
    let _temp_dirs = TempDirGuard;
    // 链路加密装配必须早于配对：认证响应里的 kdPublicB64 就是这条身份公钥
    desktop_ctx::enable_link_crypto_http().await;
    let (port, handle, server_task) = desktop_ctx::start_server().await;
    // 全程走本机非环回地址：桌面端加密过滤器豁免环回对端（见 local_non_loopback_ipv4）
    let host_ip = local_non_loopback_ipv4();
    let host = host_ip.to_string();
    mobile_ctx::set_target_at(&host, port).await;
    let base = format!("http://{host}:{port}");
    let address = format!("{host}:{port}");

    // ==================== P-001 Egress L1 门禁（反例组先行） ====================
    // 未声明的目标：L1 必须在**发请求之前**拒（这里没有任何服务在监听，
    // 若门禁失效，报错形态会从 Egress 变成传输失败——正是要区分的两种失败）
    let undeclared_port = desktop_ctx::pick_free_port();
    assert_egress(
        execute_proxy(
            desktop_request(
                "p001a",
                "GET",
                &format!("http://{host}:{undeclared_port}/api/sessions"),
                None,
            ),
            None,
        )
        .await
        .expect_err("P-001a 未声明的桌面目标必须被 L1 拒绝"),
        ERROR_URL_NOT_DECLARED,
        "P-001a 未声明目标",
    );

    // https + kind=desktop：桌面端 HTTP 是局域网明文单端口，https 必须拒
    assert_egress(
        execute_proxy(
            desktop_request("p001b", "GET", "https://example.com/api/sessions", None),
            None,
        )
        .await
        .expect_err("P-001b desktop kind 不得放行 https"),
        ERROR_URL_NOT_DECLARED,
        "P-001b https 逃逸",
    );

    // external（缺省 kind）+ 无 UI：L3 需授权 → app=None 必须 fail-closed
    let external = HttpProxyRequest {
        request_id: "p001c".to_string(),
        method: "GET".to_string(),
        url: "https://example.com/some/resource".to_string(),
        headers: Default::default(),
        body: None,
        timeout_ms: Some(5_000),
        kind: None,
    };
    assert_egress(
        execute_proxy(external, None)
            .await
            .expect_err("P-001c 无 UI 时 L3 必须 fail-closed（不得默认放行外网）"),
        wasmnest_mobile_lib::egress::ERROR_URL_DENIED,
        "P-001c external + app=None",
    );

    // ==================== 声明桌面目标（生产入口：前端 setApiBaseUrl 时 invoke） ====================
    egress_declare_desktop_target(host.clone(), port).expect("声明桌面目标");

    // ==================== 认证（真实配对 → pin 随认证响应落地） ====================
    let auth = wasmnest_mobile_lib::auth::http::AuthHttpClient::new();
    let ctx = wasmnest_mobile_lib::auth::http::DeviceAuthContext {
        device_id: "crossend-proxy-device",
        device_name: "CrossEnd Proxy",
        fingerprint: "crossend-proxy-fp",
        uid_hash: None,
    };
    let pairing = auth
        .request_pairing(&base, ctx.device_id, ctx.device_name, ctx.fingerprint)
        .await
        .expect("P-002 配对码签发");
    let token = auth
        .verify_pairing_code(&base, ctx, &pairing.pairing_code, &address)
        .await
        .expect("P-002 配对码换 token")
        .token;
    // 生产收尾走 AuthManager：全局 token + pin + AuthSuccess 都由 apply_auth_success 落地
    wasmnest_mobile_lib::state::get_auth_manager()
        .authenticate_with_token(&token)
        .await
        .expect("P-003a 真实 reauth（认证中心自签自验）");

    let pinned = wasmnest_mobile_lib::state::get_link_crypto_context().kd_public_b64;
    assert!(
        pinned.as_deref().is_some_and(|p| !p.is_empty()),
        "P-003a pin 必须由真实认证链落地（认证响应的 kdPublicB64 = 桌面 Kd 公钥）；\
         缺失说明 headless 链路加密装配没赶上配对"
    );

    // ==================== P-002 JWT 注入 ====================
    let sessions = desktop_request("p002a", "GET", &format!("{base}/api/sessions"), None);
    let ok = execute_proxy(sessions, None).await.expect("P-002a 有效 token 必须放行");
    assert_eq!(ok.status, 200, "P-002a /api/sessions 应 200");
    let listed = envelope_ok(&ok.body_text, "P-002a /api/sessions");
    assert!(
        listed["data"]["sessions"].is_array(),
        "P-002a data.sessions 应为数组，got {}",
        listed["data"]
    );

    // 无 token → 没注入 → 网关 401（与「注入了但被拒」是两种不同的失败）
    //
    // 注意口径：**HTTP 级失败经代理面是 `Ok(HttpProxyResponse{status})`，不是 Err**
    // （Err 只承载传输 / Egress 门禁失败）——前端按 `code != 0` 归一化。
    mobile_ctx::clear_identity();
    let no_token = execute_proxy(
        desktop_request("p002b", "GET", &format!("{base}/api/sessions"), None),
        None,
    )
    .await
    .expect("P-002b 无 token 的请求应走完传输（拒绝发生在网关层，不是连接失败）");
    assert_eq!(
        no_token.status, 401,
        "P-002b 无 token 时不得有任何注入 → 网关必须 401（防“空 token 也放行”），body={}",
        no_token.body_text
    );
    let denied_code: serde_json::Value = serde_json::from_str::<serde_json::Value>(&no_token.body_text)
        .map(|v| v["code"].clone())
        .unwrap_or(serde_json::Value::Null);
    assert_eq!(
        denied_code,
        serde_json::json!(1007),
        "P-002b 401 应带桌面显式业务码 1007（而非空体），body={}",
        no_token.body_text
    );

    // 伪造 token → 注入了但认证中心拒绝（同样 401，业务码不同路径）
    mobile_ctx::remember_token("forged.jwt.token");
    let forged = execute_proxy(
        desktop_request("p002c", "GET", &format!("{base}/api/sessions"), None),
        None,
    )
    .await
    .expect("P-002c 伪造 token 的请求应走完传输（拒绝发生在认证中心）");
    assert_eq!(
        forged.status, 401,
        "P-002c 伪造 token 必须被认证中心拒（无降级放行），body={}",
        forged.body_text
    );

    // 恢复真实身份（P-003 起的加密用例需要合法 JWT）
    wasmnest_mobile_lib::state::get_auth_manager()
        .authenticate_with_token(&token)
        .await
        .expect("恢复真实身份");

    // ==================== 打开移动端加密（只翻主开关，pin 保持真实链落地值） ====================
    let mut crypto = wasmnest_mobile_lib::state::get_link_crypto_context();
    crypto.enabled = true;
    crypto.encrypt_http = true;
    wasmnest_mobile_lib::state::set_link_crypto_context(crypto);
    assert!(
        wasmnest_mobile_lib::state::is_http_encryption_active(),
        "P-003a 加密开关打开且已 pin 后应判定为加密激活"
    );

    let config_id = desktop_ctx::seed_shell_config(CONFIG_NAME).await;

    // ==================== P-003a 加密信封：有 body 的 POST 真实加解密互连 ====================
    let (before_enc, before_fail, _) = desktop_ctx::link_crypto_counters();
    let started = execute_proxy(
        desktop_request(
            "p003-start",
            "POST",
            &format!("{base}/api/sessions/start"),
            Some(serde_json::json!({ "configId": config_id, "cols": 100, "rows": 30 }).to_string()),
        ),
        None,
    )
    .await
    .expect("P-003a 加密信封 POST 必须往返成功");
    assert_eq!(started.status, 200, "P-003a /api/sessions/start 应 200");
    let start_env = envelope_ok(&started.body_text, "P-003a /api/sessions/start");
    let session_id = start_env["data"]["sessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("P-003a 响应应带 sessionId，got {start_env:#?}"))
        .to_string();
    assert!(!session_id.is_empty(), "P-003a sessionId 不得为空");
    let (after_enc, after_fail, _) = desktop_ctx::link_crypto_counters();
    assert_eq!(
        after_fail, before_fail,
        "P-003a 桌面端不得记录解密失败（信封与 AAD 路由绑定必须对得上）"
    );
    assert!(
        after_enc >= before_enc + 2,
        "P-003a 有 body 的加密往返应至少 +2 加密帧（入站解封 + 出站加密），\
         实测 {before_enc} → {after_enc}；若为 0 说明请求根本没走加密（明文 200 恒真陷阱）"
    );

    // ==================== P-003b 白名单：auth 路径不加密 ====================
    let (pre_auth_enc, _, _) = desktop_ctx::link_crypto_counters();
    let auth_via_proxy = execute_proxy(
        desktop_request(
            "p003b",
            "POST",
            &format!("{base}/api/auth/pairing"),
            Some(
                serde_json::json!({
                    "deviceId": "crossend-proxy-device-2",
                    "deviceName": "CrossEnd Proxy 2",
                    "fingerprint": "crossend-proxy-fp-2",
                })
                .to_string(),
            ),
        ),
        None,
    )
    .await
    .expect("P-003b 加密开启时 auth 路径仍必须可用（明文，不一刀切加密）");
    assert_eq!(auth_via_proxy.status, 200, "P-003b /api/auth/pairing 应 200");
    let pairing_env = envelope_ok(&auth_via_proxy.body_text, "P-003b /api/auth/pairing");
    assert!(
        pairing_env["data"]["pairingCode"]
            .as_str()
            .is_some_and(|c| !c.is_empty()),
        "P-003b 响应应是可解析的真实信封（若被误加密，移动端不会解密 → 拿不到配对码），got {pairing_env:#?}"
    );
    let (post_auth_enc, _, _) = desktop_ctx::link_crypto_counters();
    assert_eq!(
        post_auth_enc, pre_auth_enc,
        "P-003b /api/auth/* 白名单必须明文（请求与响应都不该计入加密帧）"
    );

    // ==================== P-004 生产端点抽样（全部经代理面） ====================
    // ---- P-004a resize：移动端**无 Rust 客户端**的端点，只有代理面能跑 ----
    let (pre_resize_enc, _, _) = desktop_ctx::link_crypto_counters();
    let resized = execute_proxy(
        desktop_request(
            "p004a",
            "POST",
            &format!("{base}/api/sessions/{session_id}/resize"),
            Some(serde_json::json!({ "cols": 120, "rows": 40, "force": false }).to_string()),
        ),
        None,
    )
    .await
    .expect("P-004a resize 必须往返成功");
    assert_eq!(resized.status, 200, "P-004a resize 应 200");
    envelope_ok(&resized.body_text, "P-004a resize");
    let (post_resize_enc, _, _) = desktop_ctx::link_crypto_counters();
    assert!(
        post_resize_enc >= pre_resize_enc + 2,
        "P-004a resize 是带 body 的加密往返，应至少 +2 加密帧（{pre_resize_enc} → {post_resize_enc}）"
    );

    // ---- P-004b GET 面：configs / quick-actions / task-queue ----
    let configs = execute_proxy(
        desktop_request("p004b1", "GET", &format!("{base}/api/configs"), None),
        None,
    )
    .await
    .expect("P-004b /api/configs");
    assert_eq!(configs.status, 200, "P-004b /api/configs 应 200");
    let configs_env = envelope_ok(&configs.body_text, "P-004b /api/configs");
    let seeded = configs_env["data"]["configs"]
        .as_array()
        .unwrap_or_else(|| panic!("P-004b data.configs 应为数组，got {}", configs_env["data"]))
        .iter()
        .find(|c| c["id"] == config_id.as_str())
        .unwrap_or_else(|| panic!("P-004b 播种的配置应出现在列表，got {}", configs_env["data"]));
    assert_eq!(
        seeded["name"], CONFIG_NAME,
        "P-004b 配置条目名应与播种值逐字一致（wire 字段名漂移会在这里现形）"
    );

    let quick = execute_proxy(
        desktop_request("p004b2", "GET", &format!("{base}/api/quick-actions"), None),
        None,
    )
    .await
    .expect("P-004b /api/quick-actions");
    assert_eq!(quick.status, 200, "P-004b /api/quick-actions 应 200");
    let quick_env = envelope_ok(&quick.body_text, "P-004b /api/quick-actions");
    assert!(
        quick_env["data"]["actions"].is_array(),
        "P-004b data.actions 应为数组（空数组 = 放行了但无数据），got {}",
        quick_env["data"]
    );

    let queue = execute_proxy(
        desktop_request(
            "p004b3",
            "GET",
            &format!("{base}/api/plugin/com.wasmnest.terminal-session/task-queue/list?session_id={session_id}"),
            None,
        ),
        None,
    )
    .await
    .expect("P-004b task-queue/list");
    assert_eq!(queue.status, 200, "P-004b task-queue/list 应 200");
    let queue_env = envelope_ok(&queue.body_text, "P-004b task-queue/list");
    assert_eq!(
        queue_env["data"]["session_id"],
        session_id.as_str(),
        "P-004b task-queue/list 应回显请求的 session_id（query 透传 + 内部端点档位 jwt 放行）"
    );

    // GET 面同样走加密协商（无请求体 → 仅出站加密 +1；至少证明响应密文被移动端解开）
    let (pre_get_enc, _, _) = desktop_ctx::link_crypto_counters();
    execute_proxy(
        desktop_request("p004c", "GET", &format!("{base}/api/quick-actions"), None),
        None,
    )
    .await
    .expect("P-004c 加密态下的 GET");
    let (post_get_enc, _, _) = desktop_ctx::link_crypto_counters();
    assert!(
        post_get_enc > pre_get_enc,
        "P-004c GET 也带协商头，桌面端应加密响应（{pre_get_enc} → {post_get_enc}）；\
         若为 0 说明响应没加密，移动端拿到的是可解析明文——这条契约就是防这个"
    );

    // ==================== P-005 取消在途请求 ====================
    // 「在途」用本地黑洞监听器构造：接受连接后永不回字节（不伪造任何协议应答，
    // 只让请求挂在传输层直到被取消）
    let blackhole = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind blackhole listener");
    let blackhole_port = blackhole.local_addr().expect("blackhole addr").port();
    egress_declare_desktop_target("127.0.0.1".to_string(), blackhole_port).expect("声明黑洞目标");
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = blackhole.accept().await {
            held.push(stream); // 持有连接、永不读写
        }
    });

    let cancel_id = "p005".to_string();
    let in_flight = {
        let cancel_id = cancel_id.clone();
        let url = format!("http://127.0.0.1:{blackhole_port}/api/sessions");
        tokio::spawn(async move {
            execute_proxy(
                HttpProxyRequest {
                    request_id: cancel_id,
                    method: "GET".to_string(),
                    url,
                    headers: Default::default(),
                    body: None,
                    timeout_ms: Some(60_000),
                    kind: Some("desktop".to_string()),
                },
                None,
            )
            .await
        })
    };
    // 让请求先真正发出去（进入 pending 表）再取消
    tokio::time::sleep(Duration::from_millis(300)).await;
    http_cancel(cancel_id.clone()).await.expect("P-005 http_cancel 应成功");
    let cancelled = in_flight
        .await
        .expect("P-005 在途任务不应 panic")
        .expect_err("P-005 在途请求必须被取消");
    assert!(
        cancelled.to_string().contains("REQUEST_CANCELED"),
        "P-005 取消应显性报 REQUEST_CANCELED，got {cancelled}"
    );
    // 未知 request_id 幂等（前端取消按钮重复点 / 超时后清理都走这里）
    http_cancel("p005-unknown-id".to_string())
        .await
        .expect("P-005 未知 request_id 的取消必须幂等成功");

    // ==================== P-006 两端开关不一致：桌面端必须**显性拒绝** ====================
    // 背景：spec §6 第 3 条「响应绑定」要求对端按协商头参与，而实现语义段又写
    // 「主开关关闭 ⇒ 过滤器不注册」——两者矛盾。实现落在后者，修复前后果是：
    // 密文原样透给插件 → 插件当 JSON 解析 → 回 `1002 configId required`（误导性业务码，
    // HTTP 200）。本组用例钉死 fail-visible 形状：显性 4xx + 点名开关。
    desktop_ctx::set_link_crypto_enabled(false);

    // 带 body 的 POST（密文）
    let refused = execute_proxy(
        desktop_request(
            "p006-post",
            "POST",
            &format!("{base}/api/sessions/start"),
            Some(serde_json::json!({ "configId": config_id, "cols": 100, "rows": 30 }).to_string()),
        ),
        None,
    )
    .await
    .expect("P-006 桌面端不参与时请求应走完传输（拒绝在网关/过滤器层，不是连接失败）");
    assert_eq!(
        refused.status, 400,
        "P-006 桌面端未参与加密却收到协商头 → 必须显性 4xx；got {} body={}",
        refused.status, refused.body_text
    );
    assert!(
        refused.body_text.contains("trafficEncryption"),
        "P-006 拒绝消息应点名开关（否则用户拿到一句无处置建议的 400），got {}",
        refused.body_text
    );
    assert!(
        !refused.body_text.contains("configId required"),
        "P-006 这条路径不得再退化成插件的业务错误（密文当 JSON 解析的产物），got {}",
        refused.body_text
    );

    // GET 也拒：否则「GET 静默明文通、POST 神秘报业务错」的不一致更难排查
    let refused_get = execute_proxy(
        desktop_request("p006-get", "GET", &format!("{base}/api/sessions"), None),
        None,
    )
    .await
    .expect("P-006 GET 同样走完传输");
    assert_eq!(
        refused_get.status, 400,
        "P-006 GET 带协商头时也必须显性拒绝（不得静默按明文放行），got {}",
        refused_get.status
    );

    // opt-in 基线回归：桌面端不参与时，**无协商头的明文请求照常通过**
    let mut crypto = wasmnest_mobile_lib::state::get_link_crypto_context();
    crypto.enabled = false;
    wasmnest_mobile_lib::state::set_link_crypto_context(crypto);
    let still_plain_ok = execute_proxy(
        desktop_request("p006-plain", "GET", &format!("{base}/api/sessions"), None),
        None,
    )
    .await
    .expect("P-006 默认态（两端都不加密）明文请求必须照常通过");
    assert_eq!(
        still_plain_ok.status, 200,
        "P-006 两端都关 = 明文可用（opt-in 基线不得被 fail-visible 改掉），body={}",
        still_plain_ok.body_text
    );

    // 复原（后半段用例与收尾都跑在「桌面端参与」这一侧）
    desktop_ctx::set_link_crypto_enabled(true);
    let mut crypto = wasmnest_mobile_lib::state::get_link_crypto_context();
    crypto.enabled = true;
    wasmnest_mobile_lib::state::set_link_crypto_context(crypto);

    // ==================== 收尾 ====================
    // 收尾请求走明文代理（把加密开关复原，避免遗留态影响后续场景）
    let mut crypto = wasmnest_mobile_lib::state::get_link_crypto_context();
    crypto.enabled = false;
    wasmnest_mobile_lib::state::set_link_crypto_context(crypto);
    execute_proxy(
        desktop_request(
            "teardown",
            "DELETE",
            &format!("{base}/api/sessions/{session_id}/remove"),
            None,
        ),
        None,
    )
    .await
    .expect("收尾移除会话");
    mobile_ctx::clear_identity();
    desktop_ctx::stop_server(handle, server_task).await;
}
