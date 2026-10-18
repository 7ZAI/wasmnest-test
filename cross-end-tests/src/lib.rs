//! 跨端真实互连集成测试（移动端真实客户端代码 ↔ 桌面端真实服务器）
//!
//! 存在的理由：两端既有测试各自用**协议级 mock 对端**验证 wire 契约
//! （桌面 `pty_session_chain` 用通用 reqwest / tokio-tungstenite 当移动端；
//! 移动端 `tests/common` 用假桌面服务器当对端）。两套 mock 各自自洽，
//! **文档与任一端实现偏离时两端测试全绿，真实互连必坏**。
//!
//! 本包在同一测试进程内让「桌面端真实 Actix 服务器 + 真实 wasm 认证中心产物」
//! 与「移动端真实客户端代码」（`AuthHttpClient` / `SessionHttpClient` /
//! `TerminalLinkManager`）互连，零 mock、不开 adb、不起 WebView。
//!
//! 每个场景 = 独立测试二进制（进程隔离）：桌面端 `AppContext` 是进程级
//! `OnceLock` 单例，场景间无法重装。
//!
//! spec：`.scratch/2026-09-30-cross-end-integration-tests/spec.md`
