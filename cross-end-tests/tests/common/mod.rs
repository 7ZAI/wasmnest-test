//! 跨端互连测试的共享装配（桌面无头服务 + 移动端客户端）
//!
//! 两个子模块（另有独立 resolver 模块 `app_paths` / `repo_paths`，由各测试文件经
//! `#[path]` 直接编入）：
//! - [`desktop_ctx`]：进程内无头启动桌面端真实 Actix 服务器 + 激活**真实**
//!   wasm 认证中心产物（`com.wasmnest.bedcode`），复刻
//!   `wasmnest/wasmnest-desktop/src-tauri/tests/pty_session_chain.rs` 的 `app_handle(None)`
//!   模式（该模式的先例与理由见该文件头注释）。
//! - [`mobile_ctx`]：移动端真实客户端装配（目标设备 / 全局 token / 事件与
//!   终端输出记录替身）。
//!
//! 零 mock 口径：桌面侧跑真实插件 + 真实 PTY，移动侧跑真实 HTTP / WS 客户端。
//! 替身只**记录**已发生的事实（事件名 / 输出字节），不伪造任何协议应答。

#![allow(dead_code)]

pub mod desktop_ctx;
pub mod mobile_ctx;
pub mod mobile_plugin;
