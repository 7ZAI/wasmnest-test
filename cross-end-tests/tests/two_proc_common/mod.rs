//! 两进程 rig 的测试侧公共模块（`two-proc` 构建模式；`wasmnest-test` 仓的
//! `cross-end-tests` 包内）
//!
//! - [`desktop`]：桌面进程驱动（spawn `desktop-harness` bin + stdin/stdout JSON-RPC）。
//! - [`mobile_ctx`] / [`mobile_plugin`]：**复用单进程 rig 的同名夹具**（真插件产物 +
//!   无头宿主 + 页面窄转发）——`#[path]` 直接编入同一源文件，夹具单一真源不再有两份副本。
//!
//! ⚠ 本模块**不得**引用 `common::desktop_ctx`（桌面 lib）：`two-proc` 模式要求测试进程
//! 只链移动侧（cargo feature 把桌面 lib 出图 ⇒ server-crate `common-host` 无人开启）。
//! 装配守卫见 `tests/two_proc_selfcheck.rs`（fail-visible：依赖图被破坏时直接红）。

pub mod desktop;

// 共享夹具文件：在只用部分的测试二进制里未用项不算死代码（two_proc_selfcheck 只用
// DesktopHarness；two_proc_terminal_ws 用全套）。项在对应场景二进制里照常被引用。
#[allow(dead_code)]
#[path = "../common/mobile_ctx.rs"]
pub mod mobile_ctx;

#[allow(dead_code)]
#[path = "../common/mobile_plugin.rs"]
pub mod mobile_plugin;