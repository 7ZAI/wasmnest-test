//! WasmNest 性能测试工程
//!
//! 与 `cross-end-tests` 的分工：那边钉**正确性**（真实两端互连、零 mock），
//! 这边量**性能**（装配态延迟 / 吞吐 / 启动 / 内存）。两者共享同一套跨仓路径
//! 纪律：平台、能力、应用仓都经 sibling 路径读取，基准加载真实随包产物。
//!
//! 模块：
//! - `bridge`：wasm ↔ 宿主 ↔ 前端命令面（迁移自平台仓 `wasm_bridge_bench`）；
//! - `apps`：四款真实业务应用的装载 / 激活 / 本地命令；
//! - `boundary`：宿主规模（N 个实例同时常驻）与宿主 → 前端注册表投影边界；
//! - `report` / `env` / `support`：取数、环境快照、路径与夹具装配。
//!
//! spec：本仓 README 与 `docs/adr/0001-perf-benchmarks-live-in-test-repo.md`。

pub mod env;
pub mod modules;
pub mod report;
pub mod support;
