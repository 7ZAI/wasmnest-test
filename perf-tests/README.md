# 性能基准工程（perf-tests）

对 WasmNest 平台桥接、宿主规模边界与随包真实应用做**可重复的 release 性能测量**。与
`cross-end-tests/` 平级、独立 Cargo workspace：正确性测试与性能测量分开构建，
避免 debug 构建、测试夹具或不同 profile 污染数字。

```text
perf-harness
├─ bridge 模块   宿主 ↔ wasm 插件的桥接原语（迁移自平台仓 wasm_bridge_bench）
├─ boundary 模块 宿主规模（N 个应用同时常驻）与宿主 → 前端注册表投影边界
└─ apps 模块     四个随包业务应用的真实命令链路（ai-chatbox / agent-hub / BedCode / file-transfer）
```

## 前置条件

| 依赖 | 说明 |
| --- | --- |
| 四仓 sibling 布局 | 平台仓、能力仓、应用仓与本仓同级；路径 resolver fail-visible，可用 `WASMNEST_PLATFORM_ROOT` / `WASMNEST_LIB_ROOT` / `WASMNEST_APPS_ROOT` 覆盖 |
| 应用产物 | 先构建平台仓桌面端随包应用（`wasmnest-desktop` 的 `pnpm run plugins:build`），产物在 `resources/plugins/desktop/<plugin-id>/` |
| Rust stable | apps 模块会把 `HOME` 指到临时沙箱；bridge / boundary 的夹具编译显式使用原始 `HOME`（`nightly-2026-09-16` 工具链由夹具构建脚本固定） |
| Linux（推荐） | 常驻/峰值 RSS 经 `/proc/self/status` 读取；其他平台峰值退回当前值 |

## 运行

```bash
cd perf-tests

# 冒烟（秒级）：A1 / D2 / S1 / F1 / H1 / I1 / J1 / K1 / L1 / R1
cargo run --release --bin perf-harness

# 全量矩阵
cargo run --release --bin perf-harness -- --full

# 只看某个模块 / 分组 / 场景
cargo run --release --bin perf-harness -- --module apps --full
cargo run --release --bin perf-harness -- --module boundary --full
cargo run --release --bin perf-harness -- --module bridge --group E
cargo run --release --bin perf-harness -- --group K --iters 10

# 列出稳定场景 id
cargo run --release --bin perf-harness -- --list

# JSON 报告（默认 target/perf/results/perf-<unix>.json）与跨次对比
cargo run --release --bin perf-harness -- --full --json baseline.json
cargo run --release --bin perf-harness -- --full --compare baseline.json
```

参数：

| 参数 | 含义 |
| --- | --- |
| `--module bridge\|boundary\|apps\|all` | 模块选择（默认 all；services / crossend 为第二批） |
| `--full` | 全量场景；默认只跑冒烟子集 |
| `--group <G>` | 单字母分组或 id 子串过滤 |
| `--iters <N>` | 每测点重复次数（默认 full=5、冒烟=1，取中位数） |
| `--json [path]` | 报告落盘路径（无路径时用默认路径） |
| `--compare <path>` | 与历史 JSON 按测点 id 对比（只观察，不参与门禁） |
| `--list` | 打印场景 id / 分组 / 标题后退出 |
| `--allow-debug` | 允许 debug 构建（只用于开发 harness 本身） |

debug 构建默认被拒绝：性能数字只对 release 有意义。

## 场景

### bridge（迁移自平台仓 `wasm_bridge_bench`）

| 组 | 覆盖 |
| --- | --- |
| A | 命令通道空载 / 载荷尺寸扫描 / guest CPU 下界 |
| B | host-log、storage、crypto、config、fs 等原语往返 |
| C | 总线事件发布 / 订阅投递 |
| D | 插件互调（同实例 / 跨实例、同步与异步） |
| E | 大载荷流式（分块读写） |
| F | 异步形态（process.run / 定时器） |
| G | 并发与调度（实例锁、慢调用阻塞） |
| M | wasm-core 调用矩阵：载荷尺寸 × 并发、属主队列饱和、异步进程并发、mutex vs event-loop |

### M · wasm-core 调用矩阵（full）

| 场景 | 覆盖 | 判定 |
| --- | --- | --- |
| M1 | 单 Store 串行载荷矩阵：`echo` 0 / 1 KiB / 64 KiB / 1 MiB × 1 / 8 / 64 并发；`storage` 1 KiB / 64 KiB / 1 MiB × 1 / 8 并发 | 回显/读回全对；1 MiB × 64 echo 单次 < 20 ms；1 MiB × 8 KV 单次 < 100 ms |
| M2 | event-loop 属主队列饱和：慢调用占住属主后投递 `64 + 16` 条 nop | 前 64 条入队，后 16 条 `queue is full`；只允许队列满错误 |
| M3 | 异步进程并发：1 / 4 / 8 路 `process.run` 提交，轮询到全部 `processDone` | 提交 0 失败；提交中位数 < 50 ms；全部完成 < 10 s |
| M4 | mutex vs event-loop 同负载：2 / 4 路 1 MiB KV | 两种模型均完成；event-loop / mutex < 10×（记录实际比值） |

M 组只在 `--full` 下运行。M2 的容量 64 来自 `owner.rs::OWNER_QUEUE_CAP`（经测试转发面读取），
不是拍脑袋常量；M4 的两种模型通过“停用 → 重新加载实例 → 按模型装表 → 激活”切换，
保证调用模型仍是实例级快照。

### boundary（宿主规模 / 前端注册表投影）

| 组 | 场景 | 覆盖 |
| --- | --- | --- |
| S | S1 | 同一 wasm 组件摆成 N 个属主，逐个真激活（4 / 8 / 16 / 32）：每档激活总耗时、平均单应用耗时、常驻 RSS、逐实例命令；32 是容量探针，16 是交付门禁支撑数 |
| S | S2 | N 个已激活实例并发命令扇出（N 条同时下发，看总耗时 / 单命令长尾 / 失败数） |
| S | S3 | N 个实例顺序停用：总耗时、停用后 RSS、回收量、是否仍有 activated 实例 |
| F | F1 | `plugin_list_loaded` 的同源清单（`list_plugins`）在 4 / 16 / 64 / 256 个插件下的取数耗时与 JSON 体积（Tauri IPC 前的数据面） |

S1/S2/S3 的 N 个实例使用 bench 夹具（同一组件多属主），F1 使用 TS-only 清单
（无 wasm 实例）；两者都走生产同形装配，把「运行时实例」与「清单体积」两个
变量隔开。真实前端渲染 / 多任务预览不在无头进程里测，见「边界」。

### apps（真实随包应用）

| 组 | 场景 | 覆盖 |
| --- | --- | --- |
| H | H1 | 四应用冷激活（含组件实例化） |
| I | I1–I6 | ai-chatbox：JSONL 消息读改写（1 KiB / 64 KiB）、消息读取、会话索引、会话更新与删除 |
| J | J1–J4 | agent-hub：供应商预设更新、状态读取、技能读取、用量统计 |
| K | K1–K6 | BedCode：会话配置 upsert/list、预设任务创建/删除、任务历史列表与统计、agent 能力清单 |
| L | L1–L5 | file-transfer：设置、设备快照写读（64 / 512 条）、任务与历史列表 |
| R | R1 | 四应用常驻后的当前 RSS / 峰值 RSS（最后执行，必须最后） |

冒烟子集：`A1 D2 S1 F1 H1 I1 J1 K1 L1 R1`（S2/S3 只在 `--full` 下跑）。

## 数量级门禁

门禁只挡「慢了/胖了一个数量级」，不是精确回归线：机器差异不该让基准变红，
但链路退化必须立刻可见。当前门限：

| 测点 | 门限 |
| --- | --- |
| S1 同时常驻（full 的 16 档） | 16 个全部 activated；总激活 < 60 s；RSS < 4 GiB；单命令中位数 < 10 ms |
| S1 同时常驻（32 档） | 只记录容量探针，不设门禁（能跑到多少记多少） |
| S2 并发扇出 | 总耗时 < 1 s；单命令中位数 < 50 ms；失败数 = 0 |
| S3 停用回收 | 停用总耗时 < 60 s；停用后 activated 实例 = 0 |
| F1 注册表投影（256 档） | 取数 < 100 ms；JSON < 4 MiB |
| M1 1 MiB echo × 64 并发 | 单次中位数 < 20 ms |
| M1 1 MiB KV × 8 并发 | 单次中位数 < 100 ms |
| M2 队列饱和 | 入队 ≤ 64；拒绝 ≥ 16；非队列满错误 = 0 |
| M3 异步进程 1/4/8 路 | 提交失败 = 0；提交中位数 < 50 ms；全部完成 < 10 s |
| M4 调用模型对照 | 2/4 路总耗时 < 1 s；event-loop / mutex < 10× |
| H1 单应用激活 | < 30 s |
| I1 save-message 1 KiB | < 50 ms |
| I2 save-message 64 KiB | < 200 ms |
| I3 get-messages（64 KiB 行） | < 200 ms |
| I4 list-conversations / I5 save-conversation | < 100 ms / < 50 ms |
| I6 delete-conversation | < 100 ms |
| J1 save-preset / J3 read-skill | < 100 ms |
| J2 get-providers-state / J4 get-usage-stats | < 1 s / < 5 s |
| K1 config upsert / K6 supported-agents | < 100 ms |
| K2 config list / K4 history-list / K5 history-stats | < 500 ms |
| K3 preset-create / preset-delete | < 200 ms |
| L1 get-settings / L3 get-device-snapshot / L4 list-tasks / L5 list-history | < 100 ms |
| L2 save-device-snapshot 64 条 / L2b 512 条 | < 100 ms / < 500 ms |
| R1 四应用常驻 RSS | < 4 GiB |

bridge 各组的门限沿用迁移前口径，见 `src/modules/bridge.rs` 各场景的
`rep.budget(...)` 调用。

## 环境隔离与无头语义

- 每次运行在临时目录建内核库、插件私有库、用户插件目录；应用产物复制后再扫描，
  平台仓 `resources/` 只读。boundary 的 N 个夹具属主 / N 个 TS-only 清单同样
  落临时目录，退出清理。
- apps 模块把进程 `HOME` 指向临时沙箱（应用数据根 = `$HOME/.wasmnest/<app>`），
  退出恢复；bridge 的夹具编译子进程显式使用原始 `HOME`。
- R1 读的是**整进程** RSS：`--module all` 会把 bridge / boundary 模块的运行时
  驻留算进来（boundary 退出会清理环境，但分配器未必立刻归还）；单看四应用口径
  请跑 `--module apps`。
- 无 `AppHandle`：弹窗授权不可用。需要文件目录的应用按生产同款的
  `fs_granted_paths` 预授权，第一方免弹窗目录清单按生产装配注入；依赖弹窗或
  对端推送（如 file-transfer `set-settings`）的命令不在测量范围。
- 激活失败不中断整份报告：失败进 `failures`，退出码非零；依赖该应用的场景记为失败。

## 新增场景

1. 在对应模块的 `all()` 里登记 id / 分组 / 标题 / 是否冒烟；
2. 写一个 `async fn`，用 `call_timed` 采样、`rep.record` 落测点、`rep.budget` 加门禁；
3. 断言回执的关键字段（`success` / `saved` / 数组长度等），不做「有返回即通过」的表面测试；
4. 需要前置数据的场景先做一次不计时的 setup，测点只覆盖目标路径；
5. 破坏性场景每轮自行复位，避免污染后续测点。

## 报告

- 表格：按组输出中位数、p95、min/max；门禁单独打印 PASS/FAIL；
- JSON：含环境快照（机器、rustc、wasmtime 锁定版本、四仓 HEAD 与脏标记）、
  全部样本、门禁与失败项；
- `--compare`：按测点 id 对齐打印变化百分比，跨机器不可比，同机前后对比才有意义；
- 归档报告：落 `reports/`，文件名日期前缀（如
  [`reports/2026-10-21-full-performance-report.md`](reports/2026-10-21-full-performance-report.md)，
  已包含 M 调用矩阵与桌面应用性能评价）。

## 边界

- **应用数没有硬编码上限**：宿主编排层不设 `max_apps` 常量；S1 的 16 是交付
  支撑下限（当前随包 4 个应用的 4 倍余量），32 是容量探针（观察到哪算哪，
  不把「超过 16」本身当回归）。真实上限由 RSS / 调度决定，S1 记录的是本机
  本次实测值，跨机器不可直接比较。
- **S1 是合成夹具，不是真实应用内存**：同一 bench 组件多属主只保留桥接原语，
  单实例增量远小于真实业务应用；四个真实应用同时常驻的 RSS 看 `R1`。要扩大
  真实应用数量，需要按应用产物复制属主并逐应用预授权，属于后续扩展，不用
  合成数字冒充。
- **前端显示边界在平台仓**：`perf-tests` 无 WebView，`plugin_list_loaded` 的
  数据面由 F1 覆盖；多任务面板同时挂 N 张应用卡 / N 份 `mountPreview`、3 s
  超时回退与全量卸载，由平台仓 vitest
  `src/__tests__/shell/appPreviewScale.test.ts`（32 个预览）与
  `useShellApps.test.ts`（256 条清单投影）锁定。真实 WebView 的帧率 / 内存
  仍需 GUI e2e，不在本仓的无头 release 数字里。
- 第二批：`services`（宿主服务面）与 `crossend`（跨端互连性能）。
- 移动端应用产物、file-transfer 原生核（rust/core）单测性能暂未纳入。
- 平台仓的夹具、打包脚本（`bench:zip`）与 e2e 用例仍在平台仓；本工程只接管宿主侧驱动。
