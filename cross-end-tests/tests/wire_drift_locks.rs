//! wire 契约词汇漂移锁（三仓拆分 L2/E2b · 票 08 自能力仓迁入平台仓；2026-10-18
//! 随跨端测试面再迁出为独立仓 `wasmnest-test`）
//!
//! ## 为什么锁在跨端测试仓、副本在能力仓
//!
//! 六个机制 crate（`wasmnest-server-base` / `-server-http` / `-server-websocket` /
//! `-server-peer-net` / `-discovery-engine` / `-pty-engine`）与移动侧
//! `wasmnest-ws-client-engine` 在「能力域脱绑」时把桌面 / 移动 SDK 的 wire 词汇
//! **自持为副本**，以免能力仓反向依赖端 SDK。副本与 SDK 原版的漂移是静默形态
//! （编译过、测试绿、运行期行为悄悄分叉），故以**源文件逐字比对**钉死：任一侧
//! 改字段 / 改类型 / 改注释 → 本锁红，双侧必须同步改。
//!
//! 这些锁的**主题属平台侧契约**（SDK 是被比对真源，平台是端契约拥有者），而能力仓
//! 要求单克隆可测（不读平台源文件）——票 08 把锁随主题迁入平台仓，2026-10-18 跨端
//! 测试面独立成仓后再随主题迁入本仓（经平台仓 `packages/` submodule 读取）；能力仓内的副本
//! 文件（`packages/<crate>/src/wire.rs`）本体不动、仍由本锁钉住。能力仓侧剩余的
//! 自洽用例（如 `pty_event_topic` 形状）不读平台文件，留在原 crate。
//!
//! ## 锁法
//!
//! 只读**源文件**做行 / 块级文本比对（不依赖编译期依赖关系、不启运行时）：SDK 源
//! 在本仓（`wasmnest-{desktop,mobile}/packages/…`），能力仓副本经 submodule mount
//! 读取（V1 布局：平台仓 `packages/` == 能力仓仓根）。
//!
//! 换行符归一化（`\r\n` → `\n`）：仓库双端文件换行风格不一（SDK 侧 CRLF），
//! 「逐字一致」按字符序列比对，行尾风格不算漂移。
//!
//! **防空转**：读文件 / 提取标记失败一律 panic（绝不静默跳过）——拆仓最危险的失效
//! 形态是锁读不到文件却照绿。提取块后再由断言文案指明漂移的双侧同步义务。

use std::fs;
use std::path::{Path, PathBuf};

// 跨仓路径解析（平台仓兄弟目录 / WASMNEST_PLATFORM_ROOT）
#[path = "common/repo_paths.rs"]
mod repo_paths;

/// 平台仓根（跨端测试仓的兄弟目录 `<WASMNEST>/wasmnest`）
fn repo_root() -> PathBuf {
    repo_paths::platform_root()
}

/// 读源文件并归一化换行（缺失即 panic，防锁空转）
fn read_normalized(path: &Path) -> String {
    fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("漂移锁读源文件失败（{}）：{e}", path.display()))
        .replace("\r\n", "\n")
}

/// 桌面 SDK 的 Rust 源码（漂移锁比对真源）
fn desktop_sdk_src(rel: &str) -> String {
    read_normalized(
        &repo_root()
            .join("wasmnest-desktop/packages/plugin-sdk-desktop/rust/src")
            .join(rel),
    )
}

/// 移动 SDK 的 Rust 源码（漂移锁比对真源）
fn mobile_sdk_src(rel: &str) -> String {
    read_normalized(
        &repo_root()
            .join("wasmnest-mobile/packages/plugin-sdk-mobile/rust/src")
            .join(rel),
    )
}

/// 能力仓内某 crate 的源文件（兄弟目录 `../wasmnest-lib` == 能力仓仓根）
fn lib_file(crate_name: &str, rel: &str) -> String {
    read_normalized(&repo_paths::lib_root().join(crate_name).join(rel))
}

/// 某能力 crate 的 `src/wire.rs`（副本比对对象）
fn local_wire(crate_name: &str) -> String {
    lib_file(crate_name, "src/wire.rs")
}

/// 提取文件中从 `start_marker`（含）起、到 `end_marker` 首次出现（不含）止的文本块
fn extract_block(source: &str, start_marker: &str, end_marker: &str) -> String {
    let start = source
        .find(start_marker)
        .unwrap_or_else(|| panic!("漂移锁：源文件缺起始标记 {start_marker:?}"));
    let tail = &source[start..];
    let end = tail
        .find(end_marker)
        .unwrap_or_else(|| panic!("漂移锁：源文件缺结束标记 {end_marker:?}"));
    tail[..end].to_string()
}

/// 同 [`extract_block`]，但起始标记取**最后一次出现**（`rfind`）
///
/// 仅 `wasmnest-ws-client-engine` 需要：其 `wire.rs` 模块文档里逐字引用了标记文本
/// （记账说明），用 `find` 会命中那段散文而不是真实常量块——抽出的是一句散文，锁
/// 于是比对两段不相干文本、永远红（lib 侧首版实测即如此）。结束标记仍取 `find`：
/// 从真实块起点出发，结束标记的第一次出现就是块的结尾（记账注释在块之前）。
fn extract_block_from_last(source: &str, start_marker: &str, end_marker: &str) -> String {
    let start = source
        .rfind(start_marker)
        .unwrap_or_else(|| panic!("漂移锁：源文件缺起始标记 {start_marker:?}"));
    let tail = &source[start..];
    let end = tail
        .find(end_marker)
        .unwrap_or_else(|| panic!("漂移锁：源文件缺结束标记 {end_marker:?}"));
    tail[..end].to_string()
}

/// 提取文件中以 `line_prefix` 开头的第一行（trim 后比对）
fn extract_line(source: &str, line_prefix: &str) -> String {
    source
        .lines()
        .find(|l| l.trim_start().starts_with(line_prefix))
        .unwrap_or_else(|| panic!("漂移锁：源文件缺 {line_prefix:?} 行"))
        .trim()
        .to_string()
}

/// 某一行（trim 后）是否出现在文件里
fn has_line(source: &str, needle: &str) -> bool {
    source.lines().any(|l| l.trim() == needle)
}

// ==================== wasmnest-server-base（能力域脱绑 P5） ====================

mod server_base {
    use super::*;

    /// [`BusMessage`] 与 SDK `lib.rs` 的定义逐字一致
    ///
    /// 行级比对钉死 wire 形状（derive / struct 声明 / 五字段名与类型及顺序）：
    /// 字段增删、类型变更、serde 行为变更任一发生即漂移转红。
    #[test]
    fn bus_message_copy_matches_sdk() {
        let sdk = desktop_sdk_src("lib.rs");
        let local = local_wire("wasmnest-server-base");
        for prefix in [
            "#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]",
            "pub struct BusMessage {",
            "pub topic: String,",
            "pub sender: String,",
            "pub payload: serde_json::Value,",
            "pub payload_binary: Option<Vec<u8>>,",
            "pub timestamp: u64,",
        ] {
            let sdk_line = extract_line(&sdk, prefix);
            let local_line = extract_line(&local, prefix);
            assert_eq!(
                sdk_line, local_line,
                "{prefix} 自持副本与桌面 SDK 原版漂移——能力域脱绑 P5：双侧必须逐字一致"
            );
        }
    }
}

// ==================== wasmnest-server-http（能力域脱绑 P1） ====================

mod server_http {
    use super::*;

    /// [`EndpointAuth`] 定义块（含注释）与 SDK `types.rs` 逐字一致
    ///
    /// 本地结束标记是非 doc 的 `//`（能力仓侧文件末尾无被文档化的项，`///` 会成悬空
    /// doc 编译错误）；比对块本体不变。
    #[test]
    fn endpoint_auth_copy_matches_sdk() {
        let sdk = desktop_sdk_src("types.rs");
        let local = local_wire("wasmnest-server-http");
        let sdk_block = extract_block(&sdk, "/// 端点认证档位", "/// 一条 HTTP 端点声明");
        let local_block = extract_block(&local, "/// 端点认证档位", "// 一条 HTTP 端点声明");
        assert_eq!(
            sdk_block, local_block,
            "EndpointAuth 自持副本与桌面 SDK 原版漂移——能力域脱绑 P1：双侧必须逐字一致"
        );
    }

    /// [`PERMISSION_NETWORK_HTTP`] 与 SDK `permission.rs` 逐字一致
    #[test]
    fn permission_network_http_copy_matches_sdk() {
        let sdk = desktop_sdk_src("permission.rs");
        let local = local_wire("wasmnest-server-http");
        let sdk_line = extract_line(&sdk, "pub const PERMISSION_NETWORK_HTTP");
        let local_line = extract_line(&local, "pub const PERMISSION_NETWORK_HTTP");
        assert_eq!(
            sdk_line, local_line,
            "PERMISSION_NETWORK_HTTP 自持副本与桌面 SDK 原版漂移——能力域脱绑 P1：双侧必须逐字一致"
        );
    }
}

// ==================== wasmnest-server-websocket（能力域脱绑 P3） ====================

mod server_websocket {
    use super::*;

    /// WS_* 状态事件常量定义块（含注释）与 SDK `host/ws.rs` 逐字一致
    #[test]
    fn ws_constants_copy_matches_sdk() {
        let sdk = desktop_sdk_src("host/ws.rs");
        let local = local_wire("wasmnest-server-websocket");
        let sdk_block = extract_block(&sdk, "/// 连接建立（客户端域）", "/// 生成属主私有状态事件 topic");
        let local_block = extract_block(&local, "/// 连接建立（客户端域）", "/// 生成属主私有状态事件 topic");
        assert_eq!(
            sdk_block, local_block,
            "WS_* 状态事件常量自持副本与桌面 SDK 原版漂移——能力域脱绑 P3：双侧必须逐字一致"
        );
    }

    /// [`EndpointAuth`] 定义块（含注释）与 SDK `types.rs` 逐字一致
    #[test]
    fn endpoint_auth_copy_matches_sdk() {
        let sdk = desktop_sdk_src("types.rs");
        let local = local_wire("wasmnest-server-websocket");
        let sdk_block = extract_block(&sdk, "/// 端点认证档位", "/// 一条 HTTP 端点声明");
        let local_block = extract_block(&local, "/// 端点认证档位", "/// 一条 HTTP 端点声明");
        assert_eq!(
            sdk_block, local_block,
            "EndpointAuth 自持副本与桌面 SDK 原版漂移——能力域脱绑 P3：双侧必须逐字一致"
        );
    }

    /// [`PERMISSION_WS_CLIENT`] / [`PERMISSION_WS_SERVER`] 与 SDK `permission.rs`
    /// 逐字一致
    #[test]
    fn permission_ws_copy_matches_sdk() {
        let sdk = desktop_sdk_src("permission.rs");
        let local = local_wire("wasmnest-server-websocket");
        for prefix in [
            "pub const PERMISSION_WS_CLIENT",
            "pub const PERMISSION_WS_SERVER",
        ] {
            let sdk_line = extract_line(&sdk, prefix);
            let local_line = extract_line(&local, prefix);
            assert_eq!(
                sdk_line, local_line,
                "{prefix} 自持副本与桌面 SDK 原版漂移——能力域脱绑 P3：双侧必须逐字一致"
            );
        }
    }

    /// [`TOPIC_NS_SEP`] / [`owned_topic`] 与 SDK `host/bus.rs` 逐字一致
    /// （签名 + 函数体行级比对；SDK 的 API_TOPIC_PREFIX / REPLY_TOPIC_PREFIX
    /// 本域不消费，不复制、不比对）
    #[test]
    fn topic_helpers_copy_matches_sdk() {
        let sdk = desktop_sdk_src("host/bus.rs");
        let local = local_wire("wasmnest-server-websocket");
        for prefix in [
            "pub const TOPIC_NS_SEP",
            "pub fn owned_topic",
            "format!(\"{owner}{TOPIC_NS_SEP}{name}\")",
        ] {
            let sdk_line = extract_line(&sdk, prefix);
            let local_line = extract_line(&local, prefix);
            assert_eq!(
                sdk_line, local_line,
                "{prefix} 自持副本与桌面 SDK 原版漂移——能力域脱绑 P3：双侧必须逐字一致"
            );
        }
    }
}

// ==================== wasmnest-server-peer-net（能力域脱绑 P4） ====================

mod server_peer_net {
    use super::*;

    /// [`PERMISSION_PEER`] 与 SDK `permission.rs` 逐字一致
    #[test]
    fn permission_peer_copy_matches_sdk() {
        let sdk = desktop_sdk_src("permission.rs");
        let local = local_wire("wasmnest-server-peer-net");
        let sdk_line = extract_line(&sdk, "pub const PERMISSION_PEER");
        let local_line = extract_line(&local, "pub const PERMISSION_PEER");
        assert_eq!(
            sdk_line, local_line,
            "PERMISSION_PEER 自持副本与桌面 SDK 原版漂移——能力域脱绑 P4：双侧必须逐字一致"
        );
    }
}

// ==================== wasmnest-discovery-engine（能力域脱绑 P2） ====================

mod discovery_engine {
    use super::*;

    /// `TOPIC_NS_SEP` 常量块（含注释）与 SDK `host/bus.rs` 逐字一致
    #[test]
    fn topic_ns_sep_copy_matches_sdk() {
        let sdk = desktop_sdk_src("host/bus.rs");
        let local = local_wire("wasmnest-discovery-engine");
        let sdk_block = extract_block(&sdk, "/// 私有 topic 命名空间分隔符", "/// 互调请求道前缀");
        let local_block = extract_block(&local, "/// 私有 topic 命名空间分隔符", "/// 互调请求道前缀");
        assert_eq!(
            sdk_block, local_block,
            "TOPIC_NS_SEP 自持副本与桌面 SDK 原版漂移——能力域脱绑 P2：双侧必须逐字一致"
        );
    }

    /// `owned_topic` 函数块（含注释）与 SDK `host/bus.rs` 逐字一致
    ///
    /// SDK 中 `TOPIC_NS_SEP` 与 `owned_topic` 定义之间隔着互调前缀常量，
    /// 本 crate 不复制那些宿主面常量，故拆两块锁。
    #[test]
    fn owned_topic_copy_matches_sdk() {
        let sdk = desktop_sdk_src("host/bus.rs");
        let local = local_wire("wasmnest-discovery-engine");
        let sdk_block = extract_block(&sdk, "/// 构造属主私有 topic", "/// 解析 topic 的属主");
        let local_block = extract_block(&local, "/// 构造属主私有 topic", "/// 解析 topic 的属主");
        assert_eq!(
            sdk_block, local_block,
            "owned_topic 自持副本与桌面 SDK 原版漂移——能力域脱绑 P2：双侧必须逐字一致"
        );
    }

    /// mDNS 事件名常量块（`MDNS_FOUND` / `MDNS_LOST`）与 SDK `host/mdns.rs` 逐字一致
    ///
    /// 本地结束标记是非 doc 的 `//`（能力仓侧文件末尾无被文档化的项，`///` 会成悬空
    /// doc 编译错误）；比对块本体不变。
    #[test]
    fn mdns_event_names_copy_matches_sdk() {
        let sdk = desktop_sdk_src("host/mdns.rs");
        let local = local_wire("wasmnest-discovery-engine");
        let sdk_block = extract_block(&sdk, "/// 发现到新实例", "/// 生成属主私有发现事件 topic");
        let local_block = extract_block(&local, "/// 发现到新实例", "// 生成属主私有发现事件 topic");
        assert_eq!(
            sdk_block, local_block,
            "mDNS 事件名（MDNS_FOUND / MDNS_LOST）自持副本与桌面 SDK 原版漂移——能力域脱绑 P2：双侧必须逐字一致"
        );
    }
}

// ==================== wasmnest-pty-engine（能力域脱绑 P4） ====================

mod pty_engine {
    use super::*;

    /// `TOPIC_NS_SEP` 常量块（含注释）与 SDK `host/bus.rs` 逐字一致
    #[test]
    fn topic_ns_sep_copy_matches_sdk() {
        let sdk = desktop_sdk_src("host/bus.rs");
        let local = local_wire("wasmnest-pty-engine");
        let sdk_block = extract_block(&sdk, "/// 私有 topic 命名空间分隔符", "/// 互调请求道前缀");
        let local_block = extract_block(&local, "/// 私有 topic 命名空间分隔符", "/// 互调请求道前缀");
        assert_eq!(
            sdk_block, local_block,
            "TOPIC_NS_SEP 自持副本与桌面 SDK 原版漂移——能力域脱绑 P4：双侧必须逐字一致"
        );
    }

    /// `owned_topic` 函数块（含注释）与 SDK `host/bus.rs` 逐字一致
    #[test]
    fn owned_topic_copy_matches_sdk() {
        let sdk = desktop_sdk_src("host/bus.rs");
        let local = local_wire("wasmnest-pty-engine");
        let sdk_block = extract_block(&sdk, "/// 构造属主私有 topic", "/// 解析 topic 的属主");
        let local_block = extract_block(&local, "/// 构造属主私有 topic", "/// 解析 topic 的属主");
        assert_eq!(
            sdk_block, local_block,
            "owned_topic 自持副本与桌面 SDK 原版漂移——能力域脱绑 P4：双侧必须逐字一致"
        );
    }

    /// PTY 事件常量块（`PTY_EXIT` / `PTY_OUTPUT`，含注释）与 SDK `host/pty.rs` 逐字一致
    #[test]
    fn pty_event_names_copy_matches_sdk() {
        let sdk = desktop_sdk_src("host/pty.rs");
        let local = local_wire("wasmnest-pty-engine");
        let sdk_block = extract_block(&sdk, "/// PTY 进程退出", "/// 生成属主私有事件 topic");
        let local_block = extract_block(&local, "/// PTY 进程退出", "/// 生成属主私有事件 topic");
        assert_eq!(
            sdk_block, local_block,
            "PTY 事件名（PTY_EXIT / PTY_OUTPUT）自持副本与桌面 SDK 原版漂移——能力域脱绑 P4：双侧必须逐字一致"
        );
    }

    /// 权限位（`PERMISSION_PTY_SPAWN` / `PERMISSION_PTY_IO`）与 SDK `permission.rs`
    /// 逐字一致
    #[test]
    fn pty_permissions_copy_matches_sdk() {
        for (const_name, marker) in [
            ("PERMISSION_PTY_SPAWN", "pub const PERMISSION_PTY_SPAWN"),
            ("PERMISSION_PTY_IO", "pub const PERMISSION_PTY_IO"),
        ] {
            let sdk = desktop_sdk_src("permission.rs");
            let local = local_wire("wasmnest-pty-engine");
            let sdk_line = extract_line(&sdk, marker);
            let local_line = extract_line(&local, marker);
            assert_eq!(
                sdk_line, local_line,
                "{const_name} 自持副本与桌面 SDK 原版漂移——能力域脱绑 P4：双侧必须逐字一致"
            );
        }
    }

    /// `KeyCombo` 生产段（`wire/key.rs`）与 SDK `wire/key.rs` 逐字一致
    #[test]
    fn key_combo_copy_matches_sdk() {
        let sdk = desktop_sdk_src("wire/key.rs");
        let local = lib_file("wasmnest-pty-engine", "src/wire/key.rs");
        let sdk_block = extract_block(
            &sdk,
            "//! 按键组合线协议（原宿主",
            "// ==================== 单元测试 ====================",
        );
        let local_block = extract_block(
            &local,
            "//! 按键组合线协议（原宿主",
            "// ==================== 单元测试 ====================",
        );
        assert_eq!(
            sdk_block, local_block,
            "KeyCombo 生产段自持副本与桌面 SDK 原版漂移——能力域脱绑 P4：双侧必须逐字一致"
        );
    }
}

// ==================== wasmnest-ws-client-engine（引擎抽根，移动 SDK） ====================

mod ws_client_engine {
    use super::*;

    /// 状态事件名常量块（`WS_OPEN` / `WS_ERROR` / `WS_CLOSE` /
    /// `WS_RECONNECT_SCHEDULED`，含注释）与移动 SDK `host/ws.rs` 逐字一致
    #[test]
    fn event_name_constants_copy_matches_mobile_sdk() {
        let sdk_block = extract_block_from_last(
            &mobile_sdk_src("host/ws.rs"),
            "/// 连接建立（客户端域）",
            "/// 生成属主私有状态事件 topic",
        );
        let local_block = extract_block_from_last(
            &local_wire("wasmnest-ws-client-engine"),
            "/// 连接建立（客户端域）",
            "/// 生成属主私有状态事件 topic",
        );
        assert_eq!(
            sdk_block, local_block,
            "状态事件名自持副本与移动 SDK 原版漂移（事件名 / 注释任一改动都必须双侧同步）"
        );
        assert!(
            sdk_block.contains("ws:open") && sdk_block.contains("ws:reconnect-scheduled"),
            "漂移锁空转：提取块不含预期事件名，实得 {sdk_block:?}"
        );
    }

    /// topic 助手块（`ws_event_topic` / `WS_MESSAGE` / `ws_message_topic`，含注释）
    /// 与移动 SDK `host/ws.rs` 逐字一致
    #[test]
    fn topic_helpers_copy_matches_mobile_sdk() {
        let sdk_block = extract_block_from_last(
            &mobile_sdk_src("host/ws.rs"),
            "/// 生成属主私有状态事件 topic",
            "// ==================== 帧信封（宿主 → 插件的入站帧） ====================",
        );
        let local_block = extract_block_from_last(
            &local_wire("wasmnest-ws-client-engine"),
            "/// 生成属主私有状态事件 topic",
            "// ==================== 帧信封（宿主 → 插件的入站帧） ====================",
        );
        assert_eq!(
            sdk_block, local_block,
            "属主私有 topic 拼法自持副本与移动 SDK 原版漂移——宿主发布 topic 与插件订阅 topic \
             必须同源（漂移的表现为「订阅了却永远收不到」，静默无报错）"
        );
    }

    /// 帧信封的形状常量（kind / 头长）与权限字面量：行级比对（含 SDK 侧行必须在场）
    #[test]
    fn frame_shape_and_permission_lines_match_mobile_sdk() {
        let sdk_ws = mobile_sdk_src("host/ws.rs");
        let sdk_permission = mobile_sdk_src("permission.rs");
        let local = local_wire("wasmnest-ws-client-engine");

        // 帧 kind 与头长：信封字节布局的双方约定（宿主构造 / 插件解析）
        for line in [
            "pub const WS_FRAME_KIND_TEXT: u8 = 1;",
            "pub const WS_FRAME_KIND_BINARY: u8 = 2;",
            "pub const WS_FRAME_HEADER_LEN: usize = 3;",
        ] {
            assert!(
                has_line(&sdk_ws, line),
                "移动 SDK host/ws.rs 缺帧形状行：{line}"
            );
            assert!(
                has_line(&local, line),
                "本 crate wire.rs 缺帧形状行（漂移）：{line}"
            );
        }

        // 权限字面量：拒绝文案与 manifest 声明词汇共同引用它
        let permission_line = "pub const PERMISSION_WS_CLIENT: &str = \"ws:client\";";
        assert!(
            has_line(&sdk_permission, permission_line),
            "移动 SDK permission.rs 缺 {permission_line}"
        );
        assert!(
            has_line(&local, permission_line),
            "本 crate wire.rs 的权限字面量与移动 SDK 漂移：{permission_line}"
        );
    }
}
