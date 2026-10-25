//! 桌面进程驱动（两进程 rig 的测试侧）
//!
//! spawn `wasmnest-crossend-desktop-harness` 二进制（真实桌面服务器进程，独立包/workspace
//! 成员，随包产物 + 真 Actix + 真 PTY），经 stdin/stdout JSON-RPC 控制面驱动：种子会话
//! 配置 / 插件互调与命令面 / WS 端点观测 / 链路加密开关与计数 / 优雅关停。协议见
//! `wasmnest-test/desktop-harness/src/lib.rs` 的 `rpc_dispatch` 与
//! `.scratch/2026-10-15-cross-end-two-process/spec.md`。
//!
//! 与 `tests/common/desktop_ctx.rs`（单进程 rig 的进程内直调面）对应：本文件是同一套
//! 测试 API 的**跨进程版**（每个 helper 一次 rpc 往返）。**只在本包 `two-proc` 构建
//! 模式下编译**（桌面 lib 出图 ⇒ 移动宿主域实现无竞争安装——单进程 rig 装不下的那半）。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};

/// 认证中心 / 会话真源插件 id（与 desktop_ctx / harness 同值）
pub const SESSION_PLUGIN_ID: &str = "com.wasmnest.bedcode";

/// 单次 rpc 往返预算（桌面进程初始化 + cranelift AOT 编译可达数秒）
const RPC_TIMEOUT: Duration = Duration::from_secs(30);

/// 轮询判据预算（与桌面端 desktop_ctx 的 WAIT_TIMEOUT 同口径）
const WAIT_TIMEOUT: Duration = Duration::from_secs(15);

/// harness 二进制路径（workspace 共享 target 桶的落点；可用环境变量覆盖）
///
/// 两进程模式不构建它（`required-features = ["desktop"]` 的 bin 在 `two-proc` 模式下
/// 被跳过）——由先跑的 default 模式构建（`cargo build -p wasmnest-crossend-desktop-harness`）。
/// 缺失时**显性失败**（不是 skip）：跨端互连的唯一驱动方式就是真进程。
fn harness_bin_path() -> PathBuf {
    if let Ok(p) = std::env::var("WASMNEST_CROSSEND_DESKTOP_BIN") {
        return PathBuf::from(p);
    }
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/debug/wasmnest-crossend-desktop-harness");
    assert!(
        p.exists(),
        "desktop harness 二进制缺失：{}。先构建（default 模式）：`cd wasmnest-test && cargo build -p wasmnest-crossend-desktop-harness`",
        p.display()
    );
    p
}

/// 桌面进程句柄（stdin/stdout JSON-RPC；串行使用）
pub struct DesktopHarness {
    child: Child,
    reader: BufReader<ChildStdout>,
    stdin: ChildStdin,
    stderr: Arc<Mutex<String>>,
    /// 桌面服务器端口（harness READY 报告，测试不猜端口）
    pub port: u16,
    next_id: u64,
}

impl DesktopHarness {
    /// spawn 桌面进程并等待 READY（30s 预算；失败 dump stderr 现场）
    pub async fn spawn_and_ready() -> Self {
        let bin = harness_bin_path();
        let mut child = tokio::process::Command::new(&bin)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn desktop harness");
        let stderr = Arc::new(Mutex::new(String::new()));
        {
            let err = child.stderr.take().expect("stderr piped");
            let buf = stderr.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(err);
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line).await {
                        Ok(0) | Err(_) => break,
                        Ok(_) => buf.lock().unwrap_or_else(|p| p.into_inner()).push_str(&line),
                    }
                }
            });
        }
        let stdout = child.stdout.take().expect("stdout piped");
        let stdin = child.stdin.take().expect("stdin piped");
        let mut harness = Self {
            child,
            reader: BufReader::new(stdout),
            stdin,
            stderr,
            port: 0,
            next_id: 0,
        };
        let mut line = String::new();
        tokio::time::timeout(RPC_TIMEOUT, harness.reader.read_line(&mut line))
            .await
            .unwrap_or_else(|_| panic!("等待桌面进程 READY 超时；stderr={}", harness.stderr_dump()))
            .unwrap_or_else(|e| panic!("读取桌面进程 READY 失败：{e}；stderr={}", harness.stderr_dump()));
        let line = line.trim().to_string();
        assert!(
            line.starts_with("READY "),
            "桌面进程未就绪：{line:?}；stderr={}",
            harness.stderr_dump()
        );
        harness.port = line["READY ".len()..]
            .trim()
            .parse()
            .expect("READY 端口解析失败");
        harness
    }

    /// 攒下来的桌面进程 stderr（失败现场用）
    fn stderr_dump(&self) -> String {
        self.stderr.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// 一次 JSON-RPC 往返（串行使用；error 通道 → panic 带现场）
    pub async fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        let req = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let mut line = serde_json::to_string(&req).expect("serialize rpc") + "\n";
        tokio::time::timeout(RPC_TIMEOUT, self.stdin.write_all(line.as_bytes()))
            .await
            .unwrap_or_else(|_| panic!("rpc {method} 写入超时；stderr={}", self.stderr_dump()))
            .expect("write rpc");
        self.stdin.flush().await.expect("flush rpc");
        let deadline = Instant::now() + RPC_TIMEOUT;
        let mut buf = String::new();
        loop {
            assert!(
                Instant::now() < deadline,
                "rpc {method} 响应超时；stderr={}",
                self.stderr_dump()
            );
            match tokio::time::timeout(RPC_TIMEOUT, self.reader.read_line(&mut buf)).await {
                Ok(Ok(0)) => panic!(
                    "rpc {method}: 桌面进程 stdout EOF（进程已退出？）；stderr={}",
                    self.stderr_dump()
                ),
                Ok(Ok(_)) => {}
                Ok(Err(e)) => panic!("rpc {method}: 读响应失败：{e}；stderr={}", self.stderr_dump()),
                Err(_) => panic!("rpc {method}: 读响应超时；stderr={}", self.stderr_dump()),
            }
            let resp: Value = match serde_json::from_str(buf.trim()) {
                Ok(v) => v,
                Err(e) => panic!("rpc {method}: 响应非 JSON：{e}（{buf}）；stderr={}", self.stderr_dump()),
            };
            if resp["id"].as_u64() == Some(id) {
                if let Some(err) = resp.get("error") {
                    panic!("rpc {method} 错误：{err}；stderr={}", self.stderr_dump());
                }
                return resp.get("result").cloned().unwrap_or(Value::Null);
            }
            buf.clear();
        }
    }

    // ==================== 高层 helpers（镜像 desktop_ctx 的测试 API） ====================

    /// 播种一条可直接启动的会话配置（bash，工作目录临时目录），返回 config id
    pub async fn seed_shell_config(&mut self, name: &str) -> String {
        let r = self.rpc("seed-shell-config", json!({ "name": name })).await;
        r["id"].as_str().expect("seed-shell-config must return id").to_string()
    }

    /// 插件 WS 端点当前在线连接摘要（含认证态）
    pub async fn ws_endpoint_clients(&mut self, plugin_id: &str, path: &str) -> Vec<Value> {
        let r = self
            .rpc("ws-endpoint-clients", json!({ "pluginId": plugin_id, "path": path }))
            .await;
        r.as_array().expect("ws-endpoint-clients must return array").clone()
    }

    /// 轮询直到该端点上有**首帧认证已通过**的连接（超时 panic 并打印现场）
    ///
    /// 这是「事件通道/终端就绪」的无竞态判据：插件侧广播前先看 `clientCount > 0`，
    /// 而认证态置位才意味着这条连接真的会收到广播帧（语义与 desktop_ctx 同款）。
    pub async fn wait_ws_endpoint_authenticated(&mut self, plugin_id: &str, path: &str, what: &str) -> Value {
        let deadline = Instant::now() + WAIT_TIMEOUT;
        loop {
            let clients = self.ws_endpoint_clients(plugin_id, path).await;
            if let Some(c) = clients.iter().find(|c| c["authenticated"] == true) {
                return c.clone();
            }
            if Instant::now() >= deadline {
                panic!("timed out waiting for {what}; clients={clients:#?}; stderr={}", self.stderr_dump());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// 轮询直到该端点上出现一条**与给定连接不同**且已认证的连接（超时 panic）
    ///
    /// 自愈类断言用「换了连接」而不是「曾经空过」：实测监督任务重连快到毫秒级，
    /// 任何轮询间隔都观察不到中间空窗（语义与 desktop_ctx 同款）。
    pub async fn wait_new_ws_endpoint_client(
        &mut self,
        plugin_id: &str,
        path: &str,
        previous_client_id: &str,
        what: &str,
    ) -> Value {
        let deadline = Instant::now() + WAIT_TIMEOUT;
        loop {
            let clients = self.ws_endpoint_clients(plugin_id, path).await;
            if let Some(c) = clients.iter().find(|c| {
                c["authenticated"] == true && c["clientId"].as_str() != Some(previous_client_id)
            }) {
                return c.clone();
            }
            if Instant::now() >= deadline {
                panic!("timed out waiting for {what}; clients={clients:#?}; stderr={}", self.stderr_dump());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// 同上的「反例」版：给定预算内**不得**出现新连接（返回是否真的保持静默）
    pub async fn assert_no_new_ws_endpoint_client(
        &mut self,
        plugin_id: &str,
        path: &str,
        known_client_id: &str,
        what: &str,
        budget: Duration,
    ) {
        let deadline = Instant::now() + budget;
        loop {
            let clients = self.ws_endpoint_clients(plugin_id, path).await;
            if let Some(c) = clients.iter().find(|c| {
                c["authenticated"] == true && c["clientId"].as_str() != Some(known_client_id)
            }) {
                panic!("{what}：不该出现的重连真的发生了，新连接 = {c}；stderr={}", self.stderr_dump());
            }
            if Instant::now() >= deadline {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// 桌面侧主动断开该端点全部连接（返回断开条数）
    pub async fn disconnect_ws_endpoint(&mut self, plugin_id: &str, path: &str, code: u16, reason: &str) -> usize {
        let r = self
            .rpc(
                "disconnect-ws-endpoint",
                json!({ "pluginId": plugin_id, "path": path, "code": code, "reason": reason }),
            )
            .await;
        r["count"].as_u64().expect("count").try_into().expect("count usize")
    }

    /// 驱动插件 Rust 命令面（= 桌面 UI 那条路；返回命令结果 JSON）
    pub async fn plugin_command(&mut self, plugin_id: &str, command: &str, args: Value) -> Value {
        self.rpc(
            "plugin-command",
            json!({ "pluginId": plugin_id, "command": command, "args": args }),
        )
        .await
    }

    /// 宿主 → 插件互调（播种插件侧数据的唯一路径；params = api 入参值本身）
    pub async fn plugin_api_call(&mut self, api: &str, params: Value) -> Value {
        self.rpc("plugin-api-call", json!({ "api": api, "params": params })).await
    }

    /// 链路加密计数器快照（只读）
    pub async fn link_crypto_counters(&mut self) -> (u64, u64, u64) {
        let r = self.rpc("link-crypto-counters", json!({})).await;
        let a = r.as_array().expect("link-crypto-counters must return array");
        (
            a[0].as_u64().unwrap_or(0),
            a[1].as_u64().unwrap_or(0),
            a[2].as_u64().unwrap_or(0),
        )
    }

    /// 优雅关停（rpc shutdown → 等进程退出 → 超时强杀）
    pub async fn shutdown(&mut self) {
        let _ = self.rpc("shutdown", json!({})).await;
        match tokio::time::timeout(Duration::from_secs(10), self.child.wait()).await {
            Ok(Ok(status)) => {
                if !status.success() {
                    tracing::warn!("desktop harness 退出码非零：{status}");
                }
            }
            Ok(Err(e)) => panic!("wait desktop harness failed: {e}"),
            Err(_) => {
                tracing::warn!("desktop harness 未在超时内退出，强杀");
                let _ = self.child.kill();
                let _ = self.child.wait().await;
            }
        }
    }
}

impl Drop for DesktopHarness {
    fn drop(&mut self) {
        // panic 路径兜底：直接杀掉（测试进程也要崩了，不需要等优雅退出）
        let _ = self.child.kill();
    }
}
