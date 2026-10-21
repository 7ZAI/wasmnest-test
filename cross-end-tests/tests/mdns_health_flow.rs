//! 场景 11：mDNS 发现/广播 + `/api/health` 探测（用户「发现并连上桌面」的入口）
//!
//! 覆盖的盲区：**连接建立之前的整条链路**。既有场景全部靠 `set_target` 直连绕过：
//! 用户真实路径是「扫到局域网里的桌面 → 看一眼 health → 连上去」，这三步此前
//! 从未被两端同时跑过。断了的症状非常安静：移动端「扫不到设备」列表空着，
//! 桌面端一切正常，没有任何错误可查。
//!
//! ## 为什么这个场景必须走 supervisor 启动（而不是 `start_server`）
//!
//! 服务器生命周期的**主人**是 `wasmnest_server_core::supervisor::ServerSupervisor`：
//! 它把端口写进自己的状态（`/api/health` 的 `port` 取自这里）、启动 mDNS 广播
//! （`supervisor.rs:231`）、重置指标。GUI bootstrap 走的正是
//! `init_config` → `ws_manager.init()` → `supervisor.start(port)`。
//! 而 rig 常用的 `composition::start_http_server` → `app::serve` **绕过 supervisor**：
//! 既不广播（票面观察到的现象），`/api/health` 还会报默认端口 8765 而不是真实端口。
//! 本文件因此走 supervisor 同款启动，断言的是生产真实发生的那三件事。
//!
//! ## 环境要求：同机不得有第二个 WasmNest 桌面实例在跑
//!
//! 本场景用 mDNS 做「发现」，而 mDNS 的**实例名** = `WasmNest-<device_name>`，
//! `device_name` 取系统信息端口（宿主壳代理 = 机器名）。同机跑着**真桌面端**
//! （`pnpm run tauri:dev` 常态）时，它广播的实例名与本 rig **逐字相同**：
//!
//! 1. 两个应答者抢同一实例名 → RFC 6762 冲突处理把**后到的一方改名**
//!    （rig 侧实测被改成 `WasmNest-<host> (2)._wasmnest._tcp.local.`）；
//! 2. 移动端发现缓存按 `instance_name` 建 `HashMap`（`MdnsDiscovery::services`），
//!    同名记录**互相覆盖**——先到先得，后到覆盖先到；
//! 3. 两个记录的 TXT 逐字相同（`device_name` / `platform` / `version` 全一致，
//!    改名只改实例名不改 TXT），故**按 TXT 挑不出哪条是 rig 自己的**。
//!
//! 后果：本场景在共享主机上**竞态 flaky**（实测同机复跑：一次红、一次绿）——
//! 绿的那次是 rig 的记录先落进缓存、红的那次是真桌面端的记录覆盖了它。
//! 这不是协议缺陷：rig 自己的广播记录本身完全正确
//! （实测 `port=<rig 端口> / platform=desktop / device_name=WasmNest-<host> / version=<应用版本>`）。
//!
//! **判定口径**：本场景要求**同机无其它 WasmNest 桌面实例在跑**
//! （CI / 干净开发机满足；共享开发机请先关掉桌面端，或用网络命名空间隔离——
//! 隔离后组播不跨命名空间，真桌面端的广播看不见，rig 自发自收）：
//!
//! ```bash
//! cargo test --test mdns_health_flow --no-run      # 取已构建的测试二进制路径
//! unshare -rn bash -c \
//!   'ip link set lo up; ip link set lo multicast on; ./target/debug/deps/mdns_health_flow-<hash>'
//! ```
//!
//! （实测：同一二进制在隔离命名空间下稳定通过。）
//!
//! ⚠ **不得**为了「在共享主机上稳定绿」而放宽本文件的断言
//! （例如把记录选择判据从 TXT 换成 rig 端口）——那会让端口断言退化为
//! 选择条件、失去钉住力。共享主机的正解是**隔离环境**（上条命令），
//! 不是改断言。

//! ## 行为契约（unit-test-discipline G1：每条有来源）
//!
//! | 契约 | 来源（代码证据） | 行为 | 场景 |
//! |---|---|---|---|
//! | M-001 | 桌面 `supervisor::start_mdns_advertisement` → `MdnsAdvertiser::start`；移动端 `MdnsDiscovery::start_headless` / `get_services` | 桌面广播 → 移动端真实浏览 `_wasmnest._tcp.local.` → 解析出该服务：`port` 与真实服务器端口逐字一致、`platform == "desktop"`、`device_name` = 实例名、`version` TXT 逐字、`address` 非空且可达、`host_name` 以 `.local.` 结尾 | 正例 |
//! | M-002 | 移动端 `httpProbe`（`useHttpApi.ts:620`）照抄其请求构造：`egress_declare_desktop_target` → `kind="desktop"` 的 `GET /api/health` | 探测真实往返 → 断言 `{status:"ok", port:<真实端口>, uptime_secs:≥0}`（与前端解析的三个字段逐字对齐） | 正例 |
//! | M-003 | 移动端 `MdnsDiscovery::stop` | 停止后移动端发现列表清空（发现链路不留陈旧条目，用户不会连到已下线的机器） | 正例 |

mod common;

use std::time::Duration;

use wasmnest_mobile_lib::mdns::discovery::MdnsDiscovery;
use futures_util::FutureExt;

use common::desktop_ctx;
use common::mobile_ctx;

/// panic 路径也要清临时目录
struct TempDirGuard;

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        desktop_ctx::cleanup_temp_dirs();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mobile_discovers_desktop_over_mdns_and_probes_health() {
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
    let port = desktop_ctx::pick_free_port();
    // 生产同款启动（supervisor 顺带起 mDNS 广播 + 把真实端口写进 /api/health）
    desktop_ctx::start_server_via_supervisor(port).await;
    // 发现器由外层持有：失败路径也要能**正常 stop**（直接 drop 一个运行中的
    // `mdns_sd::ServiceDaemon` 会 join 它的收包线程，失败一轮要等 ~2 分钟）
    let discovery = MdnsDiscovery::new();
    let outcome = std::panic::AssertUnwindSafe(scenario(port, &discovery))
        .catch_unwind()
        .await;
    discovery.stop().await;
    desktop_ctx::stop_server_via_supervisor().await;
    mobile_ctx::clear_identity();
    if let Err(payload) = outcome {
        std::panic::resume_unwind(payload);
    }
}

/// 场景主体（独立成函数：让收尾能在断言失败时也执行，见外层 `catch_unwind`）
async fn scenario(port: u16, discovery: &MdnsDiscovery) {
    mobile_ctx::set_target(port).await;

    // 广播里的服务名 / 版本按 supervisor 的同一份规则算出（`WasmNest-<device>`）
    let info = desktop_ctx::system_info();
    let service_name = format!("WasmNest-{}", info.device_name);
    let expected_version = desktop_ctx::advertised_app_version();

    // ==================== M-001 移动端浏览 ====================
    discovery.start_headless().await.expect("移动端开始浏览 mDNS");
    assert!(discovery.is_scanning().await, "M-001 浏览应处于扫描中");

    // mDNS 是异步解析 + 组播：轮询等待，不定值 sleep（预算同 mobile_ctx::WAIT_TIMEOUT）
    let deadline = std::time::Instant::now() + mobile_ctx::WAIT_TIMEOUT;
    let found = loop {
        let services = discovery.get_services().await;
        if !services.is_empty() {
            break services;
        }
        if std::time::Instant::now() >= deadline {
            panic!(
                "M-001 桌面已广播 {service_name}，移动端 15s 内未发现（若本机/容器无组播则此环境\
                 无法验证，见交付说明）；发现列表={services:#?}"
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let desktop = found
        .iter()
        .find(|s| s.device_name == service_name || s.instance_name.contains(&service_name))
        .unwrap_or_else(|| panic!("M-001 发现列表应含桌面服务 {service_name}，实际={found:#?}"));

    assert_eq!(
        desktop.port, port,
        "M-001 发现到的端口必须与 rig 服务器真实端口逐字一致（用户按这条连）"
    );
    assert_eq!(
        desktop.platform, "desktop",
        "M-001 platform TXT 应为 desktop（移动端据此区分桌面/移动端广播）"
    );
    assert_eq!(
        desktop.device_name, service_name,
        "M-001 device_name TXT 应是广播里的服务名（列表展示名）"
    );
    assert!(
        !desktop.address.is_empty(),
        "M-001 解析出的 address 不得为空（空 = 用户无从连接）"
    );
    assert_eq!(
        desktop.txt_records.get("version").map(String::as_str),
        Some(expected_version.as_str()),
        "M-001 version TXT 应逐字透传桌面**应用**版本（真源 = SystemInfoPort::app_version，\
         不是 SystemInfo::collect().app_version —— 后者是 server-base 包的版本）"
    );
    assert!(
        desktop.host_name.ends_with(".local."),
        "M-001 主机名应为 <实例名>.local.（advertiser 按此注册），got {}",
        desktop.host_name
    );

    // ==================== M-002 /api/health 探测（照抄前端 httpProbe 的请求构造） ====================
    // 前端先声明目标（probe 发生在 ws_connect 之前，ConnectionManager 尚未设目标）
    wasmnest_mobile_lib::commands::egress::egress_declare_desktop_target(desktop.address.clone(), desktop.port)
        .expect("声明探测目标");

    let probe = wasmnest_mobile_lib::commands::http_proxy::execute_proxy(
        wasmnest_mobile_lib::commands::http_proxy::HttpProxyRequest {
            request_id: "m002-probe".to_string(),
            method: "GET".to_string(),
            url: format!("http://{}:{}/api/health", desktop.address, desktop.port),
            headers: Default::default(),
            body: None,
            timeout_ms: Some(3_000), // 前端同值
            kind: Some("desktop".to_string()),
        },
        None,
    )
    .await
    .expect("M-002 /api/health 探测必须往返成功");
    assert_eq!(probe.status, 200, "M-002 探测应 200（body={}）", probe.body_text);
    let health: serde_json::Value = serde_json::from_str(&probe.body_text)
        .unwrap_or_else(|e| panic!("M-002 health 响应应为 JSON，got {:?}（{e}）", probe.body_text));
    assert_eq!(health["status"], "ok", "M-002 status 应为 ok");
    assert_eq!(
        health["port"], desktop.port,
        "M-002 health.port 必须与发现到的端口一致（两端对「连哪个口」达成同一结论）"
    );
    assert!(
        health["uptime_secs"].is_u64(),
        "M-002 uptime_secs 应为无符号整数（前端直接展示），got {}",
        health["uptime_secs"]
    );

    // ==================== M-003 停止后不留陈旧条目 ====================
    // （真正的 stop 在外层收尾里做——失败路径也要停；这里只验「停止后不留陈旧条目」）
    discovery.stop().await.expect("M-003 停止浏览");
    assert!(
        discovery.get_services().await.is_empty(),
        "M-003 停止浏览后不应残留已发现服务（用户会连到已下线的机器）"
    );
}
