//! 桥接基准 harness · 场景矩阵
//!
//! 每个场景 = 一组贴近桌面端真实业务的测点。分组与 `bench/README.md` 的矩阵一致：
//!
//! - **A 基线**：命令通道空载 / 载荷尺寸扫描 / guest 内部 CPU 下界
//!   （A2 − A3 = 载荷跨越 WASM 边界的净成本）
//! - **B 原语往返**：storage / crypto / fs / 私有库 / host-log 的固定开销与每字节成本
//! - **C 事件推送**：wasm → 宿主 → 前端（无头下测「guest 序列化 + import 桥」，
//!   IPC 与前端派发由 e2e webview 层补齐）
//! - **D wasm ↔ wasm**：总线 JSON / 二进制、互调 JSON-RPC 阻塞调用
//! - **E 流式**：PTY 输出环游标拉取（终端输出流）+ 非流式 HTTP 大响应
//! - **F 异步形态**：`process.run`（事件回调）vs `process.run-sync`（阻塞）、
//!   周期定时器回调
//! - **G 并发**：同一实例的并发命令扇出（实例锁串行化的直接证据），
//!   以及「慢调用是否堵死同实例全部交互」的量化（G2）
//!
//! 每个测点都带**行为断言**（收到的字节数、调用次数、计数增量），不只是计时——
//! 基准数字建立在「确实跑对了」之上（同 `unit-test-discipline` 的门禁精神）。

use crate::report::Report;
use crate::report::{arg_usize, mib_per_sec, micros, ns_per_byte, Cmp};
use crate::support::bridge::{BenchEnv, BUS_BIN_TOPIC, BUS_JSON_TOPIC};
use serde_json::json;

/// 场景执行体（boxed future：场景函数都是 async，但列表要能按值持有）
pub type ScenarioRun<'a> = std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + 'a>>;

/// 一个场景
pub struct Scenario {
    /// 稳定 id（`--list` / `--group` 用）
    pub id: &'static str,
    /// 分组字母
    pub group: &'static str,
    /// 人读标题
    pub title: &'static str,
    /// 冒烟模式（无参数 `cargo test`）也跑——只放 3 个数量级门禁探针
    pub smoke: bool,
    /// 执行体
    pub run: for<'a> fn(&'a BenchEnv, usize, &'a mut Report) -> ScenarioRun<'a>,
}

/// 全部场景（`--list` 输出顺序 = 运行顺序）
pub fn all() -> Vec<Scenario> {
    vec![
        Scenario {
            id: "A1",
            group: "A",
            title: "命令通道空载（nop）",
            smoke: true,
            run: |env, iters, rep| Box::pin(a1_nop(env, iters, rep)),
        },
        Scenario {
            id: "A2",
            group: "A",
            title: "命令载荷尺寸扫描（echo 0 B ~ 1 MiB）",
            smoke: false,
            run: |env, iters, rep| Box::pin(a2_echo(env, iters, rep)),
        },
        Scenario {
            id: "A3",
            group: "A",
            title: "guest 内部 CPU 下界（churn，A2 的对照）",
            smoke: false,
            run: |env, iters, rep| Box::pin(a3_churn(env, iters, rep)),
        },
        Scenario {
            id: "B1",
            group: "B",
            title: "host-log 往返（最廉价的 import 标尺）",
            smoke: false,
            run: |env, iters, rep| Box::pin(b1_log(env, iters, rep)),
        },
        Scenario {
            id: "B2",
            group: "B",
            title: "插件 KV 大 value 往返（storage）",
            smoke: false,
            run: |env, iters, rep| Box::pin(b2_storage(env, iters, rep)),
        },
        Scenario {
            id: "B3",
            group: "B",
            title: "AEAD 加解密（host-crypto）",
            smoke: false,
            run: |env, iters, rep| Box::pin(b3_crypto(env, iters, rep)),
        },
        Scenario {
            id: "B4",
            group: "B",
            title: "文件写 + 读（host-fs）",
            smoke: false,
            run: |env, iters, rep| Box::pin(b4_fs(env, iters, rep)),
        },
        Scenario {
            id: "B5",
            group: "B",
            title: "插件私有库批量写 + 读回",
            smoke: false,
            run: |env, iters, rep| Box::pin(b5_db(env, iters, rep)),
        },
        Scenario {
            id: "B6",
            group: "B",
            title: "事务批执行（execute-batch）",
            smoke: false,
            run: |env, iters, rep| Box::pin(b6_db_batch(env, iters, rep)),
        },
        Scenario {
            id: "C1",
            group: "C",
            title: "事件单发大包（wasm → 宿主 → 前端）",
            smoke: false,
            run: |env, iters, rep| Box::pin(c1_emit(env, iters, rep)),
        },
        Scenario {
            id: "C2",
            group: "C",
            title: "事件分块流式（同体量分块 emit）",
            smoke: false,
            run: |env, iters, rep| Box::pin(c2_emit_chunk(env, iters, rep)),
        },
        Scenario {
            id: "D1",
            group: "D",
            title: "wasm → wasm 总线 JSON 通道",
            smoke: false,
            run: |env, iters, rep| Box::pin(d1_bus_json(env, iters, rep)),
        },
        Scenario {
            id: "D1b",
            group: "D",
            title: "总线突发发布的背压丢弃率",
            smoke: false,
            run: |env, iters, rep| Box::pin(d1b_bus_backpressure(env, iters, rep)),
        },
        Scenario {
            id: "D2",
            group: "D",
            title: "wasm → wasm 总线二进制通道",
            smoke: true,
            run: |env, iters, rep| Box::pin(d2_bus_binary(env, iters, rep)),
        },
        Scenario {
            id: "D3",
            group: "D",
            title: "插件互调（小载荷 JSON-RPC 阻塞调用）",
            smoke: false,
            run: |env, iters, rep| Box::pin(d3_api_call(env, iters, rep)),
        },
        Scenario {
            id: "D4",
            group: "D",
            title: "插件互调（大载荷 reply 回传）",
            smoke: false,
            run: |env, iters, rep| Box::pin(d4_api_call_big(env, iters, rep)),
        },
        Scenario {
            id: "E1",
            group: "E",
            title: "终端输出流（ring-fetch 游标拉取，批量曲线）",
            smoke: false,
            run: |env, iters, rep| Box::pin(e1_pty_stream(env, iters, rep)),
        },
        Scenario {
            id: "E2",
            group: "E",
            title: "终端输出流 + 二进制再转发（消费即分发）",
            smoke: false,
            run: |env, iters, rep| Box::pin(e2_pty_forward(env, iters, rep)),
        },
        Scenario {
            id: "E3",
            group: "E",
            title: "HTTP 非流式大响应（host-http 客户端域）",
            smoke: false,
            run: |env, iters, rep| Box::pin(e3_http(env, iters, rep)),
        },
        Scenario {
            id: "F1",
            group: "F",
            title: "进程执行：同步阻塞 vs 异步事件",
            smoke: false,
            run: |env, iters, rep| Box::pin(f1_process(env, iters, rep)),
        },
        Scenario {
            id: "G1",
            group: "G",
            title: "同实例并发命令扇出（实例锁串行化）",
            smoke: false,
            run: |env, iters, rep| Box::pin(g1_concurrency(env, iters, rep)),
        },
        Scenario {
            id: "G2",
            group: "G",
            title: "慢调用是否堵死同实例全部交互",
            smoke: false,
            run: |env, iters, rep| Box::pin(g2_slow_call_blocks_instance(env, iters, rep)),
        },
        Scenario {
            id: "F2",
            group: "F",
            title: "周期定时器回调（host-timer）",
            smoke: false,
            run: |env, iters, rep| Box::pin(f2_timer(env, iters, rep)),
        },
    ]
}

/// 运行桥接模块：按过滤条件选场景 → 建环境 → 逐个执行 → 收尾。
///
/// 场景失败不中断后续场景（一条链路坏掉不应吞掉整份报告），失败项进
/// `rep.failures`，最终由入口统一决定退出码。
pub async fn run(
    full: bool,
    iters: usize,
    group_filter: Option<&str>,
    rep: &mut Report,
) -> anyhow::Result<()> {
    let scenarios: Vec<Scenario> = all()
        .into_iter()
        .filter(|s| match group_filter {
            None => true,
            Some(g) => s.group.eq_ignore_ascii_case(g) || s.id.contains(g),
        })
        .filter(|s| full || s.smoke)
        .collect();

    if scenarios.is_empty() {
        println!("[bridge] 无匹配场景（group={group_filter:?}, full={full}）");
        return Ok(());
    }

    let env = crate::support::bridge::build_env(full).await?;
    println!("[bridge] 环境就绪：{}", env.describe());

    for scenario in scenarios {
        let t0 = std::time::Instant::now();
        match (scenario.run)(&env, iters, rep).await {
            Ok(()) => {}
            Err(e) => {
                eprintln!("[bridge] 场景 {} 失败: {e:#}", scenario.id);
                rep.failures.push(format!("{}: {e:#}", scenario.id));
            }
        }
        println!(
            "[bridge] ── {} 完成（{:.0} ms）",
            scenario.id,
            t0.elapsed().as_secs_f64() * 1e3
        );
    }

    env.shutdown().await;
    Ok(())
}

/// `--list`：稳定 id / 分组 / 标题
pub fn list() -> Vec<(&'static str, &'static str, &'static str)> {
    all()
        .into_iter()
        .map(|s| (s.id, s.group, s.title))
        .collect()
}

/// 取测点的 guest 内部耗时（`*Nanos` 字段，缺省 0）
fn guest_nanos(v: &serde_json::Value) -> f64 {
    v.get("guestNanos").and_then(|x| x.as_u64()).unwrap_or(0) as f64
}

// ==================== A · 基线 ====================

/// A1：命令通道空载。一次完整「宿主 → guest → 宿主」往返，无原语调用。
/// 这是所有场景的地板值，也是 `plugin_invoke` 命令面的固定开销下界。
async fn a1_nop(env: &BenchEnv, iters: usize, rep: &mut Report) -> anyhow::Result<()> {
    // 预热（首调含 wasmtime 首次进入 + 序列化器初始化）
    env.call_timed("bench.nop", json!({})).await?;
    let mut samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let (_, d) = env.call_timed("bench.nop", json!({})).await?;
        samples.push(micros(d));
    }
    let median = median(&samples);
    rep.record("A1.nop", "A 基线", "nop 往返", "µs", samples, "命令面固定开销下界");
    rep.budget("A1", "nop 往返不得数量级变慢", median, 3000.0, "µs", Cmp::Less);
    Ok(())
}

/// A2：载荷尺寸扫描。命令总耗时 = 通道固定开销 + 2×载荷编解码 + guest 分配。
async fn a2_echo(env: &BenchEnv, iters: usize, rep: &mut Report) -> anyhow::Result<()> {
    let mut worst = 0.0f64;
    for bytes in [0usize, 1024, 65536, 262144, 1048576] {
        env.call_timed("bench.echo", json!({ "bytes": bytes })).await?;
        let mut samples = Vec::with_capacity(iters);
        for _ in 0..iters {
            let (v, d) = env.call_timed("bench.echo", json!({ "bytes": bytes })).await?;
            anyhow::ensure!(
                arg_usize(&v, "bytes") == bytes,
                "echo 回显体量不符: {} != {bytes}",
                arg_usize(&v, "bytes")
            );
            samples.push(micros(d));
        }
        let m = median(&samples);
        if bytes == 1048576 {
            worst = m;
        }
        let median_us = median(&samples);
        rep.record(
            &format!("A2.echo-{bytes}"),
            "A 基线",
            &format!("echo {bytes} B"),
            "µs",
            samples,
            "同步返回大载荷（含编解码）",
        );
        if bytes > 0 {
            // 每字节成本：「该不该把大对象切小」的唯一决策量
            let nsb = ns_per_byte(std::time::Duration::from_secs_f64(median_us / 1e6), bytes as f64);
            rep.record(
                &format!("A2.echo-nsB-{bytes}"),
                "A 基线",
                &format!("echo 每字节成本（{bytes} B 档）"),
                "ns/B",
                vec![nsb],
                "含 guest 生成 + 双向编解码",
            );
        }
    }
    rep.budget("A2", "1 MiB 同步回传不得数量级变慢", worst, 60000.0, "µs", Cmp::Less);
    Ok(())
}

/// A3：guest 内部 CPU 下界（同产出量、不跨边界回传）。
/// 与 A2 同尺寸之差 ≈ 载荷跨越 WASM 边界的净成本。
async fn a3_churn(env: &BenchEnv, iters: usize, rep: &mut Report) -> anyhow::Result<()> {
    for bytes in [1024usize, 65536, 262144, 1048576] {
        env.call_timed("bench.churn", json!({ "bytes": bytes })).await?;
        let mut total = Vec::with_capacity(iters);
        let mut inner = Vec::with_capacity(iters);
        for _ in 0..iters {
            let (v, d) = env.call_timed("bench.churn", json!({ "bytes": bytes })).await?;
            total.push(micros(d));
            inner.push(guest_nanos(&v) / 1000.0);
        }
        rep.record(
            &format!("A3.churn-total-{bytes}"),
            "A 基线",
            &format!("churn {bytes} B（墙钟）"),
            "µs",
            total,
            "guest 内产字节，不回传",
        );
        rep.record(
            &format!("A3.churn-inner-{bytes}"),
            "A 基线",
            &format!("churn {bytes} B（guest 内）"),
            "µs",
            inner,
            "wasi 时钟分段计时",
        );
    }
    Ok(())
}

// ==================== B · 单一原语往返 ====================

/// B1：host-log 往返 —— 一次 import 调用（无序列化、无分配）的标尺
async fn b1_log(env: &BenchEnv, iters: usize, rep: &mut Report) -> anyhow::Result<()> {
    let count = 200usize;
    env.call_timed("bench.log", json!({ "count": count, "bytes": 64 }))
        .await?;
    let mut samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let (_, d) = env
            .call_timed("bench.log", json!({ "count": count, "bytes": 64 }))
            .await?;
        samples.push(micros(d) / count as f64);
    }
    let m = median(&samples);
    rep.record(
        "B1.log",
        "B 原语",
        "host-log ×200（每次）",
        "µs",
        samples,
        "import 桥固定开销标尺",
    );
    rep.budget("B1", "单次 host-log 不得数量级变慢", m, 500.0, "µs", Cmp::Less);
    Ok(())
}

/// B2：插件 KV 大 value（会话注解 / 设置项等业务真源的形态）
async fn b2_storage(env: &BenchEnv, iters: usize, rep: &mut Report) -> anyhow::Result<()> {
    for bytes in [1024usize, 65536, 1048576] {
        env.call_timed("bench.storage-rt", json!({ "key": "bench.payload", "bytes": bytes }))
            .await?;
        let mut samples = Vec::with_capacity(iters);
        for _ in 0..iters {
            let (v, d) = env
                .call_timed("bench.storage-rt", json!({ "key": "bench.payload", "bytes": bytes }))
                .await?;
            anyhow::ensure!(
                arg_usize(&v, "readBytes") == bytes,
                "storage 回读体量不符: {} != {bytes}",
                arg_usize(&v, "readBytes")
            );
            samples.push(micros(d));
        }
        let median_us = median(&samples);
        rep.record(
            &format!("B2.storage-{bytes}"),
            "B 原语",
            &format!("storage set+get {bytes} B"),
            "µs",
            samples,
            "插件私有 KV（SQLite）",
        );
        let nsb = ns_per_byte(std::time::Duration::from_secs_f64(median_us / 1e6), bytes as f64);
        rep.record(
            &format!("B2.storage-nsB-{bytes}"),
            "B 原语",
            &format!("storage 每字节成本（{bytes} B 档）"),
            "ns/B",
            vec![nsb],
            "与 A2 同口径对比：KV 存大对象是否划算",
        );
    }
    Ok(())
}

/// B3：AEAD 加解密（链路加密面：HTTP 信封 / 传输分块）
async fn b3_crypto(env: &BenchEnv, iters: usize, rep: &mut Report) -> anyhow::Result<()> {
    for bytes in [65536usize, 1048576] {
        env.call_timed("bench.crypto-rt", json!({ "bytes": bytes })).await?;
        let mut samples = Vec::with_capacity(iters);
        for _ in 0..iters {
            let (v, d) = env.call_timed("bench.crypto-rt", json!({ "bytes": bytes })).await?;
            anyhow::ensure!(
                arg_usize(&v, "plainBytes") == bytes,
                "解密回明文体量不符: {}",
                arg_usize(&v, "plainBytes")
            );
            samples.push(mib_per_sec(d, bytes as f64));
        }
        rep.record(
            &format!("B3.crypto-{bytes}"),
            "B 原语",
            &format!("AEAD 加解密 {bytes} B"),
            "MiB/s",
            samples,
            "宿主加密引擎（guest→host→guest 双向）",
        );
    }
    Ok(())
}

/// B4：文件写 + 读（host-fs；授权前缀已预置）
async fn b4_fs(env: &BenchEnv, iters: usize, rep: &mut Report) -> anyhow::Result<()> {
    for bytes in [65536usize, 1048576] {
        let path = env.fs_dir.join(format!("bench-fs-{bytes}.txt"));
        let path = path.to_string_lossy().to_string();
        env.call_timed("bench.fs-rt", json!({ "path": path, "bytes": bytes }))
            .await?;
        let mut samples = Vec::with_capacity(iters);
        for _ in 0..iters {
            let (v, d) = env
                .call_timed("bench.fs-rt", json!({ "path": path.clone(), "bytes": bytes }))
                .await?;
            anyhow::ensure!(
                arg_usize(&v, "readBytes") == bytes,
                "fs 回读体量不符: {}",
                arg_usize(&v, "readBytes")
            );
            samples.push(micros(d));
        }
        rep.record(
            &format!("B4.fs-{bytes}"),
            "B 原语",
            &format!("fs 写+读 {bytes} B"),
            "µs",
            samples,
            "含 fs_auth 前缀校验",
        );
    }
    Ok(())
}

/// B5：插件私有库批量写 + 全量读回（业务真源面：会话表 / 传输历史）
async fn b5_db(env: &BenchEnv, iters: usize, rep: &mut Report) -> anyhow::Result<()> {
    for (rows, bytes) in [(100usize, 1024usize), (10, 65536)] {
        env.call_timed("bench.db-bulk", json!({ "rows": rows, "bytes": bytes }))
            .await?;
        let mut samples = Vec::with_capacity(iters);
        for _ in 0..iters {
            let (v, d) = env
                .call_timed("bench.db-bulk", json!({ "rows": rows, "bytes": bytes }))
                .await?;
            anyhow::ensure!(
                arg_usize(&v, "readRows") == rows,
                "db 读回行数不符: {} != {rows}",
                arg_usize(&v, "readRows")
            );
            samples.push(micros(d) / rows as f64);
        }
        rep.record(
            &format!("B5.db-{rows}x{bytes}"),
            "B 原语",
            &format!("私有库 {rows} 行 × {bytes} B（每行）"),
            "µs",
            samples,
            "写 + 全量读回摊到每行",
        );
    }
    Ok(())
}

/// B6：事务批执行（一条语句 vs 一批语句的固定开销差）
async fn b6_db_batch(env: &BenchEnv, iters: usize, rep: &mut Report) -> anyhow::Result<()> {
    for statements in [1usize, 64] {
        env.call_timed("bench.db-batch", json!({ "statements": statements }))
            .await?;
        let mut samples = Vec::with_capacity(iters);
        for _ in 0..iters {
            let (_, d) = env
                .call_timed("bench.db-batch", json!({ "statements": statements }))
                .await?;
            samples.push(micros(d));
        }
        rep.record(
            &format!("B6.batch-{statements}"),
            "B 原语",
            &format!("execute-batch {statements} 条"),
            "µs",
            samples,
            "事务内顺序执行",
        );
    }
    Ok(())
}

// ==================== C · wasm → 宿主 → 前端 ====================

/// C1：事件单发大包。**无头上下文口径**：`host-events.emit` 无 AppHandle 时降级
/// （宿主记 warn 后返回 Ok），故测到的是「guest 序列化 + import 桥 + 宿主解析」，
/// 不含 Tauri IPC 与 webview 派发（后者见 `e2e/specs/bench.spec.ts`）。
async fn c1_emit(env: &BenchEnv, iters: usize, rep: &mut Report) -> anyhow::Result<()> {
    for (bytes, count) in [(1024usize, 200usize), (65536, 20), (262144, 5)] {
        env.call_timed("bench.emit", json!({ "bytes": bytes, "count": count }))
            .await?;
        let mut samples = Vec::with_capacity(iters);
        for _ in 0..iters {
            let (_, d) = env
                .call_timed("bench.emit", json!({ "bytes": bytes, "count": count }))
                .await?;
            samples.push(micros(d) / count as f64);
        }
        let m = median(&samples);
        rep.record(
            &format!("C1.emit-{bytes}"),
            "C 事件",
            &format!("emit {bytes} B ×{count}（每次）"),
            "µs",
            samples,
            "无头口径：不含 IPC/webview",
        );
        if bytes == 262144 {
            rep.budget("C1", "256 KiB 事件单发不得数量级变慢", m, 5000.0, "µs", Cmp::Less);
        }
    }
    Ok(())
}

/// C2：同体量分块流式（output-ack / 分片推送议题的实测依据）：
/// 1 MiB 按不同 chunk 逐块 emit，看每次固定开销如何被摊薄。
async fn c2_emit_chunk(env: &BenchEnv, iters: usize, rep: &mut Report) -> anyhow::Result<()> {
    let total = 1024 * 1024usize;
    for chunk in [4096usize, 16384, 65536, 262144] {
        env.call_timed("bench.emit-chunk", json!({ "bytes": total, "chunkBytes": chunk }))
            .await?;
        let mut per_chunk = Vec::with_capacity(iters);
        let mut whole = Vec::with_capacity(iters);
        for _ in 0..iters {
            let (v, d) = env
                .call_timed("bench.emit-chunk", json!({ "bytes": total, "chunkBytes": chunk }))
                .await?;
            let chunks = arg_usize(&v, "chunks").max(1);
            per_chunk.push(micros(d) / chunks as f64);
            whole.push(micros(d));
        }
        rep.record(
            &format!("C2.chunk-{chunk}"),
            "C 事件",
            &format!("1 MiB 分块 emit（{chunk} B/块，每次）"),
            "µs",
            per_chunk,
            "分块流式摊薄曲线",
        );
        rep.record(
            &format!("C2.chunk-total-{chunk}"),
            "C 事件",
            &format!("1 MiB 分块 emit（{chunk} B/块，总）"),
            "µs",
            whole,
            "同体量总耗时",
        );
    }
    Ok(())
}

// ==================== D · wasm ↔ wasm ====================

/// D1：总线 JSON 通道（主实例发布 → 对端实例订阅侧计数）
///
/// 批量刻意取 **50 < 订阅队列容量 64**（`bus.rs::SUBSCRIBER_QUEUE_CAPACITY`）：
/// 突发发布超出队列容量会被**背压丢弃**，那属 D1b 的测量面，不污染本测点的
/// 「每条都投到」语义。
async fn d1_bus_json(env: &BenchEnv, iters: usize, rep: &mut Report) -> anyhow::Result<()> {
    let (bytes, count) = (1024usize, 50usize);
    env.call_peer("bench.bus-reset", json!({})).await?;
    env.call_timed(
        "bench.bus-publish",
        json!({ "topic": BUS_JSON_TOPIC, "bytes": bytes, "count": count }),
    )
    .await?;
    // 预热轮次的异步投递先落定再归零，避免与计时轮次混计
    let _ = env.wait_peer_recv("jsonCount", count, 5000).await;
    env.call_peer("bench.bus-reset", json!({})).await?;
    let mut samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let (_, d) = env
            .call_timed(
                "bench.bus-publish",
                json!({ "topic": BUS_JSON_TOPIC, "bytes": bytes, "count": count }),
            )
            .await?;
        samples.push(micros(d) / count as f64);
    }
    // 总线投递是异步派发，收讫计数要轮询等（否则读到的是“还没到”）
    let got = env.wait_peer_recv("jsonCount", count, 5000).await?;
    let m = median(&samples);
    rep.record(
        "D1.bus-json",
        "D 互调",
        "总线 publish JSON（每次）",
        "µs",
        samples,
        &format!("对端收讫 {got} 条"),
    );
    rep.budget("D1", "总线 JSON 单次发布不得数量级变慢", m, 2000.0, "µs", Cmp::Less);
    Ok(())
}

/// D2：总线二进制通道（零 JSON 编解码，MB 级载荷）
async fn d2_bus_binary(env: &BenchEnv, iters: usize, rep: &mut Report) -> anyhow::Result<()> {
    let (bytes, count) = (65536usize, 20usize);
    env.call_peer("bench.bus-reset", json!({})).await?;
    env.call_timed(
        "bench.bus-binary",
        json!({ "topic": BUS_BIN_TOPIC, "bytes": bytes, "count": count }),
    )
    .await?;
    let _ = env.wait_peer_recv("binCount", count, 5000).await;
    env.call_peer("bench.bus-reset", json!({})).await?;
    let mut samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let (_, d) = env
            .call_timed(
                "bench.bus-binary",
                json!({ "topic": BUS_BIN_TOPIC, "bytes": bytes, "count": count }),
            )
            .await?;
        samples.push(mib_per_sec(d, (bytes * count) as f64));
    }
    let got = env.wait_peer_recv("binCount", count, 5000).await?;
    let m = median(&samples);
    rep.record(
        "D2.bus-binary",
        "D 互调",
        "总线 publish-binary 1.25 MiB",
        "MiB/s",
        samples,
        &format!("对端收讫 {got} 块"),
    );
    // 冒烟门禁：二进制通道吞吐不得数量级塌陷（预期 ≥ 1 MiB/s）
    rep.budget("D2", "总线二进制吞吐不得低于数量级地板", m, 1.0, "MiB/s", Cmp::Greater);
    Ok(())
}

/// D3：插件互调（小载荷）。`host-api-call` = 发布请求 + 宿主侧静态回复订阅 +
/// oneshot 唤醒，guest 阻塞等待——并发模型议题（插件并发模型专项）关注的那一跳。
async fn d3_api_call(env: &BenchEnv, iters: usize, rep: &mut Report) -> anyhow::Result<()> {
    let repeats = 50usize;
    env.call_timed("bench.api-call", json!({ "bytes": 0, "repeats": repeats }))
        .await?;
    let mut samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let (_, d) = env
            .call_timed("bench.api-call", json!({ "bytes": 0, "repeats": repeats }))
            .await?;
        samples.push(micros(d) / repeats as f64);
    }
    let m = median(&samples);
    rep.record(
        "D3.api-call",
        "D 互调",
        "互调 api-call（每次）",
        "µs",
        samples,
        "JSON-RPC 阻塞往返",
    );
    rep.budget("D3", "互调单次不得数量级变慢", m, 20000.0, "µs", Cmp::Less);
    Ok(())
}

/// D1b：总线背压丢弃率。订阅者队列容量 64（`bus.rs::SUBSCRIBER_QUEUE_CAPACITY`），
/// 一次性突发发布远超容量的消息时，满队列即**丢弃**（不阻塞发布方）。
/// 这里把丢弃率当**数据点**记录，而不是当失败——它是总线背压设计的真实语义。
async fn d1b_bus_backpressure(env: &BenchEnv, iters: usize, rep: &mut Report) -> anyhow::Result<()> {
    let (bytes, burst) = (1024usize, 512usize);
    let mut rates = Vec::with_capacity(iters);
    let mut per_msg = Vec::with_capacity(iters);
    for _ in 0..iters {
        env.call_peer("bench.bus-reset", json!({})).await?;
        let (_, d) = env
            .call_timed(
                "bench.bus-publish",
                json!({ "topic": BUS_JSON_TOPIC, "bytes": bytes, "count": burst }),
            )
            .await?;
        per_msg.push(micros(d) / burst as f64);
        // 留出投递窗口后再读收讫（等满 2s 会把每轮都拖成 2s）
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let recv = env.call_peer("bench.bus-recv", json!({})).await?;
        let got = arg_usize(&recv, "jsonCount") as f64;
        rates.push(got / burst as f64 * 100.0);
    }
    let rate = median(&rates);
    rep.record(
        "D1b.bus-burst-delivery",
        "D 互调",
        "突发 512 条的送达率",
        "%",
        rates,
        &format!("订阅队列容量 64 → 丢弃率 {:.1}%（背压语义，非故障）", 100.0 - rate),
    );
    rep.record(
        "D1b.bus-burst-publish",
        "D 互调",
        "突发 512 条发布（每次）",
        "µs",
        per_msg,
        "发布方不被阻塞",
    );
    Ok(())
}

/// D4：插件互调（大载荷 reply 回传）
async fn d4_api_call_big(env: &BenchEnv, iters: usize, rep: &mut Report) -> anyhow::Result<()> {
    let (bytes, repeats) = (65536usize, 5usize);
    env.call_timed("bench.api-call", json!({ "bytes": bytes, "repeats": repeats }))
        .await?;
    let mut samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let (v, d) = env
            .call_timed("bench.api-call", json!({ "bytes": bytes, "repeats": repeats }))
            .await?;
        anyhow::ensure!(
            arg_usize(&v, "resultBytes") >= bytes,
            "互调 reply 体量偏小: {}",
            arg_usize(&v, "resultBytes")
        );
        samples.push(micros(d) / repeats as f64);
    }
    rep.record(
        "D4.api-call-64KiB",
        "D 互调",
        "互调 api-call（64 KiB reply，每次）",
        "µs",
        samples,
        "大 reply 回传",
    );
    Ok(())
}

// ==================== E · 流式 ====================

/// E1：终端输出流。spawn 产出确定字节数的进程，guest 按游标拉取输出环。
/// 关键量：跨边界次数（`calls`）× 单次开销 = 流式消费总成本。
///
/// 宿主单次 ring-fetch 返回上限 `PLUGIN_PTY_RING_FETCH_MAX_BYTES` = 16 KiB：
/// 传入更大的 `maxBytes` 会被钳制，故 64 KiB 档的调用次数与 16 KiB 档相同——
/// 这个「数控事实」本身就是一条断言。
async fn e1_pty_stream(env: &BenchEnv, iters: usize, rep: &mut Report) -> anyhow::Result<()> {
    const HOST_FETCH_MAX: u64 = 16 * 1024;
    let bytes = 1024 * 1024usize;
    for max_bytes in [4096u64, HOST_FETCH_MAX, 65536] {
        env.call_timed(
            "bench.pty-stream",
            json!({ "bytes": bytes, "maxBytes": max_bytes, "forward": false }),
        )
        .await?;
        let mut per_call = Vec::with_capacity(iters);
        let mut per_call_guest = Vec::with_capacity(iters);
        let mut total = Vec::with_capacity(iters);
        for _ in 0..iters {
            let (v, d) = env
                .call_timed(
                    "bench.pty-stream",
                    json!({ "bytes": bytes, "maxBytes": max_bytes, "forward": false }),
                )
                .await?;
            let calls = arg_usize(&v, "calls").max(1);
            per_call_guest.push(guest_nanos(&v) / 1000.0 / calls as f64);
            let calls = arg_usize(&v, "calls");
            let read = arg_usize(&v, "readBytes");
            anyhow::ensure!(read == bytes, "PTY 输出读回体量不符: {read} != {bytes}");
            anyhow::ensure!(
                !v.get("truncated").and_then(|x| x.as_bool()).unwrap_or(true),
                "环容量 > 产出时不应出现 truncated"
            );
            // 数控断言：有效批量 = min(maxBytes, 宿主钳制值)
            let effective = max_bytes.min(HOST_FETCH_MAX);
            let expected = bytes.div_ceil(effective as usize);
            let diff = (calls as i64 - expected as i64).unsigned_abs();
            anyhow::ensure!(
                diff <= 1,
                "调用次数应≈ceil({bytes}/{effective})（64 KiB 档被宿主钳制为 16 KiB）: got {calls}, want {expected}"
            );
            per_call.push(micros(d) / calls.max(1) as f64);
            total.push(micros(d));
        }
        let guest_share = median(&per_call_guest) / median(&per_call).max(1e-9) * 100.0;
        rep.record(
            &format!("E1.pty-{max_bytes}"),
            "E 流式",
            &format!("ring-fetch maxBytes={max_bytes}（每次）"),
            "µs",
            per_call,
            "调用次数即批量数控证据",
        );
        rep.record(
            &format!("E1.pty-guest-{max_bytes}"),
            "E 流式",
            &format!("ring-fetch guest 内占比（maxBytes={max_bytes}）"),
            "%",
            vec![guest_share],
            "其余为宿主侧（环读 + 边界搬运）",
        );
        let total_median = median(&total);
        rep.record(
            &format!("E1.pty-total-{max_bytes}"),
            "E 流式",
            &format!("ring-fetch 1 MiB 总耗时（maxBytes={max_bytes}）"),
            "µs",
            total,
            "纯消费侧（生产已 settle）",
        );
        if max_bytes == HOST_FETCH_MAX {
            rep.budget(
                "E1",
                "1 MiB 终端输出流拉取不得数量级变慢",
                total_median,
                3_000_000.0,
                "µs",
                Cmp::Less,
            );
        }
    }
    Ok(())
}

/// E2：输出流 + 二进制再转发（消费即分发的双跳成本）
async fn e2_pty_forward(env: &BenchEnv, iters: usize, rep: &mut Report) -> anyhow::Result<()> {
    let bytes = 1024 * 1024usize;
    env.call_peer("bench.bus-reset", json!({})).await?;
    env.call_timed(
        "bench.pty-stream",
        json!({ "bytes": bytes, "maxBytes": 16384, "forward": true, "forwardTopic": BUS_BIN_TOPIC }),
    )
    .await?;
    let mut samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let (v, d) = env
            .call_timed(
                "bench.pty-stream",
                json!({ "bytes": bytes, "maxBytes": 16384, "forward": true, "forwardTopic": BUS_BIN_TOPIC }),
            )
            .await?;
        let calls = arg_usize(&v, "calls");
        samples.push(micros(d) / calls.max(1) as f64);
    }
    let got = env.wait_peer_recv("binBytes", bytes, 10000).await?;
    rep.record(
        "E2.pty-forward",
        "E 流式",
        "ring-fetch + publish-binary 再转发（每次）",
        "µs",
        samples,
        &format!("对端收讫 {got} B"),
    );
    Ok(())
}

/// E3：HTTP 非流式大响应（guest 无法消费流式响应——流事件只到前端，
/// 故 guest 侧只测整包；流式面的真实成本在 webview 层）
async fn e3_http(env: &BenchEnv, iters: usize, rep: &mut Report) -> anyhow::Result<()> {
    for n in [1024 * 1024usize, 8 * 1024 * 1024] {
        let url = env.http_bytes_url(n);
        env.call_timed("bench.http-fetch", json!({ "url": url })).await?;
        let mut samples = Vec::with_capacity(iters);
        for _ in 0..iters {
            let (v, d) = env
                .call_timed("bench.http-fetch", json!({ "url": env.http_bytes_url(n) }))
                .await?;
            let body = arg_usize(&v, "bodyBytes");
            // 响应体经 JSON 字符串回传，实际体量含 JSON 转义开销；此处只断言「拿到全量」
            anyhow::ensure!(body >= n, "http 响应体量不足: {body} < {n}");
            samples.push(mib_per_sec(d, body as f64));
        }
        let m = median(&samples);
        rep.record(
            &format!("E3.http-{n}"),
            "E 流式",
            &format!("http 非流式 {n} B"),
            "MiB/s",
            samples,
            "宿主代发请求 + 响应体经 guest 返回",
        );
        if n == 1024 * 1024 {
            rep.budget("E3", "1 MiB 非流式响应不得数量级变慢", m, 5.0, "MiB/s", Cmp::Greater);
        }
    }
    Ok(())
}

// ==================== F · 同步 / 异步形态 ====================

/// F1：`process.run-sync`（阻塞到结束）vs `process.run`（立即返 run-id + 事件回调）。
/// 差值即「同步等待」在桥接链上的代价——业务上体现为插件实例被占住的时长。
async fn f1_process(env: &BenchEnv, iters: usize, rep: &mut Report) -> anyhow::Result<()> {
    let script = "head -c 200000 /dev/zero | tr '\\0' a";

    env.call_timed("bench.proc-sync", json!({ "script": script })).await?;
    let mut sync_samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let (v, d) = env.call_timed("bench.proc-sync", json!({ "script": script })).await?;
        anyhow::ensure!(
            arg_usize(&v, "stdoutBytes") >= 200000,
            "run-sync 未捕获全量 stdout: {}",
            arg_usize(&v, "stdoutBytes")
        );
        sync_samples.push(micros(d));
    }

    // 异步：提交往返 + 终态回调延迟（轮询 counters，终态计数单调）
    env.call_main("bench.reset", json!({})).await?;
    let out_path = env.fs_dir.join("proc-async.log").to_string_lossy().to_string();
    let mut submit_samples = Vec::with_capacity(iters);
    let mut callback_samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        // 计数单调递增：逐轮取基线，等「基线 +1」才算本轮的终态回调
        // （固定等 >=1 在第二轮起会立即通过，测到的是上一轮的延迟）
        let base = arg_usize(&env.call_main("bench.counters", json!({})).await?, "processDone");
        let (_, submit) = env
            .call_timed("bench.proc-async", json!({ "script": script, "outputPath": out_path }))
            .await?;
        submit_samples.push(micros(submit));
        let start = std::time::Instant::now();
        let deadline = start + std::time::Duration::from_secs(30);
        loop {
            let c = env.call_main("bench.counters", json!({})).await?;
            if arg_usize(&c, "processDone") > base {
                break;
            }
            anyhow::ensure!(std::time::Instant::now() < deadline, "process.run 终态回调 30s 未到");
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
        callback_samples.push(micros(start.elapsed()));
    }
    let v = env.call_main("bench.counters", json!({})).await?;
    anyhow::ensure!(
        arg_usize(&v, "processDone") >= iters,
        "process.run 终态回调数不足: {} < {iters}",
        arg_usize(&v, "processDone")
    );

    rep.record(
        "F1.proc-sync",
        "F 异步",
        "process.run-sync（阻塞，含子进程）",
        "µs",
        sync_samples,
        "同步等终态",
    );
    rep.record(
        "F1.proc-async-submit",
        "F 异步",
        "process.run 提交往返",
        "µs",
        submit_samples,
        "立即返 run-id",
    );
    rep.record(
        "F1.proc-async-callback",
        "F 异步",
        "process.run 终态回调延迟",
        "µs",
        callback_samples,
        "提交 → on_process_done",
    );
    Ok(())
}

/// F2：周期定时器回调（agent-hub 轮询 / 心跳类形态）
async fn f2_timer(env: &BenchEnv, iters: usize, rep: &mut Report) -> anyhow::Result<()> {
    let _ = iters;
    env.call_main("bench.reset", json!({})).await?;
    let (_, d) = env
        .call_timed("bench.timer-fire", json!({ "intervalMs": 1000 }))
        .await?;
    rep.record(
        "F2.timer-register",
        "F 异步",
        "timer.register 注册往返",
        "µs",
        vec![micros(d)],
        "1 s 周期",
    );

    // 等 2.6 s 覆盖至少 2 次触发（首 tick 在一个周期后）
    tokio::time::sleep(std::time::Duration::from_millis(2600)).await;
    let c = env.call_main("bench.counters", json!({})).await?;
    let fired = arg_usize(&c, "timerFired");
    anyhow::ensure!(fired >= 1, "定时器未触发（fired={fired}）");
    rep.record(
        "F2.timer-fire",
        "F 异步",
        "定时器回调（2.6 s 内次数）",
        "次",
        vec![fired as f64],
        "周期任务回灌插件命令面",
    );
    Ok(())
}

// ==================== G · 并发 ====================

/// G1：同一实例的并发命令扇出。插件实例锁（`Arc<Mutex<LoadedWasmPlugin>>`）
/// 使并发调用串行化——这里给出该串行化在**不同并发度**下的实际表现，
/// 供并发模型专项（`.scratch/2026-09-26-plugin-concurrency-model/`）取基线。
async fn g1_concurrency(env: &BenchEnv, iters: usize, rep: &mut Report) -> anyhow::Result<()> {
    for fanout in [1usize, 2, 4, 8] {
        env.call_main("bench.nop", json!({})).await?;
        let mut samples = Vec::with_capacity(iters);
        for _ in 0..iters {
            let t0 = std::time::Instant::now();
            let futures = (0..fanout).map(|_| env.call_timed("bench.nop", json!({})));
            let results = futures_util::future::join_all(futures).await;
            samples.push(micros(t0.elapsed()) / fanout as f64);
            for r in results {
                r?;
            }
        }
        rep.record(
            &format!("G1.fanout-{fanout}"),
            "G 并发",
            &format!("{fanout} 路并发 nop（摊每次）"),
            "µs",
            samples,
            "实例锁串行化的直接体现（nop 太短，串行化不可见）",
        );
    }
    Ok(())
}

/// G2：**慢调用是否堵死同实例的全部交互**（并发模型专项的量化基线）
///
/// G1 用 `nop`（~50 µs）扇出，串行化被固定开销掩盖（本轮实测并发越高单次反而越快）。
/// 这里改用**有实质时长的调用**（`bench.storage-rt` 1 MiB，debug 下 ~26 ms/次）：
///
/// - G2a **阻塞证据**：慢调用在途时并发发一条 `nop`，量它的延迟。
///   当前模型下二者共用 `Arc<Mutex<LoadedWasmPlugin>>` 且 `run_guest_call`
///   全程持锁 → `nop` 延迟应从 ~50 µs 跳到「慢调用的量级」；
///   未来改成事件循环属主 + guest task 后，这条应回落。
/// - G2b **扇出吞吐**：K 路慢调用并发的总耗时 / K×单次耗时 = 串行化倍率。
///
/// 取数顺序说明：`join!` 先轮询 `slow`，慢调用先拿锁；若顺序反了（nop 先拿锁）
/// 会低估阻塞，故同时记录**慢调用自身耗时**作对照——`nop` 延迟逼近慢调用耗时
/// 才是阻塞的证据。
async fn g2_slow_call_blocks_instance(env: &BenchEnv, iters: usize, rep: &mut Report) -> anyhow::Result<()> {
    const SLOW_BYTES: usize = 1024 * 1024;
    let slow_args = json!({ "key": "g2.slow", "bytes": SLOW_BYTES });
    let (v, _) = env.call_timed("bench.storage-rt", slow_args.clone()).await?;
    anyhow::ensure!(
        arg_usize(&v, "readBytes") == SLOW_BYTES,
        "慢调用未拿到全量数据: {}",
        arg_usize(&v, "readBytes")
    );

    let mut nop_alone = Vec::with_capacity(iters);
    let mut nop_during = Vec::with_capacity(iters);
    let mut slow_own = Vec::with_capacity(iters);
    for _ in 0..iters {
        // 基线：孤立 nop（无慢调用在途）
        let (_, d) = env.call_timed("bench.nop", json!({})).await?;
        nop_alone.push(micros(d));

        // 慢调用先拿锁（join! 按序轮询），nop 必须等它释放 → 直接读出阻塞
        let (slow_res, probe_res) = tokio::join!(
            env.call_timed("bench.storage-rt", slow_args.clone()),
            env.call_timed("bench.nop", json!({}))
        );
        let (_, slow_d) = slow_res?;
        let (_, probe_d) = probe_res?;
        slow_own.push(micros(slow_d));
        nop_during.push(micros(probe_d));
    }
    let baseline = median(&nop_alone);
    let blocked = median(&nop_during);
    let slow_median = median(&slow_own);
    rep.record("G2.nop-alone", "G 并发", "nop（无慢调用在途）", "µs", nop_alone, "基线");
    rep.record(
        "G2.nop-during-slow",
        "G 并发",
        "nop（慢调用在途）",
        "µs",
        nop_during,
        "阻塞的直接读数",
    );
    rep.record(
        "G2.slow-own",
        "G 并发",
        "慢调用自身（1 MiB storage）",
        "µs",
        slow_own,
        "阻塞上限对照",
    );
    rep.record(
        "G2.block-ratio",
        "G 并发",
        "阻塞倍数（慢调用在途 nop / 孤立 nop）",
        "×",
        vec![blocked / baseline.max(1e-9)],
        "1 = 无阻塞；接近「慢调用自身 / 孤立 nop」= 完全串行",
    );

    // G2b：K 路慢调用扇出的串行化倍率
    for fanout in [2usize, 4] {
        let t0 = std::time::Instant::now();
        let results =
            futures_util::future::join_all((0..fanout).map(|_| env.call_timed("bench.storage-rt", slow_args.clone())))
                .await;
        let total = micros(t0.elapsed());
        for r in results {
            r?.0;
        }
        rep.record(
            &format!("G2.fanout-{fanout}"),
            "G 并发",
            &format!("{fanout} 路并发慢调用（总耗时）"),
            "µs",
            vec![total],
            &format!(
                "串行化倍率 {:.2}×（对比单次 {} µs）",
                total / slow_median.max(1e-9),
                slow_median
            ),
        );
    }
    Ok(())
}

// ==================== 统计助手 ====================

fn median(samples: &[f64]) -> f64 {
    if samples.is_empty() {
        return f64::NAN;
    }
    let mut s = samples.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    s[s.len() / 2]
}
