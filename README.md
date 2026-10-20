# wasmnest-test

**WasmNest 跨端测试仓 —— 让真实的移动端客户端代码与真实的桌面端服务器互连，零协议 mock。**

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-stable-DEA584?logo=rust&logoColor=white)](https://www.rust-lang.org)
[![wasmtime](https://img.shields.io/badge/wasmtime-48-654FF0?logo=webassembly&logoColor=white)](https://wasmtime.dev)
[![Zero mock](https://img.shields.io/badge/interop-zero%20mock-success)](#为什么需要这个仓)

简体中文 | [English](README_en.md)

## 为什么需要这个仓

两端各自都有测试，但过去它们对面都是**协议级 mock**：桌面端用通用 `reqwest` / `tokio-tungstenite` 扮演移动端，移动端用假桌面服务器扮演对端。两套 mock 各自自洽——文档或任一端实现悄悄偏离时，两端测试仍然全绿，真实互连却会坏。

本仓把两条真实链路放进同一个测试台：

```text
移动端真实客户端 / 真实 wasm 插件产物
        ⇅  真实 TCP（HTTP / WebSocket）
桌面端真实 Actix 服务器 + 真实认证中心 wasm 插件 + 真实 bash PTY
```

零协议 mock、不开 adb、不起 WebView。每个场景是一个独立测试二进制（进程隔离），因为桌面 `AppContext` 是进程级 `OnceLock` 单例。

## 两种测试台

| 形态 | 构建模式 | 说明 |
| --- | --- | --- |
| **单进程** | 默认（`desktop` feature） | 桌面 lib、移动 lib 与测试代码同进程；覆盖配对、认证、会话 HTTP、事件通道、失败路径、生命周期、mDNS 等场景 |
| **两进程** | `--no-default-features --features two-proc` | 桌面侧跑独立 `desktop-harness` 进程，测试进程只链移动侧；桌面 lib 整体出图，避免两端宿主域实现（`inventory` 进程级注册）互相覆盖 |

两进程形态是终端流场景的权威形态：单进程版 `terminal_ws_flow` 因两端宿主域实现冲突而 `#[ignore]`，真实覆盖由 `two_proc_terminal_ws` 承接。

两种形态都有**台子自检**：场景红了先证明台子没坏（服务器已按 GUI bootstrap 同一装配面装好端口、移动端目标地址可回读、真实插件确实在处理请求、停机后端口确实释放）。

> **设备层不在本仓**：双端 UI 端到端（桌面脚本点击 + USB 真机 adb / WebView CDP 点击与断言）住应用仓
> `wasmnest-apps/e2e/dual/`，入口 `node scripts/e2e-dual.mjs --app bedcode`，说明见其 `docs/E2E.md` §10。
> 本仓刻意保持零设备依赖：默认 `cargo test` 在无真机、无 GUI 的机器上必须能跑；设备套件是另一层回归（nightly / 发布前），
> 不应把「没插手机」变成协议层测试的红或静默 skip。

## 场景与锁

### 互连场景

| 用例 | 钉住什么 |
| --- | --- |
| `harness_selfcheck` | 测试台接线：目标地址、服务器端口装配、真实 HTTP 往返、端口归属、插件 WS 端点登记 |
| `pairing_auth_flow` | 配对 / 认证 HTTP 闭环：错码不消耗有效码、token 交换、错误码映射 |
| `jwt_rotate_reconnect` | 入场密钥轮换宽限期：轮换前签发的 token 在轮换后仍可验签并重连 |
| `session_http_flow` | 移动真实插件 → 桌面真实插件的会话 HTTP 面；JWT 由宿主代注 |
| `terminal_ws_flow` | 终端流单进程形态（**默认 `#[ignore]`**，见上） |
| `two_proc_terminal_ws` | 两进程终端流：真实 bash PTY、输出连续性、重订阅重放、背压与窄转发零丢弃 |
| `fail_closed_flow` | 无中心在册 / 非法凭证一律显性拒绝；WS 永不 live、零输出字节 |
| `lifecycle_flow` | 桌面插件停用 / 激活联动：WS 回收、端点注销、移动端显性失败与重建 |
| `event_channel_flow` | 桌面插件广播 → 宿主 WS 端点 → 移动真实 WS 客户端 → 事件路由 |
| `http_proxy_flow` | 移动端唯一 HTTP 出路：Egress L1 + JWT 注入 + 链路加密信封 |
| `mdns_health_flow` | mDNS 发现 / 广播 + `/api/health` 探测（用户「发现并连上桌面」的入口） |
| `two_proc_selfcheck` | 两进程装配守卫：测试进程 `inventory` 只含移动模块；harness spawn / RPC / 优雅关停 |

### 契约锁

| 锁 | 钉住什么 |
| --- | --- |
| `wire_drift_locks` | 能力 crate 自持的 wire 词汇副本 ↔ 平台端 SDK 原版，逐字比对 |
| `permission_vocabulary_lock` | 权限词汇单一真源 ↔ 平台侧五处事实（生成物 / 校验器 / 宿主 / SDK） |
| `command_vocabulary_lock` | 双端生命周期命令面命名一致，防一端改名另一端 command not found |
| `mdns_browse_singleton_lock` | 同一服务类型只允许类型中枢调用 `daemon.browse`，禁止旁路直调 |
| `bedcode_app_identity_lock` | BedCode 的身份豁免三件与 id 家族替换边界 |
| `file_transfer_wiring_lock` | 双端单产物：核内逻辑不得回流，移动端不得重建产物源 |

## 性能基准

`perf-tests/` 是独立 Cargo workspace，对平台桥接与随包真实应用做 release 测量：

| 模块 | 测什么 |
| --- | --- |
| `bridge` | 宿主 ↔ wasm 插件的桥接原语与通道（迁移自平台仓 `wasm_bridge_bench`） |
| `apps` | 四个随包业务应用的真实命令链路（ai-chatbox / agent-hub / BedCode / file-transfer） |

```bash
cd perf-tests

cargo run --release --bin perf-harness            # 冒烟（秒级）
cargo run --release --bin perf-harness -- --full  # 全量矩阵
cargo run --release --bin perf-harness -- --list  # 场景清单
```

场景矩阵、门限、无头装配语义与 `--compare` 用法见 [`perf-tests/README.md`](perf-tests/README.md)。debug 构建被入口拒绝；apps 模块要求先构建桌面端随包应用产物。

## 前置条件

| 依赖 | 说明 |
| --- | --- |
| 四仓布局 | 平台仓、能力仓、应用仓与本仓按 sibling 布局摆放；工程内通过 `../../wasmnest`、`../../wasmnest-lib` 解析跨仓路径 |
| 应用产物 | 测试加载**真实随包 wasm 产物**，先构建两端口：`wasmnest-desktop` 与 `wasmnest-mobile` 各跑一次 `pnpm run plugins:build` |
| Rust / sccache | Rust stable；`.cargo/config.toml` 强制 `sccache`，未安装时 cargo 直接失败 |
| 终端环境 | 终端流场景使用真实 `bash` PTY，需要类 Unix 环境 |
| 应用依赖 | 构建应用产物前，应用仓各 app 工程需先完成 `pnpm install` |

非标准布局可用环境变量覆盖：

| 变量 | 作用 |
| --- | --- |
| `WASMNEST_PLATFORM_ROOT` | 平台仓根（默认 `../wasmnest`） |
| `WASMNEST_LIB_ROOT` | 能力仓根（默认 `../wasmnest-lib`） |
| `WASMNEST_APPS_ROOT` | 应用仓根（默认 `../wasmnest-apps`） |

resolver 全部 fail-visible：目录缺失直接抛错 / panic，不会返回可能无效的路径。

## 运行

```bash
cd cross-end-tests

# 全量单进程场景（terminal_ws_flow 按设计跳过）
cargo test

# 单个场景
cargo test --test pairing_auth_flow
cargo test --test wire_drift_locks

# 两进程形态（先构建桌面 harness，再跑 two-proc 测试）
cargo build -p wasmnest-crossend-desktop-harness
cargo test --no-default-features --features two-proc

# 单个两进程场景
cargo test --no-default-features --features two-proc --test two_proc_terminal_ws
```

**推荐排查顺序**：先跑 `harness_selfcheck`（或 `two_proc_selfcheck`）确认台子可信，再看具体场景。台子问题伪装成协议 bug，是本仓历史上最常见的误判来源。

## 仓库结构

```text
wasmnest-test/
├─ cross-end-tests/                # 工程本体（独立 Cargo workspace）
│  ├─ Cargo.toml                   # 主包 + workspace 根 + 两种构建模式
│  ├─ src/lib.rs                   # crate 级说明
│  ├─ tests/                       # 场景测试 + 契约锁（每个场景一个二进制）
│  │  ├─ common/                   # desktop_ctx / mobile_ctx / mobile_plugin / repo_paths
│  │  ├─ two_proc_common/          # 两进程驱动 + 复用同一套移动侧夹具
│  │  └─ *.rs
│  ├─ desktop-harness/             # 两进程形态的桌面进程（stdin/stdout JSON-RPC）
│  └─ rustfmt.toml
├─ perf-tests/                     # 性能基准（独立 Cargo workspace）
│  ├─ src/modules/{bridge,apps}.rs # 桥接基准 + 真实应用基准
│  ├─ src/support/                 # 宿主装配 / 临时 HOME / 应用夹具环境 / RSS
│  ├─ docs/adr/                    # 性能工程决策
│  ├─ GLOSSARY.md                  # 性能上下文术语
│  └─ README.md                    # 场景 / 门限 / 运行方式
├─ .cargo/config.toml              # sccache + 编译资源限制
├─ AGENTS.md                       # 本仓导航与通用规范入口
└─ LICENSE
```

## 跨仓边界

- 两端宿主通过 sibling path 直接引用：`../../wasmnest/{wasmnest-desktop,wasmnest-mobile}/src-tauri`；能力 crate 必须经 `../../wasmnest-lib/<crate>` 读取，保证依赖图中只有一份（类型同一性）。
- 桌面侧装配必须与 GUI bootstrap 同款：端口面走宿主组合根的 `server::composition::install_server_ports()`。缺端口时 HTTP 网关会静默判 `PassThrough`，插件登记的端点全部 404——看起来像协议 bug，实际是台子没接好。
- 本仓的锁是交付面的一部分：不得删锁、缩小 glob、把断言改成恒真；锁清单过期必须在同一提交内同步更新。
- 改跨端 wire / 权限词汇 / 生命周期命令面时，本仓测试与平台仓、能力仓的对应锁必须同时变绿。

## 文档与贡献

| 需求 | 入口 |
| --- | --- |
| 本仓导航与硬约束 | [`AGENTS.md`](AGENTS.md) |
| 场景行为契约 | 各测试文件头注释（每条用例的来源 / 行为 / 场景） |
| 四仓通用规范 | 平台仓 `docs/public/`（git / 注释命名 / 测试 / 版本 / issue） |
| 平台仓架构与跨端协议 | [wasmnest/docs/architecture.md](https://github.com/7ZAI/wasmnest/blob/main/docs/architecture.md) |
| 能力仓契约真源 | [wasmnest-lib](https://github.com/7ZAI/wasmnest-lib) |
| 应用仓 | [wasmnest-apps](https://github.com/7ZAI/wasmnest-apps) |

提交前至少跑改动面对应的场景与锁，并在交付说明里点名「跑了什么 / 没跑什么 / 风险」。当前没有 CI，验证 = 本地实跑。跨会话任务以 [GitHub Issues](https://github.com/7ZAI/wasmnest-test/issues) 为真源。

## 版本

研发起步期，版本线 `0.0.1`。本仓独立打 tag（`test-vX.Y.Z`），不参与产品装配；版本号只作标识语义。

## 许可证

[MIT](LICENSE) © 2026 7ZAI
