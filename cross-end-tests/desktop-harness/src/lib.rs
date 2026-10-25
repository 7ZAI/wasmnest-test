//! 桌面端无头装配 + JSON-RPC 控制面（两进程跨端 rig 的桌面进程）
//!
//! ## 与 `wasmnest-test/tests/common/desktop_ctx.rs` 同源同构
//!
//! 两者都是「无头 `AppContext` + `install_server_ports` + activate + `start_http_server`
//! + 链路加密装配 + 插件互调 + WS 端点观测」的同一套装配。那份是**单进程 rig** 的
//! 进程内直调面（测试进程里直接 `AppContext::global()`）；本 lib 是**独立进程**的
//! 装配面（被 `wasmnest-crossend-desktop-harness` bin 长期持有，经 stdin/stdout
//! JSON-RPC 对外服务）。装配顺序必须与 GUI bootstrap（`wasmnest/wasmnest-desktop/src-tauri/
//! src/lib.rs` setup）一致（server-lib-split 票 07：端口注册表先装、activate 最后，
//! 早一步会装上占位空总线）——**改装配顺序必须同步三处**。
//!
//! ## 本进程只含桌面侧宿主模块（设计不变量）
//!
//! 本 crate 的依赖图只有桌面 lib + server-* crates（Cargo.toml 里没有移动 lib），
//! `ModuleRegistry::collected()`（`inventory` 进程级注册）因此只含桌面实现；
//! 移动侧的 `fallback` 让位语义在这里无竞争、不需要。装配守卫
//! [`guard_side`] 在 bin 启动时钉住这条不变量（fail-visible：惰性链接/依赖图被
//! 破坏 → 进程不起来）。
//!
//! spec：`.scratch/2026-10-15-cross-end-two-process/spec.md`

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};
use wasmnest_desktop_lib::db::Database;
use wasmnest_desktop_lib::server::composition::{install_server_ports, start_http_server};
use wasmnest_desktop_lib::system::app_context::{AppContext, AppContextBuilder};
use wasmnest_desktop_lib::system::info::SystemInfo;
use wasmnest_desktop_lib::wasm_core::PluginHost;
use wasmnest_desktop_lib::AppConfig;
use wasmnest_discovery_engine::advertiser::MdnsAdvertiser;
use wasmnest_server_websocket::endpoint;
use wasmnest_server_websocket::registry::{ClientSummary, WsSessionRegistry};

/// 认证中心 / 会话真源插件 id（`com.wasmnest.bedcode`：配对、JWT 签发、
/// 会话登记域、WS 端点编排全在它自己进程内）
pub const SESSION_PLUGIN_ID: &str = "com.wasmnest.bedcode";

// ==================== 装配守卫（本进程只含桌面侧模块） ====================

/// 桌面侧独有宿主模块名（与移动侧区分；`fs-desktop` / `process` / `task` /
/// `platform-desktop` / `api_call` / `crypto` / `app` / `timer` 均在
/// `wasmnest/wasmnest-desktop/src-tauri/src/plugin/*.rs` 自报，移动侧没有同名项）
const DESKTOP_ONLY_MODULES: &[&str] = &[
    "fs-desktop",
    "process",
    "task",
    "platform-desktop",
    "api_call",
    "crypto",
    "app",
    "timer",
];

/// 移动侧独有宿主模块名（`mobile-*` 前缀全部来自 `wasmnest/wasmnest-mobile/src-tauri/src/plugin/*`，
/// 本进程不得出现）
const MOBILE_ONLY_MODULES: &[&str] = &[
    "mobile-fs",
    "mobile-auth",
    "mobile-connection",
    "mobile-notify",
    "mobile-host-core",
    "mobile-terminal-stream",
    "mobile-desktop-compat",
];

/// 启动守卫：`inventory` 收集结果必须「含桌面独有、不含移动独有」（fail-visible）
pub fn guard_side() {
    let names: Vec<String> = wasmnest_host_kit::ModuleRegistry::collected()
        .descs()
        .iter()
        .map(|d| d.name.to_string())
        .collect();
    let joined = names.join(", ");
    for must in DESKTOP_ONLY_MODULES {
        assert!(
            names.iter().any(|n| n == must),
            "桌面进程缺少宿主模块 {must}（inventory 收集异常，依赖图被破坏？）：{joined}"
        );
    }
    for forbid in MOBILE_ONLY_MODULES {
        assert!(
            !names.iter().any(|n| n == forbid),
            "桌面进程混入移动宿主模块 {forbid}（依赖图/惰性链接异常，必须拆 crate）：{joined}"
        );
    }
    tracing::debug!(modules = %joined, "desktop harness: host module guard passed");
}

// ==================== 装配（与 desktop_ctx.rs 同源） ====================

/// 随包 wasm 产物目录（桌面端 `pnpm run tauri:build` / `plugin-build.js` 产出）
///
/// 产物缺失时**显性失败**而非跳过：跨端互连的唯一驱动方式就是真实产物
/// （与 desktop_ctx.rs / pty_session_chain 同口径）。
pub fn bundled_plugins_dir() -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../wasmnest/wasmnest-desktop/src-tauri/resources/plugins/desktop");
    let artifact = dir.join(format!("{SESSION_PLUGIN_ID}/bedcode_plugin_terminal_session.wasm"));
    assert!(
        artifact.exists(),
        "认证中心插件产物缺失：先在 wasmnest/wasmnest-desktop/ 跑 `pnpm run tauri:build` \
         （或 node scripts/plugin-build.js --plugin {SESSION_PLUGIN_ID}），\
         期望产物：{}",
        artifact.display()
    );
    dir
}

/// 用户插件目录（独立空目录；复用随包目录会把随包插件标成 UserInstalled 来源，
/// 激活时撞审批门禁）
fn user_plugins_dir() -> PathBuf {
    std::env::temp_dir().join(format!("wasmnest-crossend2-userplugins-{}", std::process::id()))
}

/// 无头上下文的插件私有库根（配对 / 会话 / 认证记录真源在插件私有库，
/// 无头模式无 AppHandle，经 `set_plugin_db_root` 注入）
fn plugin_db_root() -> &'static PathBuf {
    static ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    ROOT.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("wasmnest-crossend2-pluginroot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    })
}

/// 链路加密身份材料目录（GUI 侧 = `app_data_dir`；无头进程取临时目录）
fn link_crypto_dir() -> &'static PathBuf {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("wasmnest-crossend2-linkcrypto-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    })
}

/// 组装真实服务 AppContext（`app_handle=None` 无头模式）+ 激活认证中心插件
///
/// 进程级 `OnceLock` 单例：本进程只装配一次。`activate_center=false` 时先不激活
/// （fail-closed 场景观察「无中心在册」），随后可经 [`activate_session_center`] 激活。
pub async fn init_app_context(activate_center: bool) {
    static INIT: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    if INIT.get().is_some() {
        return;
    }
    {
        let db = Arc::new(std::sync::Mutex::new(
            Database::new(Path::new(":memory:")).expect("create in-memory db failed"),
        ));
        db.lock()
            .expect("db lock")
            .init_schema()
            .expect("init db schema failed");

        let plugins_dir = bundled_plugins_dir();
        let user_dir = user_plugins_dir();
        std::fs::create_dir_all(&user_dir).expect("create temp user plugins dir failed");

        let plugin_host = PluginHost::new(
            db.clone(),
            &plugins_dir,
            &user_dir,
            None,
            None,
            Vec::new(),
            wasmnest_desktop_lib::wasm_core::manager::runtime::AbiPolicy::UNCONSTRAINED,
            std::sync::Arc::new(wasmnest_desktop_lib::wasm_core::host_api::ports::UnimplementedHostFacts),
            None,
            None,
        )
        .await;
        plugin_host.init_message_bus().await;
        // guest 事件派发器（生产同款）：缺省 `UnwiredGuestEvents` 会让总线投递 fail-visible
        //（会话中心 dispatch 报错 → trap；实测症状 = 插件 api 回调超时）。本进程自建
        // PluginHost 须自行注入——晚绑定使注入对扫描期已建实例同样生效。
        plugin_host.wasm_runtime().set_guest_events(std::sync::Arc::new(
            wasmnest_desktop_lib::plugin::guest_events::DesktopGuestEvents,
        ));
        plugin_host
            .wasm_host_ctx()
            .set_plugin_db_root(Some(plugin_db_root().clone()));

        let mdns_advertiser = Arc::new(tokio::sync::RwLock::new(MdnsAdvertiser::new()));
        let system_info = Arc::new(SystemInfo::collect());

        // 顺序 = GUI bootstrap 的同一顺序（src-tauri/src/lib.rs setup）：
        // AppContext 先注册 → 端口装配（`assemble()` 取总线走 `AppContext::try_global`，
        // 早一步会装上占位空总线，插件的 `bus-subscribe` 便永收不到互调请求）→
        // 最后才激活插件（激活期宿主据 manifest 自动登记端点，端点表与
        // `PluginInvoker` 端口此刻都必须已就位）。
        AppContextBuilder::new()
            .db(db.clone())
            .plugin_host(plugin_host.clone())
            .mdns_advertiser(mdns_advertiser.clone())
            .app_handle(None)
            .resource_dir(Arc::new(PathBuf::from(".")))
            .system_info(system_info)
            .build_and_init();
        install_server_ports();

        if activate_center {
            activate_session_center().await;
        }
        let _ = INIT.set(());
    }
}

/// 激活认证中心插件（`init_app_context(false)` 之后用；幂等）
pub async fn activate_session_center() {
    AppContext::global()
        .plugin_host()
        .activate_plugin(SESSION_PLUGIN_ID, false)
        .await
        .expect("activate com.wasmnest.bedcode (bundled artifact)");
}

/// 启动真实 HTTP/WS 服务器（Actix，`start_http_server` 纯函数、不依赖 AppHandle）
///
/// 端口由调用方给定（bin 用 [`pick_free_port`] 自选后 REPORT，测试不猜端口）。
pub async fn start_server(
    port: u16,
) -> (
    actix_web::dev::ServerHandle,
    tokio::task::JoinHandle<std::io::Result<()>>,
) {
    let config = AppConfig::default().network;
    let (handle, server) = start_http_server(port, &config).await.expect("test server must start");
    let task = tokio::spawn(server);
    (handle, task)
}

/// 优雅停机并等待服务器任务退出（超时 panic：静默超时会让端口被占用影响后续 target）
pub async fn stop_server(handle: actix_web::dev::ServerHandle, task: tokio::task::JoinHandle<std::io::Result<()>>) {
    tokio::time::timeout(std::time::Duration::from_secs(10), handle.stop(true))
        .await
        .expect("graceful stop must complete within timeout");
    task.await
        .expect("server task must not panic")
        .expect("server must exit Ok after graceful stop");
}

/// 探测空闲端口：绑 127.0.0.1:0 由 OS 分配，立即释放后交给服务器绑定
pub fn pick_free_port() -> u16 {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("probe free port failed");
    listener.local_addr().expect("read probed port failed").port()
}

/// 宿主 → 插件互调（JSON-RPC 约定，与 desktop_ctx.rs `plugin_api_call` 同 wire）
///
/// **params = 该 api 声明的入参值本身**（不是带参数名的对象）：SDK 宏按参数个数
/// 生成反序列化——单参 `x: T` 直接 `from_value(params)`。所以
/// `config-upsert(draft: Value)` 的 params 是 draft 对象，
/// `qr-code-generate(ttl: u64)` 的 params 是裸 `300`；传 `{"ttl":300}` 会被
/// 插件侧报 `invalid params`（且回复走 error 通道，宿主侧表现为超时）。
///
/// 可失败版（rpc 控制面用：错误要走 JSON-RPC error 通道，不得靠 panic 传递）。
pub async fn try_plugin_api_call(api: &str, params: Value) -> Result<Value, String> {
    let host_ctx = AppContext::global().plugin_host().wasm_host_ctx().clone();
    let topic = format!("wasmnest.api.{api}");
    let method = api.rsplit_once('.').map(|(_, m)| m).unwrap_or(api);
    let id = format!(
        "crossend2-req-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let payload = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    });
    let reply_json = host_ctx
        .call_plugin_api_host(&topic, &payload.to_string(), 5000)
        .map_err(|e| format!("plugin api '{api}' failed: {e}"))?;
    let reply: Value = serde_json::from_str(&reply_json).map_err(|e| format!("reply json: {e}"))?;
    if let Some(err) = reply.get("error") {
        return Err(format!("plugin api '{api}' error: {err}"));
    }
    reply
        .get("result")
        .cloned()
        .ok_or_else(|| format!("plugin api '{api}' reply missing result: {reply}"))
}

/// 驱动插件 **Rust 命令面**（= 桌面宿主前端那条路，与 `plugin_invoke` 同一下游）
///
/// 走 `PluginHost::invoke_rust_command`（`api_bridge::plugin_invoke` 的下游，
/// 去掉的是 webview + 凭证层——那是身份管道，不是输出管道）。可失败版。
pub async fn try_plugin_command(plugin_id: &str, command: &str, args: Value) -> Result<Value, String> {
    AppContext::global()
        .plugin_host()
        .invoke_rust_command(plugin_id, command, args)
        .await
        .map_err(|e| e.to_string())
}

/// 播种一条可直接启动的会话配置（bash，工作目录临时目录），返回 config id
pub async fn seed_shell_config(name: &str) -> String {
    let config = try_plugin_api_call(
        "com.wasmnest.bedcode.config-upsert",
        json!({
            "name": name,
            "environment": "linux",
            "workingDir": std::env::temp_dir().to_string_lossy(),
            "command": "bash",
        }),
    )
    .await
    .expect("config-upsert 必须成功");
    config["id"].as_str().expect("config-upsert must return id").to_string()
}

// ==================== 链路加密（HTTP 信封）headless 装配 ====================
//
// 与 desktop_ctx.rs `enable_link_crypto_http` 同款（`link_crypto::init_at_startup`：
// 建 Kd 身份 → 读 DB 配置 → 同步注册），差别只在数据目录由本进程供给（无 AppHandle
// 取不到 `app_data_dir`）。必须早于配对：认证响应里的 `kdPublicB64` 就是这条身份
// 公钥（认证中心经 `host-auth.link-identity-parts` 读）——pin 不能手工造假。
// 本阶段场景（127.0.0.1 环回）不开启；为 http_proxy 场景（非环回 + 加密）留口。

/// 启动期链路加密装配（headless 版）+ 开启 HTTP 子通道（幂等）
pub async fn enable_link_crypto_http() {
    std::fs::create_dir_all(link_crypto_dir()).expect("create link crypto dir");
    let guard = AppContext::global().db().lock().expect("db lock");
    wasmnest_server_core::link_crypto::init_at_startup(link_crypto_dir(), guard.conn());
    set_link_crypto_enabled(true);
    assert!(
        wasmnest_server_core::link_crypto::identity_parts().is_some(),
        "rig 链路加密身份未就绪（建身份失败会被 fail-safe 静默降级为全关）"
    );
}

/// 运行期开关（与桌面端宿主 `set_traffic_encryption_config` 同款，差别是不落库）
pub fn set_link_crypto_enabled(enabled: bool) {
    let mut config = wasmnest_server_core::link_crypto::current_config();
    config.enabled = enabled;
    wasmnest_server_core::link_crypto::update_config(config);
    wasmnest_server_core::link_crypto::sync_registration();
}

/// 桌面侧链路加密计数器快照（**只读**：加密帧数 / 解密失败数 / 响应取钥失败数）
pub fn link_crypto_counters() -> (u64, u64, u64) {
    let m = wasmnest_server_core::metrics::MetricsCollector::global().sample(0, 0.0, 0);
    (m.encrypted_frames, m.decrypt_failures, m.response_key_miss)
}

// ==================== 插件 WS 端点的只读观测（事件通道 / 终端场景用） ====================

/// 插件 WS 端点句柄（按属主 + 路径后缀反查；未登记则显性失败）
///
/// 插件侧拿到的 `wse-` 句柄不可预测，测试与它走同一条真源：
/// [`endpoint::find_by_mount`]（路由分发自身用的那张表）。
pub fn plugin_ws_endpoint_id(plugin_id: &str, path: &str) -> String {
    let mount = endpoint::mount_path(plugin_id, path);
    endpoint::find_by_mount(&mount)
        .unwrap_or_else(|| panic!("插件 WS 端点未登记：{mount}（插件未激活或 manifest 未声明该端点）"))
        .endpoint_id
}

/// 该端点当前在线的连接摘要（**含认证态**），序列化为 JSON（rpc 通道用）
pub async fn plugin_ws_endpoint_clients(plugin_id: &str, path: &str) -> Vec<Value> {
    let endpoint_id = plugin_ws_endpoint_id(plugin_id, path);
    WsSessionRegistry::global()
        .list_by_endpoint(&endpoint_id)
        .await
        .into_iter()
        .map(|c| client_summary_json(&c))
        .collect()
}

/// 桌面侧主动断开该端点的全部连接（返回断开条数；0 条 = 没有在线连接）
///
/// 用于「意外掉线」场景的服务端侧动作（等价于网络中断时骨架收尾的那一步），
/// 不伪造任何数据：断开之后帧真的不再流动，由断言去验。
pub async fn disconnect_plugin_ws_endpoint_clients(plugin_id: &str, path: &str, code: u16, reason: &str) -> usize {
    let endpoint_id = plugin_ws_endpoint_id(plugin_id, path);
    WsSessionRegistry::global()
        .disconnect_by_endpoint(&endpoint_id, code, reason)
        .await
}

/// 连接摘要 → JSON（`ClientSummary` 无 Serialize 派生，rpc 通道手工序列化）
pub fn client_summary_json(c: &ClientSummary) -> Value {
    json!({
        "clientId": c.client_id,
        "subject": c.subject,
        "deviceName": c.device_name,
        "fingerprint": c.fingerprint,
        "addr": c.addr,
        "authenticated": c.authenticated,
        "connectedAt": c.connected_at,
    })
}

// ==================== JSON-RPC 控制面 ====================

/// 控制面方法分发（被 bin 的 stdin 循环逐行调用）
///
/// 返回 `Ok(result)` / `Err(message)`——错误经 JSON-RPC error 通道回给测试进程，
/// 本进程不得 panic（panic 会带走整台桌面，测试只能拿到超时 + stderr dump）。
pub async fn rpc_dispatch(method: &str, params: Value) -> Result<Value, String> {
    match method {
        "activate" => {
            activate_session_center().await;
            Ok(Value::Null)
        }
        "seed-shell-config" => {
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| "seed-shell-config: missing name".to_string())?
                .to_string();
            Ok(json!({ "id": seed_shell_config(&name).await }))
        }
        "plugin-command" => {
            let plugin_id = required_str(&params, "pluginId")?;
            let command = required_str(&params, "command")?;
            let args = params.get("args").cloned().unwrap_or(Value::Null);
            try_plugin_command(&plugin_id, &command, args).await
        }
        "plugin-api-call" => {
            let api = required_str(&params, "api")?;
            let p = params.get("params").cloned().unwrap_or(Value::Null);
            try_plugin_api_call(&api, p).await
        }
        "ws-endpoint-clients" => {
            let plugin_id = required_str(&params, "pluginId")?;
            let path = required_str(&params, "path")?;
            Ok(json!(plugin_ws_endpoint_clients(&plugin_id, &path).await))
        }
        "disconnect-ws-endpoint" => {
            let plugin_id = required_str(&params, "pluginId")?;
            let path = required_str(&params, "path")?;
            let code = params.get("code").and_then(Value::as_u64).unwrap_or(1001) as u16;
            let reason = params.get("reason").and_then(Value::as_str).unwrap_or("server action");
            let count = disconnect_plugin_ws_endpoint_clients(&plugin_id, &path, code, reason).await;
            Ok(json!({ "count": count }))
        }
        "enable-link-crypto-http" => {
            enable_link_crypto_http().await;
            Ok(Value::Null)
        }
        "set-link-crypto-enabled" => {
            let enabled = params
                .get("enabled")
                .and_then(Value::as_bool)
                .ok_or_else(|| "missing enabled".to_string())?;
            set_link_crypto_enabled(enabled);
            Ok(Value::Null)
        }
        "link-crypto-counters" => {
            let (e, f, m) = link_crypto_counters();
            Ok(json!([e, f, m]))
        }
        "shutdown" => Ok(Value::Null), // bin 在分发前单独处理（要收尾停服务器）
        other => Err(format!("unknown rpc method: {other}")),
    }
}

fn required_str(params: &Value, key: &str) -> Result<String, String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("missing string param '{key}'"))
}

/// 场景收尾：清理临时目录（随包产物目录 `resources/plugins/desktop` 不得触碰）
pub fn cleanup_temp_dirs() {
    let _ = std::fs::remove_dir_all(user_plugins_dir());
    let _ = std::fs::remove_dir_all(plugin_db_root());
    let _ = std::fs::remove_dir_all(link_crypto_dir());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// rpc_dispatch 的纯参数面：缺参 / 未知方法必须走 error 通道（不得 panic）
    #[tokio::test]
    async fn dispatch_unknown_and_bad_params_are_errors() {
        assert!(rpc_dispatch("no-such-method", json!({})).await.is_err());
        assert!(rpc_dispatch("seed-shell-config", json!({})).await.is_err());
        assert!(rpc_dispatch("ws-endpoint-clients", json!({ "path": "terminal" }))
            .await
            .is_err());
    }

    /// 摘要序列化：字段逐字对应（rpc 通道的 wire 形状 = 测试断言读的键）
    #[test]
    fn client_summary_json_shape() {
        let c = ClientSummary {
            client_id: "c1".into(),
            subject: Some("dev".into()),
            device_name: Some("CrossEnd".into()),
            fingerprint: Some("fp".into()),
            addr: "127.0.0.1:1".into(),
            authenticated: true,
            connected_at: 42,
        };
        let v = client_summary_json(&c);
        assert_eq!(v["clientId"], "c1");
        assert_eq!(v["authenticated"], true);
        assert_eq!(v["connectedAt"], 42);
    }
}
