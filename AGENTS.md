# AGENTS.md —— wasmnest-test（跨端测试容器仓）

> **本仓是容器目录**：工程本体在 `cross-end-tests/`（独立 Cargo workspace：主包 + `desktop-harness`）与 `perf-tests/`（独立 Cargo workspace：性能基准 harness）；容器根只放 `.cargo/config.toml` / `.editorconfig` / `.gitignore` / `LICENSE` 与本文件。本文件给首次进入的协作者一个导航与通用规范入口；机制细节读各工程 crate 头注释与测试头注释。

## 0. 通用规范（四仓共享，真源 = 平台仓 `docs/public/`）

| 规范 | 文档 |
| --- | --- |
| git 提交规范（commit 格式 / 禁止 AI 标记 / --force 禁令 / 开工自检 / 只提交源码 / 文件回滚 / tag 规则） | `../wasmnest/docs/public/git.md` |
| 代码注释与命名（解释为什么 / 中文 / 禁票号引用 / 同步修正） | `../wasmnest/docs/public/commenting.md` |
| 测试与验证规范（单元测试形态 / 禁止交付 / 临时目录 / Seam 3 锁纪律 / 验证底线） | `../wasmnest/docs/public/testing.md` |
| 版本号管理最佳实践（**只有发布版本时才设置版本号，dev 日常不 bump**；tag 联动） | `../wasmnest/docs/public/versioning.md` |
| issue 管理规范（GitHub Issues = 跨会话存续 backlog 真源；三层分工 / 落仓规则 / 模板标签 / 关门纪律 / agent 约定） | `../wasmnest/docs/public/issues.md` |

以上与各仓 AGENTS.md 的硬约束**同级别**；通用内容不在本仓复制（本仓特殊：`cross-end-tests/` 的锁以 Rust 测试形态存在，Seam 3 纪律直接适用 public/testing.md §4）。

## 1. 工程导航

- **工程本体**：`cross-end-tests/` —— Cargo workspace（根 `Cargo.toml`）：主包（互连测试：`tests/` 下 `wire_drift_locks` / `permission_vocabulary_lock` / `mdns_browse_singleton_lock` 等）+ `desktop-harness`（桌面侧测试夹具二进制）。目标构建见各 crate `.cargo/config.toml` / 头注释。
- **性能基准**：`perf-tests/` —— 独立 Cargo workspace（`perf-harness` 二进制 + `perf_tests` 库）：`bridge` 模块（迁移自平台仓 `wasm_bridge_bench`，含 `M` 调用矩阵）+ `boundary` 模块（N 个应用同时常驻的规模边界 / 宿主 → 前端注册表投影）+ `apps` 模块（四个随包真实应用）。用法 / 场景 / 门禁见 [`perf-tests/README.md`](perf-tests/README.md)，术语见 [`perf-tests/GLOSSARY.md`](perf-tests/GLOSSARY.md)，决策见 `perf-tests/docs/adr/`。
- **双端 UI E2E 不在此处**：桌面脚本点击 + USB 真机（adb 反向端口 + WebView CDP 触摸/断言）的端到端套件住应用仓 `wasmnest-apps/e2e/dual/`（入口 `node scripts/e2e-dual.mjs --app bedcode`，说明见其 `docs/E2E.md` §10）。本仓不加设备依赖：默认 `cargo test` 必须在无真机 / 无 GUI 的机器上可跑，设备套件是 nightly / 发布前的另一层回归。
- **跨仓路径铁律**：测试工程的能力仓依赖**必须**经同级兄弟目录 `../wasmnest-lib/<crate>`——平台双端宿主的 path 依赖写死 `../../../wasmnest-lib/<crate>`，指到同一物理路径才能保证能力 crate 只有一份（类型同一性）；平台仓不再挂 `packages/` submodule。能力仓根经 `tests/common/repo_paths.rs::lib_root()` 解析（`WASMNEST_LIB_ROOT` 可覆盖，fail-visible），平台仓根经 `platform_root()`，应用仓经 `tests/common/app_paths.rs`；perf-tests 用同一套 resolver 语义（`src/support/repo_paths.rs`）。
- 任务路由：改传输 / 认证 / 配对面 → 平台仓 docs/architecture.md §6 + 本仓对应测试头注释；改互连测试 → 本仓各测试头注释 + 平台仓对应锁；改桥接原语或应用命令 → perf-tests 对应模块，数字变化写入报告环境快照。

## 2. 硬约束速查

- 不提交产物：`target/`、临时日志不入库（.gitignore 已覆盖）。
- 测试数据走临时目录，禁止污染真实数据 / 日志目录。
- 锁纪律：不得删锁 / 缩 glob / 恒真断言；锁清单过期同批更新（Seam 3，public/testing.md §4）。
- 性能纪律（perf-tests）：只采 release 数字（入口拒绝 debug）；apps 模块要求先构建随包应用产物，缺失 fail-visible；应用数据写入临时 `HOME` 沙箱，退出恢复；数量级门禁是交付面的一部分——不得删测点 / 放宽门限 / 把断言改成恒真，数字变化需在交付说明点名。
- 改动跨端面后，平台仓 / 能力仓的相关锁与本仓互连测试是同一交付面：跑完本仓测试并在交付说明点名「跑了 / 没跑」。

## 3. 交流准则（格赖斯合作原则）

面向本仓人类与 AI 协作者（含 vibe coding）的沟通规则，以格赖斯（H. P. Grice）合作原则与四项会话准则为纲：**在不牺牲准确性与必要深度的前提下提高沟通效率**。任何回复、评审意见、任务汇报、交付说明均适用。

- **量的准则（信息充分而不冗余）**：提供接收方完成行动所需的信息量，不多不少——既不遗漏关键结论 / 依据 / 风险 / 未跑项，也不堆砌无关背景、重复已知结论、贴大段代码或日志凑篇幅。判据：接收方能据此行动且无需再追问。
- **质的准则（论述真实且有依据）**：不说无依据、无法核实或不信为真的话；凡论断必有依据（命令输出、源码引用、文档指针、可复现实验），并区分「事实 / 推断 / 猜测 / 待验证」，禁止把猜测包装成结论。数据、路径、版本、锁名引用前先核实（文档字面 ≠ 事实）。
- **关系准则（回应紧扣问题）**：回应与所问直接相关，被问什么就答什么；不答非所问、不借题发挥、不夹带与当前任务无关的改动或建议。
- **方式准则（表达清晰有序）**：避免晦涩、歧义与冗长；结论先行、要点编号、按逻辑顺序组织；术语一致，路径 / 命令 / 锁名 / 版本用可复制的准确字面；先给结论与证据，再给过程与细节。

**底线而非上限**：这些准则是沟通下限，不是压缩上限——涉及准确性、安全、契约、跨仓同步、必要深度时，不得以「简洁」为由省略；与本文档其他硬约束冲突时，以对应硬约束为准。