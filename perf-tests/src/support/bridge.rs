//! 桥接基准 harness · 环境与测量支撑
//!
//! 职责：
//! 1. 编译 `packages/plugin-bench-test` 夹具（wasm32-wasip3，共享 target 目录）；
//! 2. 把产物**摆成两个 wasm 应用目录**（主实例 + 对端实例，同一组件两个属主），
//!    交给生产同形的 `PluginHost` 文件扫描 → 实例化；
//! 3. 起一个本地 HTTP 夹具服务器（`host-http` 客户端域的被测对端）；
//! 4. 提供 `call` / `call_timed` 两个取数入口（统一走 `invoke_rust_command`）。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use serde_json::json;
use wasmnest_desktop_lib::wasm_core::storage::PluginStorage;
use wasmnest_desktop_lib::wasm_core::PluginHost;
use wasmnest_wasm_core::config::CallModel;

/// bench 夹具的 wasm 目标三元组（与其它 fixture 一致）
const WASM_TARGET: &str = "wasm32-wasip3";
/// 产物 profile（fixture 一律 release，`opt-level="s" / lto=true`）
const WASM_PROFILE: &str = "release";
/// 夹具 crate 名（cdylib 产物名 = 包名下划线形式）
pub(crate) const FIXTURE_LIB: &str = "wasmnest_plugin_bench_test";
/// wasip3 工具链 pin（单一事实来源 =
/// `../../wasmnest-lib/wasmnest-wasm-core/src/manager/runtime.rs::WASIP3_NIGHTLY`，
/// 与 `packages/.cargo/config.toml` 注释互指；可用环境变量覆盖以便跟随工具链迁移）
const WASIP3_TOOLCHAIN: &str = "nightly-2026-09-16";

/// 主实例插件 id（发压方：发命令、发事件、发总线消息、发起互调）
pub const MAIN_ID: &str = "com.wasmnest.bench";
/// 对端实例插件 id（受压方：订阅总线、应答互调、提供收讫计数）
pub const PEER_ID: &str = "com.wasmnest.bench.peer";
/// 总线 JSON 通道（主 → 对端）
pub const BUS_JSON_TOPIC: &str = "bench.probe.json";
/// 总线二进制通道（主 → 对端）
pub const BUS_BIN_TOPIC: &str = "bench.probe.bin";

/// 基准环境：插件宿主 + 夹具服务器 + 临时目录
pub struct BenchEnv {
    host: Arc<PluginHost>,
    /// HTTP 夹具服务器端口（`/bytes?n=<字节数>` 返回定长响应体）
    pub http_port: u16,
    /// fs 授权目录（host-fs 场景用；已预授权进 `fs_granted_paths`）
    pub fs_dir: PathBuf,
    temp_root: PathBuf,
    _server_task: tokio::task::JoinHandle<()>,
}

impl BenchEnv {
    /// 场景执行期的自述（报告头部用）
    pub fn describe(&self) -> String {
        format!(
            "主实例 {MAIN_ID} + 对端实例 {PEER_ID}，HTTP 夹具 :{}，临时根 {}",
            self.http_port,
            self.temp_root.display()
        )
    }

    /// 宿主句柄（M2 队列饱和场景需要并发投递 + 可被 move 的任务）
    pub fn host(&self) -> Arc<PluginHost> {
        Arc::clone(&self.host)
    }

    /// 临时根（M3 异步进程输出路径等）
    pub fn temp_root(&self) -> &Path {
        &self.temp_root
    }

    /// 切换主实例调用模型（`Mutex` / `EventLoop`）。
    ///
    /// 调用模型是**实例级快照**，热切不会生效；这里按生产重建路径走：
    /// 停用 → 从同一产物重新加载实例 → 按目标模型装表 → 重新激活。
    /// 对端实例不动，避免把「模型切换」和「夹具重启」混成一个变量。
    pub async fn switch_main_model(&self, model: CallModel) -> anyhow::Result<()> {
        let _ = self.host.deactivate_plugin(MAIN_ID, false).await;

        let wasm_path = self
            .temp_root
            .join("plugins")
            .join(MAIN_ID)
            .join(format!("{FIXTURE_LIB}.wasm"));
        let plugin = self
            .host
            .wasm_runtime()
            .load_plugin_from_file(
                &wasm_path,
                MAIN_ID,
                Arc::clone(self.host.wasm_host_ctx()),
                &[],
                None,
            )
            .map_err(|e| anyhow::anyhow!("重新加载主实例失败: {e}"))?;
        self.host.install_instance(MAIN_ID, plugin, model).await;
        self.host
            .activate_plugin(MAIN_ID, false)
            .await
            .map_err(|e| anyhow::anyhow!("按 {model:?} 模型重新激活主实例失败: {e}"))?;
        Ok(())
    }

    /// 经生产同形命令面下发一条命令（`plugin_invoke` 的内核：`invoke_rust_command`）
    pub async fn call(
        &self,
        plugin_id: &str,
        command: &str,
        args: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        self.host
            .invoke_rust_command(plugin_id, command, args)
            .await
            .map_err(|e| anyhow::anyhow!("invoke {plugin_id}/{command} 失败: {e}"))
    }

    /// 主实例命令（发压方；`call` 的省略 plugin_id 版）
    pub async fn call_main(&self, command: &str, args: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        self.call(MAIN_ID, command, args).await
    }

    /// 主实例命令 + 墙钟计时（微秒精度，返回 (结果, 耗时)）
    pub async fn call_timed(
        &self,
        command: &str,
        args: serde_json::Value,
    ) -> anyhow::Result<(serde_json::Value, std::time::Duration)> {
        let t0 = Instant::now();
        let value = self.call(MAIN_ID, command, args).await?;
        Ok((value, t0.elapsed()))
    }

    /// 对端实例命令（读收讫计数等）
    pub async fn call_peer(&self, command: &str, args: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        self.call(PEER_ID, command, args).await
    }

    /// 轮询对端收讫计数直到达到 `expect`（总线投递是异步派发，读数必须等）
    ///
    /// 返回达到时的计数；超时抛错（带实际值，便于定位是“没投到”还是“投到但慢”）。
    pub async fn wait_peer_recv(&self, field: &str, expect: usize, timeout_ms: u64) -> anyhow::Result<usize> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
        loop {
            let recv = self.call_peer("bench.bus-recv", json!({})).await?;
            let got = recv.get(field).and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            if got >= expect {
                return Ok(got);
            }
            anyhow::ensure!(
                std::time::Instant::now() < deadline,
                "对端 {field} 收讫超时: {got} < {expect}（{timeout_ms} ms）"
            );
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
    }

    /// HTTP 夹具地址：`/bytes?n=<n>` 返回恰好 n 字节的响应体
    pub fn http_bytes_url(&self, n: usize) -> String {
        format!("http://127.0.0.1:{}/bytes?n={n}", self.http_port)
    }

    /// 停用插件并清理临时目录（不留后台任务 / 监听端口 / 临时库）
    pub async fn shutdown(self) {
        for id in [MAIN_ID, PEER_ID] {
            let _ = self.host.deactivate_plugin(id, false).await;
        }
        let _ = std::fs::remove_dir_all(&self.temp_root);
    }
}

/// 构建基准环境：编译夹具 → 摆应用目录 → 建 PluginHost → 激活双实例 → 起 HTTP 夹具
pub async fn build_env(full: bool) -> anyhow::Result<BenchEnv> {
    let temp_root = std::env::temp_dir().join(format!(
        "wasmnest_bridge_bench_{}_{}",
        std::process::id(),
        Instant::now().elapsed().as_nanos() as u64
            ^ std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos() as u64)
                .unwrap_or(0)
    ));
    std::fs::create_dir_all(&temp_root)?;

    build_fixture_component()?;

    // 插件目录落临时根（不写平台仓 target/bench）：基准数据纪律与跨端测试一致
    let plugins_dir = temp_root.join("plugins");
    stage_app_dirs(&plugins_dir)?;

    // 与 apps 模块同一份装配（临时内核库 / 用户插件目录 / 插件私有库根）
    let assembled = crate::support::host::assemble(&temp_root, &plugins_dir, Vec::new()).await?;
    let host = assembled.host;
    let db = assembled.db;

    // host-fs 场景需要已授权前缀：按 `fs_auth` 的持久化键（`fs_granted_paths`）
    // 直接种进插件存储——无头上下文没有 AppHandle，弹窗层必然拒绝（生产同款约束）
    let fs_dir = temp_root.join("fs");
    std::fs::create_dir_all(&fs_dir)?;
    let storage = PluginStorage::new(db.clone());
    storage
        .set(
            MAIN_ID,
            "fs_granted_paths",
            serde_json::json!([fs_dir.to_string_lossy()]),
        )
        .await?;
    storage
        .set(
            PEER_ID,
            "fs_granted_paths",
            serde_json::json!([fs_dir.to_string_lossy()]),
        )
        .await?;

    for id in [MAIN_ID, PEER_ID] {
        host.activate_plugin(id, false)
            .await
            .map_err(|e| anyhow::anyhow!("激活 {id} 失败: {e}"))?;
    }

    // 对端进入「服务方」角色：应答互调 + 订阅两条总线通道（总线不投递给发送者自身，
    // 故受压计数只能在**对端**读）
    host_call(&host, PEER_ID, "bench.api-serve", serde_json::json!({ "enable": true })).await?;
    host_call(
        &host,
        PEER_ID,
        "bench.bus-subscribe",
        serde_json::json!({ "topic": BUS_JSON_TOPIC, "binary": false }),
    )
    .await?;
    host_call(
        &host,
        PEER_ID,
        "bench.bus-subscribe",
        serde_json::json!({ "topic": BUS_BIN_TOPIC, "binary": true }),
    )
    .await?;

    // 冒烟探针：链路（guest 可调用、命令面通）不通就直接失败，别浪费时间跑场景
    let (pong, _) = {
        let t0 = Instant::now();
        let v = host_call(&host, MAIN_ID, "bench.ping", serde_json::json!({})).await?;
        (v, t0.elapsed())
    };
    if pong.get("pong").and_then(|v| v.as_bool()) != Some(true) {
        anyhow::bail!("bench 夹具 ping 失败：{pong}");
    }
    if full {
        println!("[bench] 夹具就绪：ping 往返 {:?}", pong);
    }

    let (port, task) = start_http_fixture().await?;

    // host-http 场景：给夹具 origin 落一条 allow 授权记录（票 05 出站授权）。
    // 无头上下文没有 AppHandle → 弹窗层必然拒绝（与上面 fs 授权同款约束）：
    // 基准要测的是桥接开销，不该被授权弹窗拦在门外
    let fixture_origin = format!("http://127.0.0.1:{port}");
    let auth_store = wasmnest_desktop_lib::wasm_core::security::auth_policy::AuthPolicyStore::new(db.clone());
    for id in [MAIN_ID, PEER_ID] {
        auth_store
            .grant(
                id,
                wasmnest_desktop_lib::wasm_core::security::auth_policy::AuthResource::Network,
                &fixture_origin,
                &[],
                wasmnest_desktop_lib::wasm_core::security::auth_policy::AuthRecordSource::User,
            )
            .await?;
    }

    Ok(BenchEnv {
        host,
        http_port: port,
        fs_dir,
        temp_root,
        _server_task: task,
    })
}

/// 直调 PluginHost 命令面（启动期专用；场景内统一走 `BenchEnv::call`）
async fn host_call(
    host: &Arc<PluginHost>,
    plugin_id: &str,
    command: &str,
    args: serde_json::Value,
) -> anyhow::Result<serde_json::Value> {
    host.invoke_rust_command(plugin_id, command, args)
        .await
        .map_err(|e| anyhow::anyhow!("启动期 {plugin_id}/{command} 失败: {e}"))
}

// ==================== 夹具构建 ====================

/// 桌面端工程根（`<platform>/wasmnest-desktop`）；跨仓路径统一走 resolver。
fn desktop_root() -> PathBuf {
    crate::support::repo_paths::desktop_root()
}

/// 夹具共享 target 目录：落本工程 target 桶，不写平台仓 target。
fn fixtures_target_dir() -> PathBuf {
    crate::support::repo_paths::perf_target_dir().join("fixtures")
}

/// 编译 bench 夹具组件（源码/SDK/WIT 变更即重建；产物 mtime 判定）
///
/// 规模边界模块复用同一份产物：同一组件多属主是「N 个应用同时常驻」最便宜的
/// 合成形态，且不会被真实业务数据增长干扰。
pub(crate) fn build_fixture_component() -> anyhow::Result<PathBuf> {
    let toolchain = std::env::var("BENCH_WASIP3_TOOLCHAIN").unwrap_or_else(|_| WASIP3_TOOLCHAIN.to_string());
    let packages_dir = desktop_root().join("packages");
    let plugin_dir = packages_dir.join("plugin-bench-test");
    let artifact = fixtures_target_dir()
        .join(WASM_TARGET)
        .join(WASM_PROFILE)
        .join(format!("{FIXTURE_LIB}.wasm"));

    let watch = [
        plugin_dir.join("src/lib.rs"),
        plugin_dir.join("plugin.json"),
        plugin_dir.join("Cargo.toml"),
        packages_dir.join("plugin-sdk-desktop/rust/src/wasm_host.rs"),
        packages_dir.join("plugin-sdk-desktop/rust/src/wasm.rs"),
        packages_dir.join("plugin-sdk-desktop/rust/src/api_call.rs"),
        packages_dir.join("plugin-sdk-desktop/rust/wit/wasmnest.wit"),
    ];

    let needs_build = match std::fs::metadata(&artifact).and_then(|m| m.modified()) {
        Ok(built_at) => watch.iter().any(|f| {
            std::fs::metadata(f)
                .and_then(|m| m.modified())
                .map(|t| t > built_at)
                .unwrap_or(true)
        }),
        Err(_) => true,
    };

    if needs_build {
        println!(
            "[bench] 编译夹具 {}（{WASM_TARGET}/{WASM_PROFILE}, toolchain={toolchain}）…",
            FIXTURE_LIB
        );
        let mut cargo = std::process::Command::new("cargo");
        cargo
            .env("RUSTUP_TOOLCHAIN", &toolchain)
            .env("CARGO_TARGET_DIR", fixtures_target_dir());
        // apps 模块会把进程 HOME 指向沙箱；夹具编译必须用用户真实 HOME
        // （cargo/rustup 缓存都在那里）
        if let Some(home) = crate::support::home::original_home() {
            cargo.env("HOME", home);
        }
        let status = cargo
            .args([
                "build",
                "--target",
                WASM_TARGET,
                "--release",
                "--manifest-path",
                plugin_dir.join("Cargo.toml").to_str().unwrap(),
            ])
            .status()?;
        anyhow::ensure!(status.success(), "夹具编译失败（exit={status}）");
    }

    anyhow::ensure!(artifact.exists(), "夹具产物缺失: {}", artifact.display());
    Ok(artifact)
}

/// 把夹具产物摆成两个 wasm 应用目录（同名组件、两个属主）
///
/// 目录名必须 = manifest id（loader 的身份校验，见 `manager/loader.rs`），
/// 故对端目录用改写 id 的清单副本。
fn stage_app_dirs(plugins_dir: &Path) -> anyhow::Result<()> {
    let artifact = build_fixture_component()?;
    let manifest_src = desktop_root().join("packages/plugin-bench-test/plugin.json");
    let manifest: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&manifest_src)?)?;

    for id in [MAIN_ID, PEER_ID] {
        let dir = plugins_dir.join(id);
        std::fs::create_dir_all(&dir)?;
        let mut m = manifest.clone();
        m["id"] = serde_json::Value::String(id.to_string());
        if id == PEER_ID {
            m["name"] = serde_json::Value::String("Bridge Bench Probe (peer)".to_string());
        } else {
            // 主实例是 api-call 的**调用方**（服务方是对端 peer）：不声明互调 api。
            // 两实例同源清单原来靠「后登记覆盖」共存（同名 api 最后登记方为属主），
            // 注册表 fail-closed 后（api_registry S-01）冲突登记会使后激活实例
            // 整体激活失败——按角色归属声明才是新语义下的正确形态
            m["api"] = serde_json::json!([]);
        }
        std::fs::write(dir.join("plugin.json"), serde_json::to_string_pretty(&m)?)?;
        std::fs::copy(&artifact, dir.join(format!("{FIXTURE_LIB}.wasm")))?;
        // 前端入口占位：rust-ts 插件的 `main` 字段指向它；harness 只跑命令面，
        // 不加载前端（webview 层由 e2e 单独覆盖）
        std::fs::write(dir.join("index.js"), "// bench fixture: 命令面专用，前端为空壳\n")?;
    }
    Ok(())
}

// ==================== HTTP 夹具服务器 ====================

/// 启动最小 HTTP 服务器：`/bytes?n=<n>` → 200 + `Content-Length: n` + n 字节响应体
///
/// 只为给 `host-http` 客户端域一个确定性的对端（AI 供应商响应 / 资源下载的形态）；
/// 用裸 TcpListener 手写响应，避免为了基准再拉一个 web 框架依赖。
async fn start_http_fixture() -> anyhow::Result<(u16, tokio::task::JoinHandle<()>)> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let Ok(n) = sock.read(&mut buf).await else { return };
                let request = String::from_utf8_lossy(&buf[..n]).to_string();
                let n_param = request
                    .split("n=")
                    .nth(1)
                    .and_then(|s| s.split(|c: char| !c.is_ascii_digit()).next())
                    .and_then(|s| s.parse::<usize>().ok())
                    .unwrap_or(1024);
                let body = vec![b'a'; n_param];
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(head.as_bytes()).await;
                let _ = sock.write_all(&body).await;
                let _ = sock.flush().await;
            });
        }
    });
    Ok((port, task))
}
