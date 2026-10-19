# AGENTS.md —— wasmnest-test（跨端测试容器仓）

> **本仓是容器目录**：工程本体在 `cross-end-tests/`（独立 Cargo workspace：主包 + `desktop-harness`）；容器根只放 `.cargo/config.toml` / `.editorconfig` / `.gitignore` / `LICENSE` 与本文件。本文件给首次进入的协作者一个导航与通用规范入口；机制细节读 `cross-end-tests/` 各 crate 头注释与测试头注释。

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
- **跨仓路径铁律**：测试工程的能力仓依赖**必须**经同级兄弟目录 `../wasmnest-lib/<crate>`——平台双端宿主的 path 依赖写死 `../../../wasmnest-lib/<crate>`，指到同一物理路径才能保证能力 crate 只有一份（类型同一性）；平台仓不再挂 `packages/` submodule。能力仓根经 `tests/common/repo_paths.rs::lib_root()` 解析（`WASMNEST_LIB_ROOT` 可覆盖，fail-visible），平台仓根经 `platform_root()`，应用仓经 `tests/common/app_paths.rs`。
- 任务路由：改传输 / 认证 / 配对面 → 平台仓 docs/architecture.md §6 + 本仓对应测试头注释；改互连测试 → 本仓各测试头注释 + 平台仓对应锁。

## 2. 硬约束速查

- 不提交产物：`target/`、临时日志不入库（.gitignore 已覆盖）。
- 测试数据走临时目录，禁止污染真实数据 / 日志目录。
- 锁纪律：不得删锁 / 缩 glob / 恒真断言；锁清单过期同批更新（Seam 3，public/testing.md §4）。
- 改动跨端面后，平台仓 / 能力仓的相关锁与本仓互连测试是同一交付面：跑完本仓测试并在交付说明点名「跑了 / 没跑」。