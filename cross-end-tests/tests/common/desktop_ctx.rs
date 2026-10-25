//! 桌面端无头装配：真实 AppContext + 真实认证中心插件 + 真实 HTTP/WS 服务器
//!
//! 复刻 `wasmnest/wasmnest-desktop/src-tauri/tests/pty_session_chain.rs`（`app_handle(None)`
//! 无头模式）。与桌面端单端测试的差别只有一处、也是本工程的全部意义：
//! **对面是移动端真实客户端代码**，不是通用 reqwest / tokio-tungstenite。
//!
//! **装配面必须与 GUI bootstrap 完全同款**（server-lib-split 票 07）：端口注册表
//! 经宿主组合根的单一装配点 [`wasmnest_desktop_lib::server::composition`]
//! `install_server_ports` 装。历史上本 rig 只建 `AppContext` 不装端口，症状是
//! HTTP 网关 `ports::get() == None` 判 `PassThrough` → 插件在激活期登记的全部宿主
//! 别名（`/api/auth/*` / `/api/sessions*` / `/api/configs`）一律 404，而端口占用、
//! WS 升级等不依赖端口的路径照常工作——七个场景同时红、且看起来像协议问题。
//! 自检用例 `rig_assembles_the_same_server_ports_face_as_gui_boot` 钉住这条不变量。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use wasmnest_desktop_lib::db::Database;
use wasmnest_discovery_engine::advertiser::MdnsAdvertiser;
use wasmnest_server_websocket::endpoint;
use wasmnest_server_websocket::registry::{ClientSummary, WsSessionRegistry};
// server-lib-split 票 03：内核组合入口 `server::core::app` 已下沉为
// `wasmnest_server_core::app::serve`，宿主侧兼容面在 `server::composition`
// （签名与拆分前一致，调用点不必改）
use wasmnest_desktop_lib::server::composition::{install_server_ports, start_http_server};
use wasmnest_desktop_lib::system::app_context::{AppContext, AppContextBuilder};
use wasmnest_desktop_lib::system::info::SystemInfo;
use wasmnest_desktop_lib::wasm_core::PluginHost;
use wasmnest_desktop_lib::AppConfig;

/// 认证中心 / 会话真源插件 id（`com.wasmnest.bedcode`：配对、JWT 签发、
/// 会话登记域、WS 端点编排全在它自己进程内）
pub const SESSION_PLUGIN_ID: &str = "com.wasmnest.bedcode";

/// 常驻事件通道端点后缀（插件 manifest `contributes.wsEndpoints` 声明值；宿主拼出
/// 完整挂载路径 `/ws/plugin/<plugin-id>/session-control`）
pub const EVENT_CHANNEL_PATH: &str = "session-control";

/// 单次测试的等待预算（与移动端装配同口径）
const WAIT_TIMEOUT: Duration = Duration::from_secs(15);

// ==================== 插件 WS 端点的只读观测（事件通道场景用） ====================

/// 插件 WS 端点句柄（按属主 + 路径后缀反查；未登记则显性失败）
///
/// 插件侧拿到的 `wse-` 句柄不可预测，宿主测试与它走同一条真源：
/// [`endpoint::find_by_mount`]（路由分发自身用的那张表）。
pub fn plugin_ws_endpoint_id(plugin_id: &str, path: &str) -> String {
    let mount = endpoint::mount_path(plugin_id, path);
    endpoint::find_by_mount(&mount)
        .unwrap_or_else(|| panic!("插件 WS 端点未登记：{mount}（插件未激活或 manifest 未声明该端点）"))
        .endpoint_id
}

/// 该端点当前在线的连接摘要（**含认证态**）
pub async fn plugin_ws_endpoint_clients(plugin_id: &str, path: &str) -> Vec<ClientSummary> {
    let endpoint_id = plugin_ws_endpoint_id(plugin_id, path);
    WsSessionRegistry::global().list_by_endpoint(&endpoint_id).await
}

/// 轮询直到该端点上有**首帧认证已通过**的连接（超时 panic 并打印现场）
///
/// 这是「事件通道就绪」的无竞态判据：插件侧广播前先看 `clientCount > 0`（零客户端
/// 早退），而认证态置位才意味着这一条连接真的会收到广播帧——两者之间的窗口用
/// 事件断言去等只会偶发丢首帧（真实生产里由前端 `ws_event_channel_ready` 之后的
/// HTTP 对账兜底，但测试不能靠兜底蒙混）。
pub async fn wait_plugin_ws_endpoint_authenticated(plugin_id: &str, path: &str, what: &str) -> ClientSummary {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    loop {
        let clients = plugin_ws_endpoint_clients(plugin_id, path).await;
        if let Some(c) = clients.iter().find(|c| c.authenticated) {
            return c.clone();
        }
        if Instant::now() >= deadline {
            panic!("timed out waiting for {what}; endpoint clients={clients:#?}");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 轮询直到该端点上出现一条**与给定连接不同**且首帧认证已通过的连接（超时 panic）
///
/// 自愈类断言用「换了连接」而不是「曾经空过」：实测监督任务的重连快到
/// **毫秒级**（HTTP reauth → 重建，全程 <10ms），任何轮询间隔都观察不到中间的空窗，
/// 「空过」因此是不可靠判据；「client_id 变了 + 已认证」既是可靠判据，也正是
/// 生产真正要的那条性质（真的重建了一条连接，而不是沿用旧连接）。
pub async fn wait_new_plugin_ws_endpoint_client(
    plugin_id: &str,
    path: &str,
    previous_client_id: &str,
    what: &str,
) -> ClientSummary {
    match poll_new_plugin_ws_client(plugin_id, path, previous_client_id, WAIT_TIMEOUT).await {
        Some(client) => client,
        None => panic!(
            "timed out waiting for {what}; endpoint clients={:#?}",
            plugin_ws_endpoint_clients(plugin_id, path).await
        ),
    }
}

/// 同上的「反例」版：给定预算内**不得**出现新连接（返回是否真的保持静默）
///
/// 用于「认证类致命关闭不自愈」这类反例：观察窗内没有新连接才算通过。
pub async fn assert_no_new_plugin_ws_client(
    plugin_id: &str,
    path: &str,
    known_client_id: &str,
    what: &str,
    budget: Duration,
) {
    if let Some(client) = poll_new_plugin_ws_client(plugin_id, path, known_client_id, budget).await {
        panic!("{what}：不该出现的重连真的发生了，新连接 = {}", client.client_id);
    }
}

/// 轮询窗口内寻找「不同于 `previous_client_id` 的已认证连接」；窗口耗尽返回 `None`
async fn poll_new_plugin_ws_client(
    plugin_id: &str,
    path: &str,
    previous_client_id: &str,
    budget: Duration,
) -> Option<ClientSummary> {
    let deadline = Instant::now() + budget;
    loop {
        let fresh: Option<ClientSummary> = plugin_ws_endpoint_clients(plugin_id, path)
            .await
            .into_iter()
            .find(|c| c.authenticated && c.client_id != previous_client_id);
        if fresh.is_some() {
            return fresh;
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
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

/// 随包 wasm 产物目录（桌面端 `pnpm run tauri:build` / `plugin-build.js` 产出）
///
/// 产物缺失时**显性失败**而非跳过：跨端互连的唯一驱动方式就是真实产物，
/// 静默 skip 会把「未验证」伪装成「通过」（与 `pty_session_chain` 同口径）。
pub fn bundled_plugins_dir() -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../wasmnest/wasmnest-desktop/src-tauri/resources/plugins/desktop");
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

/// 无头上下文的插件私有库根（配对 / 会话 / 认证记录真源在插件私有库，
/// 无头模式无 AppHandle，经 `set_plugin_db_root` 注入）
fn plugin_db_root() -> &'static PathBuf {
    static ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    ROOT.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("wasmnest-crossend-pluginroot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    })
}

/// 用户插件目录（独立空目录；复用随包目录会把随包插件标成 UserInstalled 来源，
/// 激活时撞审批门禁）
fn user_plugins_dir() -> PathBuf {
    std::env::temp_dir().join(format!("wasmnest-crossend-userplugins-{}", std::process::id()))
}

/// 链路加密身份材料目录（GUI 侧 = `app_data_dir`；无头 rig 取临时目录）
fn link_crypto_dir() -> &'static PathBuf {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("wasmnest-crossend-linkcrypto-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    })
}

// ==================== 链路加密（HTTP 信封）headless 装配 ====================

/// 启动期链路加密装配（headless 版）+ 开启 HTTP 子通道
///
/// 与 GUI 的 `composition::init_link_crypto_at_startup(app_handle)` **同款**
/// （`link_crypto::init_at_startup`：建 Kd 身份 → 读 DB 配置 → 同步注册），
/// 差别只在数据目录由 rig 供给（无 AppHandle 取不到 `app_data_dir`）。
///
/// 必须早于配对：认证响应里的 `kdPublicB64` 就是这条身份公钥（认证中心经
/// `host-auth.link-identity-parts` 读），移动端的 pin 由此而来——**pin 不能手工造假**
/// （假 pin 桌面解不开，P-003 会假红）。
pub async fn enable_link_crypto_http() {
    // 身份材料是落盘文件（`load_or_create` 不建父目录，GUI 侧目录由 app_data_dir 保证）
    std::fs::create_dir_all(link_crypto_dir()).expect("create link crypto dir");
    let guard = AppContext::global().db().lock().expect("db lock");
    // `init_at_startup` 自身对身份失败只 error 日志 + 强制全关（fail-safe），
    // 因此这里必须核对快照真的开了，否则后续断言会在“全关但无报错”下假绿
    wasmnest_server_core::link_crypto::init_at_startup(link_crypto_dir(), guard.conn());
    // 主开关：GUI 由用户在设置页写入 DB settings，rig 直接置运行期快照
    // （其余子开关 / 明文回落策略取默认值 = 子通道全开 + 允许老客户端明文）
    wasmnest_server_core::link_crypto::update_config(wasmnest_server_core::link_crypto::LinkCryptoConfig {
        enabled: true,
        ..Default::default()
    });
    wasmnest_server_core::link_crypto::sync_registration();
    assert!(
        wasmnest_server_core::link_crypto::identity_parts().is_some(),
        "rig 链路加密身份未就绪（建身份失败会被 fail-safe 静默降级为全关）"
    );
}

/// 运行期开关：桌面端链路加密参与意愿（与宿主 `set_traffic_encryption_config` 同款，
/// 差别是 rig 不落库——只改内存快照 + 同步过滤器注册）
///
/// 用它覆盖「两端开关不一致」这一格：移动端开着加密、桌面端关着时，桌面端必须
/// **显性拒绝**协商过的请求，而不是把密文透给业务解析。
pub fn set_link_crypto_enabled(enabled: bool) {
    let mut config = wasmnest_server_core::link_crypto::current_config();
    config.enabled = enabled;
    wasmnest_server_core::link_crypto::update_config(config);
    wasmnest_server_core::link_crypto::sync_registration();
}

/// 桌面侧链路加密计数器快照（**只读**：加密帧数 / 解密失败数 / 响应取钥失败数）
///
/// 加密帧计数的口径（`link_crypto.rs`）：实际解封了请求体 +1、成功加密响应 +1；
/// **GET 空 body 协商不计数**（否则会高估加密吞吐）。故“有 body 的加密往返”
/// 每趟 +2，是 P-003 的直接证据。
pub fn link_crypto_counters() -> (u64, u64, u64) {
    let m = wasmnest_server_core::metrics::MetricsCollector::global().sample(0, 0.0, 0);
    (m.encrypted_frames, m.decrypt_failures, m.response_key_miss)
}

/// 组装真实服务 AppContext（`app_handle=None` 无头模式）+ 激活认证中心插件
///
/// 每个测试进程只 init 一次（`AppContext` 是进程级 `OnceLock` 单例）。
pub async fn init_app_context() {
    init_app_context_inner(true).await
}

/// 同上，但**不激活**认证中心（fail-closed 场景：先观察「无中心在册」的桌面
/// 端真实行为，再经 [`activate_session_center`] 激活取正例对照）
pub async fn init_app_context_without_center() {
    init_app_context_inner(false).await
}

/// 激活认证中心插件（`init_app_context_without_center` 之后用；可重复调用）
pub async fn activate_session_center() {
    AppContext::global()
        .plugin_host()
        .activate_plugin(SESSION_PLUGIN_ID, false)
        .await
        .expect("activate com.wasmnest.bedcode (bundled artifact)");
}

async fn init_app_context_inner(activate_center: bool) {
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
        //（会话中心 dispatch 报错 → trap；实测症状 = 插件 api 回调超时）。本 rig 自建
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
            // 激活认证中心：/api/auth/*、/api/sessions*、/api/configs 与两条 WS
            // 插件端点全在插件侧；激活期宿主据 manifest 自动登记端点
            plugin_host
                .activate_plugin(SESSION_PLUGIN_ID, false)
                .await
                .expect("activate com.wasmnest.bedcode (bundled artifact)");
        }
        let _ = INIT.set(());
    }
}

/// 当前系统信息（广播实例名断言的 device_name 来源）
pub fn system_info() -> &'static wasmnest_desktop_lib::system::info::SystemInfo {
    AppContext::global().system_info().as_ref()
}

/// 广播 TXT `version` 的**生产真源**：`SystemInfoPort::app_version()`
///
/// 坑：`AppContext::system_info().app_version` **不是**应用版本——`SystemInfo::collect()`
/// 在 `wasmnest-server-base` 里编译，取的是该**包的**版本（0.1.0）；而广播走端口
/// （宿主壳实现 = 桌面 crate 的 `CARGO_PKG_VERSION` = 0.0.1）。断言必须对着端口。
pub fn advertised_app_version() -> String {
    use wasmnest_server_base::ports::get as ports_get;
    ports_get().expect("server ports installed").system_info.app_version()
}

// ==================== 生产同款服务器启动（经 ServerSupervisor，票 03） ====================

/// 启动服务器（**生产同款**：传输面服务 start 的底层动作）
///
/// 与 [`start_server`] 的区别不是「快慢」而是**语义**：supervisor 是传输面生命周期的
/// 实现（生产由 `net.transport` 服务租约在首个应用引用时调用同一对 start/stop），
/// 它额外做三件与本场景直接相关的事——
/// ① 把端口写进自己的状态（`/api/health` 的 `port` 字段取自这里，不走 supervisor 就是
/// 默认端口，用户与诊断看到的端口会撒谎）；
/// ② 登记 mDNS 广播（用户「发现并连上桌面」的入口）；
/// ③ 重置指标计数器。
///
/// 因此 mDNS / health 场景必须走这条路径（等价于「有应用持有传输面」）；
/// 其余场景走 [`start_server`]（更轻）。
pub async fn start_server_via_supervisor(port: u16) {
    use wasmnest_server_base::ports::get as ports_get;
    use wasmnest_server_core::supervisor::ServerSupervisor;

    let supervisor = ServerSupervisor::global();
    supervisor.init_config(port).await;
    wasmnest_server_websocket::WebSocketManager::global()
        .init()
        .await
        .expect("WebSocketManager init（GUI bootstrap 同一步）");
    // 端口面必须已装配（`install_server_ports`）——否则 supervisor 直接报
    // "server lifecycle port unavailable"
    assert!(
        ports_get().is_some(),
        "server ports must be installed before starting via supervisor"
    );
    supervisor
        .start(port)
        .await
        .expect("supervisor start（生产同款启动路径）");
}

/// 停止 supervisor 管理的服务器（幂等）
pub async fn stop_server_via_supervisor() {
    let _ = wasmnest_server_core::supervisor::ServerSupervisor::global()
        .stop()
        .await;
}
/// 探测空闲端口：绑 127.0.0.1:0 由 OS 分配，立即释放后交给服务器绑定
pub fn pick_free_port() -> u16 {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("probe free port failed");
    listener.local_addr().expect("read probed port failed").port()
}

/// 启动真实 HTTP/WS 服务器（Actix，`start_http_server` 纯函数、不依赖 AppHandle）
///
/// 返回 `(端口, ServerHandle, server 任务句柄)`。
pub async fn start_server() -> (
    u16,
    actix_web::dev::ServerHandle,
    tokio::task::JoinHandle<std::io::Result<()>>,
) {
    let config = AppConfig::default().network;
    let port = pick_free_port();
    let (handle, server) = start_http_server(port, &config).await.expect("test server must start");
    let task = tokio::spawn(server);
    (port, handle, task)
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

/// 宿主 → 插件互调（JSON-RPC 约定，与 `pty_session_chain::plugin_api_call` 同 wire）
///
/// **params = 该 api 声明的入参值本身**（不是带参数名的对象）：SDK 宏按参数个数
/// 生成反序列化——单参 `x: T` 直接 `from_value(params)`。所以
/// `config-upsert(draft: Value)` 的 params 是 draft 对象，
/// `qr-code-generate(ttl: u64)` 的 params 是裸 `300`；传 `{"ttl":300}` 会被
/// 插件侧报 `invalid params`（且回复走 error 通道，宿主侧表现为超时）。
///
/// 无头集成测试没有前端命令通道，播种插件侧数据（如会话配置 / QR token）的唯一路径。
pub async fn plugin_api_call(api: &str, params: serde_json::Value) -> serde_json::Value {
    let host_ctx = AppContext::global().plugin_host().wasm_host_ctx().clone();
    let topic = format!("wasmnest.api.{api}");
    let method = api.rsplit_once('.').map(|(_, m)| m).unwrap_or(api);
    let id = format!(
        "crossend-req-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    });
    let reply_json = host_ctx
        .call_plugin_api_host(&topic, &payload.to_string(), 5000)
        .unwrap_or_else(|e| panic!("plugin api '{api}' failed: {e}"));
    let reply: serde_json::Value = serde_json::from_str(&reply_json).expect("reply json");
    if let Some(err) = reply.get("error") {
        panic!("plugin api '{api}' error: {err}");
    }
    reply.get("result").cloned().expect("result")
}

/// 驱动插件 **Rust 命令面**（= 桌面宿主前端那条路，与 `plugin_invoke` 同一下游）
///
/// 走 `PluginHost::invoke_rust_command`（`api_bridge::plugin_invoke` 的下游，
/// 去掉的是 webview + 凭证层——那是身份管道，不是输出管道）。因此本函数能
/// 在无头场景里扮演**桌面终端预览消费者**（`session.output.pull` /
/// `session.output.ack`），与移动端 WS 消费者**共享同一个 PTY 输出环**。
///
/// 这是「桌面端 + 移动端同时看同一个终端」唯一能真实构造的位置：两个消费者
/// 的帧面根本不同（命令面 JSON vs WS 二进制帧），任何单端 mock 都拼不出来。
pub async fn plugin_command(plugin_id: &str, command: &str, args: serde_json::Value) -> serde_json::Value {
    try_plugin_command(plugin_id, command, args)
        .await
        .unwrap_or_else(|e| panic!("plugin command '{plugin_id}::{command}' failed: {e}"))
}

/// [`plugin_command`] 的可失败版（长循环里用：错误要能被断言到，不得靠 panic 传递）
pub async fn try_plugin_command(
    plugin_id: &str,
    command: &str,
    args: serde_json::Value,
) -> Result<serde_json::Value, String> {
    AppContext::global()
        .plugin_host()
        .invoke_rust_command(plugin_id, command, args)
        .await
        .map_err(|e| e.to_string())
}

/// 播种一条可直接启动的会话配置（bash，工作目录临时目录），返回 config id
pub async fn seed_shell_config(name: &str) -> String {
    let config = plugin_api_call(
        "com.wasmnest.bedcode.config-upsert",
        serde_json::json!({
            "name": name,
            "environment": "linux",
            "workingDir": std::env::temp_dir().to_string_lossy(),
            "command": "bash",
        }),
    )
    .await;
    config["id"].as_str().expect("config-upsert must return id").to_string()
}

/// 场景收尾：清理临时目录（随包产物目录 `resources/plugins/desktop` 不得触碰）
pub fn cleanup_temp_dirs() {
    let _ = std::fs::remove_dir_all(user_plugins_dir());
    let _ = std::fs::remove_dir_all(plugin_db_root());
    let _ = std::fs::remove_dir_all(link_crypto_dir());
}
