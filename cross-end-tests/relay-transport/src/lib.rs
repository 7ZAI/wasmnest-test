//! 内网穿透传输面集成测试工程（无生产代码）。
//!
//! 单独成 workspace 成员的理由见 `Cargo.toml`：场景只依赖能力仓
//! `wasmnest-relay` 与 `wasmnest-p2p-engine`，不需要任何宿主 lib；放进主包会让
//! 桌面 / 移动宿主（tauri + wasmtime）参与编译。
//!
//! 用例见 `tests/relay_reservation_flow.rs`：进程内真实中继（CI 可跑）+ 已部署
//! 公网中继（`#[ignore]` + `WASMNEST_RELAY_E2E_ADDRS`）。
