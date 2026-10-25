//! 跨端 rig 的移动端**真实插件**装配（2026-10-15 新增）
//!
//! 背景：票 12/15 把终端订阅协议、认证编排与会话 HTTP 客户端整体下沉到移动端
//! wasm 插件（应用仓 `apps/bedcode/mobile`，crate 名
//! `bedcode_plugin_terminal_session`）——宿主 `wasmnest_mobile_lib` 里已没有
//! `terminal_link` / `session::http`。跨端门禁要覆盖「桌面真实服务器 ↔ 移动真实
//! 客户端」，就只能在测试内以**无头宿主上下文**加载**随包真产物**并驱动其命令面
//! （与 `wasmnest/wasmnest-mobile/src-tauri/tests/mobile_plugin_activate_fs_e2e.rs` 同款夹具：
//! `MockPorts` + `setup_headless_runtime_with_config` + 移动 `GuestEventDispatcher`）。
//!
//! 替身口径（**只记录、不伪造**）：`MockPorts` 只供给端事实（主连接目标 = 本进程
//! 桌面服务器、JWT token = 配对真发）；WS 连接 / HTTP 出站 / 页面窄转发 / 权限判定
//! 全是真实宿主实现，桌面端应答全部来自真实桌面插件 + 真实 PTY。
//!
//! 产物缺失**显性失败**（不是 `[skip]`）：本 rig 的唯一驱动方式就是真实产物。

use std::path::PathBuf;
use std::sync::Arc;

use wasmnest_mobile_lib::plugin::guest_events::MobileGuestEvents;
use wasmnest_mobile_lib::plugin::manager::mobile_core_config;
use wasmnest_mobile_lib::plugin::terminal_stream::terminal_stream_gateway;
use wasmnest_wasm_core::bus::{BusMessage, MessageDispatcher};
use wasmnest_wasm_core::host_api::ports::{ConnectionEnginePort, PrimaryTarget};
use wasmnest_wasm_core::host_api::WasmHostContext;
use wasmnest_wasm_core::manager::runtime::{AbiPolicy, LoadedWasmPlugin, WasmRuntime};
use wasmnest_wasm_core::test_support::{
    build_host_ctx_with, runtime_engine, setup_headless_runtime_with_config, MockPorts,
};
use tauri::ipc::{Channel, InvokeResponseBody};
// wasmtime 经 wasm-core re-export（避免本包另锁一份 wasmtime 版本）
use wasmnest_wasm_core::wasmtime::component::Component;

/// 移动端随包插件产物（`pnpm run plugins:build` 落点；相对本包根）
pub const TERMINAL_PLUGIN_WASM: &str =
    "../../wasmnest/wasmnest-mobile/src-tauri/resources/plugins/mobile/com.wasmnest.bedcode/bedcode_plugin_terminal_session.wasm";

/// 终端会话插件 id（两端同 id、职责不同）
pub const TERMINAL_PLUGIN_ID: &str = "com.wasmnest.bedcode";

/// 主连接目标替身：返回本进程桌面服务器地址（`host-connection.primary-target` 真源）
struct DesktopTarget {
    address: String,
    port: u16,
}

#[async_trait::async_trait]
impl ConnectionEnginePort for DesktopTarget {
    async fn primary_target(&self) -> wasmnest_wasm_core::Result<Option<(PrimaryTarget, bool)>> {
        Ok(Some((
            PrimaryTarget {
                address: self.address.clone(),
                port: self.port,
            },
            true,
        )))
    }
}

/// 总线投递器：把 `ws:*` 消息真实投给 guest（锁内串行，避免测试线程与投递任务
/// 并发进同一 Store——wasmtime Store 非 Sync）
struct LockedDispatcher {
    plugin: Arc<tokio::sync::Mutex<LoadedWasmPlugin>>,
}

impl MessageDispatcher for LockedDispatcher {
    fn dispatch_to_wasm(&self, _plugin_id: &str, msg: &BusMessage) -> anyhow::Result<()> {
        let plugin = self.plugin.clone();
        let msg = msg.clone();
        wasmnest_wasm_core::runtime_util::block_on_async(async move {
            let mut plugin = plugin.lock().await;
            if let Some(bytes) = &msg.payload_binary {
                plugin
                    .on_message_binary(&msg.topic, &msg.sender, bytes)
                    .map_err(anyhow::Error::from)
            } else {
                plugin
                    .on_message(&msg.topic, &msg.sender, &msg.payload)
                    .map_err(anyhow::Error::from)
            }
        })
    }

    fn is_activated(&self, _plugin_id: &str) -> bool {
        true
    }
}

/// 移动端插件世界：真产物组件 + 无头宿主上下文 + 指向桌面服务器的端事实
pub struct MobilePluginWorld {
    _aot: PathBuf,
    #[allow(dead_code)] // 保活引擎（component 已编译完成，实例内持引用）
    runtime: WasmRuntime,
    plugin: Arc<tokio::sync::Mutex<LoadedWasmPlugin>>,
    /// 宿主上下文（测试可读权限/网关等宿主面）
    pub host_ctx: Arc<WasmHostContext>,
}

impl MobilePluginWorld {
    /// 加载并激活终端会话插件；`port` = 本进程桌面服务器端口，`token` = 配对真发 token
    pub async fn load_terminal_plugin(port: u16, token: &str) -> Self {
        let wasm = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(TERMINAL_PLUGIN_WASM);
        assert!(
            wasm.exists(),
            "移动端插件产物缺失（{}）：先跑 `cd wasmnest/wasmnest-mobile && pnpm run plugins:build`",
            wasm.display()
        );

        let aot = std::env::temp_dir().join(format!(
            "wasmnest-crossend-mobile-aot-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let ports = MockPorts::new()
            .with_token(token)
            .with_egress_allow()
            .with_connection(Arc::new(DesktopTarget {
                address: "127.0.0.1".to_string(),
                port,
            }));
        let host_ctx = build_host_ctx_with(ports);
        // 生效权限 = manifest 声明面（真机由宿主按 manifest 授予）
        host_ctx.grant_permissions(
            TERMINAL_PLUGIN_ID,
            // 与插件 manifest `permissions` 逐字一致的权限位（真机由宿主按 manifest 授予）
            &[
                "auth".to_string(),
                "bus".to_string(),
                "network:http".to_string(),
                "session:read".to_string(),
                "storage".to_string(),
                "terminal:output".to_string(),
                "ws:client".to_string(),
            ],
        );

        let runtime =
            setup_headless_runtime_with_config(Some(aot.clone()), AbiPolicy { form: None }, mobile_core_config(None))
                .expect("create headless wasm runtime");
        runtime.set_guest_events(Arc::new(MobileGuestEvents));

        let component = Component::from_file(runtime_engine(&runtime), &wasm)
            .expect("移动端插件产物不可编译（先重跑 plugins:build）");
        let plugin = runtime
            .instantiate_component(&component, TERMINAL_PLUGIN_ID, host_ctx.clone(), &[], None)
            .expect("instantiate mobile terminal plugin");
        let plugin = Arc::new(tokio::sync::Mutex::new(plugin));
        host_ctx
            .message_bus
            .set_dispatcher(Arc::new(LockedDispatcher { plugin: plugin.clone() }))
            .await;
        plugin.lock().await.activate().expect("activate mobile terminal plugin");

        Self {
            _aot: aot,
            runtime,
            plugin,
            host_ctx,
        }
    }

    /// 插件命令面调用（返回命令结果 JSON；Err = 命令显性失败）
    pub async fn invoke(&self, command: &str, args: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let out = self
            .plugin
            .lock()
            .await
            .invoke_command(command, &args.to_string())
            .map_err(|e| anyhow::anyhow!("{command} failed: {e}"))?;
        serde_json::from_str(&out).map_err(|e| anyhow::anyhow!("{command} returned non-JSON: {e} ({out})"))
    }

    /// 登记会话页面字节流（宿主窄转发表真表）——等同前端 `openTerminalStream`
    pub fn page_subscribe(&self, session_id: &str, channel: Channel<InvokeResponseBody>) {
        terminal_stream_gateway().page_subscribe(session_id, channel);
    }

    /// 注销页面字节流（退出终端页）
    pub fn page_unsubscribe(&self, session_id: &str) {
        terminal_stream_gateway().page_unsubscribe(session_id);
    }

    /// 宿主窄转发计数（丢弃可观测：与页面收到的字节对账）
    pub fn gateway_stats(&self) -> wasmnest_mobile_lib::plugin::terminal_stream::TerminalStreamStats {
        terminal_stream_gateway().stats()
    }

    /// 链路状态快照（`bedcode.get-state`；前端对账的同一观测面）
    pub async fn state(&self, session_id: &str) -> serde_json::Value {
        self.invoke(
            "bedcode.get-state",
            serde_json::json!({ "sessionId": session_id }),
        )
        .await
        .expect("get-state 必须可调用")
    }

    /// 轮询链路状态直到谓词满足（超时 panic 并打印最后一次快照）
    pub async fn wait_state(
        &self,
        session_id: &str,
        what: &str,
        mut pred: impl FnMut(&serde_json::Value) -> bool,
    ) -> serde_json::Value {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            let state = self.state(session_id).await;
            if pred(&state) {
                return state;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "等待链路状态超时：{what}；最后一次={state}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        }
    }

    /// 等链路进入 live（收 `subscribed`）——订阅/重订阅后的就绪判据
    pub async fn wait_live(&self, session_id: &str) {
        self.wait_state(session_id, "链路 live", |s| s["subscribed"] == true)
            .await;
    }
}
