# 2026-10-21 全量性能测试报告

> 本报告已整合 M 组（wasm-core 调用矩阵）内容，数据以包含 M 组的 release 全量运行为准。

## 摘要

- **执行范围**：`perf-harness --full`，模块 `all`（bridge + boundary + apps）
- **结果**：185 个测点，69 条数量级门禁全部 PASS，`failures = 0`
- **总耗时**：9.40 s（不含构建与夹具编译）
- **构建口径**：release
- **结论**：当前四个随包桌面应用与本地命令链路满足交互式桌面应用的性能底线，常用命令大多在 5 ms 内，1 MiB 级操作在 3–13 ms，四应用单独常驻约 111 MiB；主要限制是**同实例串行**、**事件突发背压**和**大 KV 写入成本**，不代表对任意第三方插件或重负载的无限保证。

## 环境与执行

| 项 | 值 |
| --- | --- |
| 机器 | AMD Ryzen 7 5800H ×16 / 13.5 GiB RAM |
| 系统 | Linux Deepin 25，6.18.48-amd64-desktop-rolling |
| 工具链 | rustc 1.98.1 / wasmtime 48.0.7 |
| 模式 | full，每测点 5 次取中位数 |
| 仓库 | `wasmnest@b84cb67+dirty` / `wasmnest-lib@3019898+dirty` / `wasmnest-apps@c16be2c` / `wasmnest-test@bc1d86a+dirty` |
| 命令 | `perf-harness --full --json target/perf/results/perf-full-2026-10-21-with-m.json` |
| 全量 JSON | `target/perf/results/perf-full-2026-10-21-with-m.json` |
| M 组 JSON | `target/perf/results/perf-m-2026-10-21.json` |
| 全量日志 | `/tmp/perf-full-2026-10-21-with-m.log` |
| M 组日志 | `/tmp/perf-m-2026-10-21.log` |

## 测试范围

| 模块 | 场景数 | 内容 |
| --- | ---: | --- |
| bridge | 27 | A–G 桥接基线/原语/事件/互调/流式/异步/并发，M 调用矩阵 |
| boundary | 4 | 同时常驻规模、并发扇出、停用回收、宿主 → 前端注册表投影 |
| apps | 24 | 四个随包真实应用的装载、本地命令链路、常驻 RSS |

## 结果总览

| 结果 | 数量 |
| --- | ---: |
| 测点 | 185 |
| 门禁 PASS | 69 |
| 门禁 FAIL | 0 |
| 运行失败项 | 0 |

## Bridge 模块

### A 基线

| 测点 | 中位数 | p95 |
| --- | ---: | ---: |
| nop 往返 | 20.23 µs | 28.26 µs |
| echo 1 MiB | 2.64 ms | 3.02 ms |
| echo 每字节成本（1 MiB） | 2.52 ns/B | — |
| churn 1 MiB（墙钟） | 2.85 ms | 3.09 ms |
| churn 1 MiB（guest 内） | 2.81 ms | 3.01 ms |

命令通道空载约 20 µs；1 MiB 双向回传约 2.6 ms，主要成本是编解码与内存搬运。

### B 原语

| 测点 | 中位数 |
| --- | ---: |
| host-log 单次 | 0.66 µs |
| storage set+get 1 KiB | 470 µs |
| storage set+get 64 KiB | 969 µs |
| storage set+get 1 MiB | 9.26 ms |
| AEAD 64 KiB / 1 MiB | 117.1 / 128.9 MiB/s |
| fs 写+读 1 MiB | 1.85 ms |
| 私有库 100 行 × 1 KiB（每行） | 205 µs |
| execute-batch 64 条 | 273 µs |

`storage` 1 MiB 约 8.8 ns/B，明显高于 echo 的 2.52 ns/B——大对象不适合塞 KV，符合现有设计判断。

### C 事件

| 测点 | 中位数 |
| --- | ---: |
| emit 256 KiB | 732.18 µs |
| 1 MiB 分块 emit（4 KiB/块，总） | 3.73 ms |
| 1 MiB 分块 emit（16 KiB/块，总） | 3.09 ms |
| 1 MiB 分块 emit（64 KiB/块，总） | 2.88 ms |
| 1 MiB 分块 emit（256 KiB/块，总） | 2.82 ms |

### D 插件互调与总线

| 测点 | 中位数 |
| --- | ---: |
| 总线 JSON 单次发布 | 9.55 µs |
| 总线二进制吞吐 | 873.4 MiB/s |
| 互调 api-call 小载荷 | 336.5 µs |
| 互调 api-call 64 KiB reply | 721.7 µs |
| 512 条突发送达率 | 28.7%（背压丢弃观察项，非门禁） |

### E 流式

| 测点 | 中位数 |
| --- | ---: |
| PTY ring-fetch 1 MiB（4 KiB/批） | 25.44 ms |
| PTY ring-fetch 1 MiB（16 KiB/批） | 23.25 ms |
| PTY ring-fetch 1 MiB（64 KiB/批） | 23.70 ms |
| PTY + 二进制再转发（每次） | 398 µs |
| HTTP 非流式 1 MiB / 8 MiB | 198.4 / 184.4 MiB/s |

PTY 1 MiB 拉取中 guest 内耗时占比约 1–2%，主要成本在宿主 ring-fetch 与批量搬运。8 MiB HTTP 吞吐已接近 1 MiB 档，门禁为 ≥ 5 MiB/s，余量充足。

### F 异步形态

| 测点 | 中位数 |
| --- | ---: |
| process.run-sync（阻塞） | 3.33 ms |
| process.run 提交往返 | 512 µs |
| process.run 终态回调延迟 | 3.32 ms |
| timer.register 注册往返 | 375 µs |
| 定时器 2.6 s 内触发次数 | 2 次 |

### G 并发

| 测点 | 中位数 |
| --- | ---: |
| 1 / 2 / 4 / 8 路并发 nop（每次） | 37 / 29 / 19 / 23 µs |
| 慢调用自身（1 MiB storage） | 11.76 ms |
| 慢调用在途时 nop | 11.84 ms |
| **阻塞倍数** | **114.1×** |
| 2 / 4 路并发慢调用（总耗时） | 19.74 / 42.20 ms |

阻塞倍数说明同实例慢调用会串行化其他交互，是当前实例锁模型的已知特性，非本次回归。

### M 调用矩阵

> 以下为全量 release 运行中的 M 组数据；M 组单独运行 39 测点 / 20 门禁也全部通过（2.86 s）。

#### M1 单 Store 串行载荷矩阵（摊每次）

| 载荷 | echo 1 并发 | echo 8 并发 | echo 64 并发 | storage 1 并发 | storage 8 并发 |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 0 B | 117.29 µs | 43.68 µs | 40.99 µs | — | — |
| 1 KiB | 36.62 µs | 32.96 µs | 29.83 µs | 538.10 µs | 428.04 µs |
| 64 KiB | 248.06 µs | 241.67 µs | 220.02 µs | 1.10 ms | 1.04 ms |
| 1 MiB | 4.54 ms | 3.38 ms | 2.73 ms | 12.27 ms | 12.46 ms |

- 回显/读回失败数：0
- 0 B 的 1 并发档受首个矩阵组合的冷启动/调度抖动影响偏高；同尺寸其它档在 40 µs 左右。
- 小载荷由固定开销主导；1 MiB echo 并发升高后单次反而略降（固定开销被摊薄）。
- 1 MiB storage 基本不随并发下降，SQLite/IO 与序列化成本占主导。
- 类型维度目前覆盖“命令 JSON 字符串 vs KV 字符串”；binary/array 需要扩展夹具，未纳入。

#### M2 event-loop 属主队列饱和

先让一个 4 MiB KV 慢调用占住属主，再一次性投递 `64 + 16 = 80` 条 nop。

| 测点 | 结果 |
| --- | ---: |
| 80 条总耗时 | 60.97 ms |
| 入队并服务 | **64** |
| 队列满拒绝 | **16** |
| 非队列满失败 | 0 |
| 入队请求延迟 | 60.88 ms（等待慢调用释放） |
| 拒绝请求延迟 | **322.47 µs** |

结论：`OWNER_QUEUE_CAP = 64` 的边界行为与 `owner_e2e.rs::owner_queue_full_is_explicit_failure` 一致；超限请求立即 fail-visible，不无限缓冲、不静默丢弃。

#### M3 异步进程并发

| 并发 | 提交中位数 | 全部完成 | 提交失败 |
| ---: | ---: | ---: | ---: |
| 1 | 750.75 µs | 6.61 ms | 0 |
| 4 | 1.56 ms | 8.13 ms | 0 |
| 8 | 3.14 ms | 11.43 ms | 0 |

提交延迟随并发增长（固定开销 + 调度），但 8 路全部完成仍在 12 ms 内；异步形态没有把提交串行成“等每个进程结束”。

#### M4 mutex vs event-loop 同负载对照

| 模型 | 2 路 1 MiB KV 总耗时 | 4 路 1 MiB KV 总耗时 |
| --- | ---: | ---: |
| mutex | 23.68 ms | 52.07 ms |
| event-loop | 19.91 ms | 37.86 ms |

- `event-loop / mutex`（4 路）：**0.727×**
- 两种模型都不放开同实例并行（wasmtime 实例门），4 路总耗时仍近似 4× 单次。
- 本次负载下 event-loop 略快，差值与模型投递/唤醒开销有关，不代表业务链路整体收益。

## Boundary 模块

### S1 同时常驻规模

| N | 实际 activated | 顺序激活总耗时 | 单应用平均 | 常驻 RSS（整进程） | 单命令中位数 |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 4 | 4 | 1.23 ms | 0.307 ms | 383.55 MiB | 108.99 µs |
| 8 | 8 | 2.42 ms | 0.303 ms | 383.69 MiB | 79.37 µs |
| 16 | 16 | 4.36 ms | 0.272 ms | 383.97 MiB | 67.91 µs |
| 32 | 32 | 8.18 ms | 0.255 ms | 384.54 MiB | 103.57 µs |

- 32 个全部激活成功，本次容量探针达到上限。
- 16 个是交付门禁支撑下限；32 个只观察，不设门禁。
- RSS 是整进程口径，all 模式包含 bridge / M 组的运行时残留；单独跑 boundary 时 32 档约 155 MiB。

### S2 / S3 / F1

| 测点 | 结果 |
| --- | ---: |
| 32 实例并发扇出（一轮） | 0.27 ms |
| 并发单命令中位数 | 124.30 µs |
| 并发失败数 | 0 |
| 32 实例顺序停用 | 3.67 ms |
| 停用后仍 activated | 0 |
| `list_plugins` 256 个 | 4.15 ms |
| 注册表 JSON 256 个 | 159.25 KiB |
| 平均每插件清单 | 637 B |

## Apps 模块

### 激活（H1）

| 应用 | 激活耗时 |
| --- | ---: |
| AI Chatbox | 1.28 ms |
| Agent Hub | 4.17 ms |
| BedCode | 12.33 ms |
| File Transfer | 1.88 ms |

### 本地命令链路

| 应用 | 测点 | 耗时 |
| --- | --- | ---: |
| AI Chatbox | save-message 1 KiB / 64 KiB | 1.01 / 1.49 ms |
| AI Chatbox | get-messages（64 KiB 行） | 0.99 ms |
| AI Chatbox | list-conversations / save-conversation | 0.51 / 1.94 ms |
| AI Chatbox | delete-conversation | 1.30 ms |
| Agent Hub | save-preset / providers-state | 4.01 / 2.93 ms |
| Agent Hub | read-skill / usage-stats | 0.42 / 1.70 ms |
| BedCode | config upsert / list | 0.52 / 0.24 ms |
| BedCode | preset-create / delete | 0.48 / 0.37 ms |
| BedCode | history-list / stats / supported-agents | 0.55 / 0.26 / 0.079 ms |
| File Transfer | get-settings / get-device-snapshot | 0.40 / 0.84 ms |
| File Transfer | save-device-snapshot 64 / 512 条 | 1.05 / 5.80 ms |
| File Transfer | list-tasks / list-history | 0.066 / 0.048 ms |

### 内存（R1）

| 测点 | 结果 |
| --- | ---: |
| 四应用常驻 RSS（`--module all`） | 420.04 MiB |
| 进程峰值 RSS（`--module all`） | 420.04 MiB |
| **四应用单独口径 RSS（release）** | **111.09 MiB** |
| **四应用单独口径峰值 RSS（release）** | **114.75 MiB** |
| 单独口径相对装配前增量 | 34.7 MiB |

`--module all` 的 420 MiB 包含 bridge / boundary / M 前序运行时残留；评价桌面应用自身内存时看单独口径 **111 MiB / 峰值 115 MiB**。

## 门禁明细

全部 69 条门禁通过。余量最窄的性能类门禁：

| 门禁 | 实测 / 门限 | 余量 |
| --- | --- | ---: |
| C1 256 KiB 事件单发 | 732.2 µs / 5 ms | 6.8× |
| M1 1 MiB echo × 64 并发单次 | 2.73 ms / 20 ms | 7.3× |
| M1 1 MiB KV × 8 并发单次 | 12.46 ms / 100 ms | 8.0× |
| R1 四应用常驻 RSS（all 口径） | 420.0 MiB / 4 GiB | 9.8× |
| S1 16 个常驻 RSS | 384.0 MiB / 4 GiB | 10.7× |
| M4 event-loop / mutex | 0.727× / 10× | 13.8× |
| M3 8 路提交 | 3.14 ms / 50 ms | 15.9× |
| M4 4 路总耗时 | 52.1 ms / 1 s | 19.2× |

计数类门禁（`S1.active-16`、`M2.ok`、`M2.rejected`）是“必须达到/不得超过”的边界，不是性能余量。

## 评价：能否满足桌面应用开发

### 结论

**可以满足当前 WasmNest 桌面端（四个随包应用、本地优先命令链路）的性能底线，且交互延迟与内存都有较大余量；但不能理解为对任意第三方插件、任意大载荷或重网络负载的无限保证。**

### 支撑证据

1. **交互延迟**：四个应用激活 1.3–12.3 ms；本地命令多数 0.05–4 ms，最慢的 512 条设备快照 5.8 ms；都在桌面交互可接受范围内。
2. **大载荷**：1 MiB 命令回传 2.6–4.5 ms；1 MiB 文件写读 1.85 ms；1 MiB KV 9.3–13 ms；256 KiB 事件单发 0.73 ms。
3. **内存**：四应用单独常驻 111 MiB / 峰值 115 MiB；32 个合成实例全进程 385 MiB。对 8 GiB 以上桌面机器余量充足。
4. **并发与规模**：32 个实例同时 activated；32 路跨实例扇出 0.27 ms；8 路异步进程提交 3.1 ms、全部完成 11.4 ms；属主队列 64 条内不丢请求。
5. **门禁余量**：69/69 通过；最窄的性能门禁 C1 仍有 6.8× 余量。

### 不满足 / 需要设计规避的点

1. **同实例串行是硬约束**：G2 阻塞倍数 114×，1 MiB KV 慢调用会让同实例其他命令等待约 11.8 ms（p95 19.5 ms）；event-loop 也不放开同实例并行（M4）。如果应用把长 HTTP、PTY 或大 DB 操作放在同一个实例里，UI 动作会被拖住。
2. **事件突发会丢**：512 条突发总线消息送达率 28.7%，队列满即丢弃（设计如此）。事件不是可靠消息队列；关键状态要用快照/查询自愈，不能依赖每条事件都到达。
3. **大 KV 写入成本明显**：1 MiB KV 约 9–13 ms，远高于 1 MiB 文件写读 1.85 ms。对话 JSONL、日志等大对象应走文件/流式，不要塞 KV。
4. **终端高吞吐仍有压力**：1 MiB PTY 拉取约 23 ms（约 44 MiB/s）；高频终端输出可能掉帧或延迟，需要批量/背压/渲染侧消费优化。
5. **前端真实渲染未测**：C1/F1 只覆盖宿主数据面；WebView 的 DOM、IPC、预览挂载由平台 e2e/vitest 覆盖，真实帧率与长时间运行内存仍需 GUI 测试。
6. **单机、短时**：结果来自一台 Ryzen 7 5800H / 13.5 GiB 机器，未覆盖低端机、Windows/macOS、移动端、24 小时 soak/泄漏；跨机器数字不可直接比较。

### 适用判断

- **适合**：本地优先的桌面应用（对话、配置、任务历史、设备快照、中等吞吐终端），以及命令延迟要求 16–50 ms 级的交互。
- **需额外设计**：重度 AI 流式、超大文件/媒体处理、高频事件推送、批量自动化（单实例 pending >64）——需要拆实例、异步进程、分片/流式或前端合并。
- **不能由本报告证明**：功能正确性、安全性、权限边界、跨平台一致性、长时间稳定性。

## 复现

```bash
cd perf-tests
cargo build --release --bin perf-harness

# 全量（含 M 组）
../target/perf/release/perf-harness --full \
  --json target/perf/results/perf-full-2026-10-21-with-m.json

# 只跑 M 组
../target/perf/release/perf-harness --full --group M \
  --json target/perf/results/perf-m-2026-10-21.json

# 四应用单独内存口径
../target/perf/release/perf-harness --full --module apps --group R
```

同机前后对比：

```bash
../target/perf/release/perf-harness --full \
  --compare target/perf/results/perf-full-2026-10-21-with-m.json
```

跨机器数字不可直接比较；对比只看同一台机器、同一构建口径下的按测点 id 变化。
