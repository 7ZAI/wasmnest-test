//! 宿主规模 / 前端注册表边界（headless）。
//!
//! 与 `bridge`（单实例原语）和 `apps`（四个真实应用）不同，本模块专门回答
//! **数量上限**问题：
//!
//! - **S 宿主规模**：同一 wasm 组件摆成 N 个属主，逐个真激活。记录每个数量档的
//!   激活耗时、常驻 RSS、跨实例命令延迟，以及停用后的回收情况。宿主编排层没有
//!   「最多 N 个应用」的硬编码常量——真实上限由内存 / 调度决定：
//!   - S1 是**固定档探针**（full 跑 4/8/16/32），画规模曲线；16 个是交付门禁的
//!     支撑下限，32 个只观察；
//!   - S4 是**加压探针**：从 4 起翻倍加压，直到某一档不可用（装配 / 激活失败、
//!     RSS 超上限、单命令延迟超响应上限），再二分收敛，直接回答「本次环境最多
//!     同时跑多少个」并给出边界归因。上限可经 `--max-apps` 调高。
//!   S1/S4 的被测物都是**合成夹具**（同一 bench 组件多属主）：测的是宿主 wasm
//!   实例容量，不是真实业务应用的内存画像——真实应用的常驻增量与按内存估算
//!   的容量见 apps 模块 R1。
//! - **F 前端投影**：`plugin_list_loaded` 返回的注册表清单（`list_plugins`）在
//!   N = 4/16/64/256 个插件时的取数耗时与 JSON 体积。这是宿主 → 前端边界的
//!   **数据面**；真实 WebView 的渲染 / 派发由平台 e2e 覆盖，不在无头进程里假装。
//!
//! 两个子环境都走生产同形装配与临时目录；N 个 wasm 实例使用 bench 夹具，
//! N 个清单使用 frontend 形态目录（不实例化 wasm），把「运行时实例」与「清单体积」
//! 两个变量隔开。

use std::time::Instant;

use futures_util::future::join_all;
use serde_json::json;
use wasmnest_wasm_core::manifest::PluginState;

use crate::report::{micros, Cmp, Report};
use crate::support::rss;
use crate::support::scale_env::{self, ScaleEnv};

/// 交付门禁支持的同时常驻应用数（当前随包 4 个应用的 4 倍余量）
const SUPPORTED_SCALE: usize = 16;
/// S1 固定档探针上限（观察值，不设门禁）
const SCALE_PROBE_LIMIT: usize = 32;
/// 注册表投影 full 模式的上限
const MANIFEST_PROBE_LIMIT: usize = 256;
/// S4 加压探针的默认上限（`--max-apps` 覆盖）
pub const DEFAULT_MAX_APPS: usize = 256;
/// S4 默认的整进程 RSS 上限（MiB；`--rss-ceiling-mib` 覆盖）
pub const DEFAULT_RSS_CEILING_MIB: f64 = 4096.0;
/// Rust 层的「卡顿」代理：N 个常驻下单命令中位数上限（µs）。
/// 前端帧率不在无头进程里测，这里只保证命令面没有被调度拖垮。
const RESPONSIVE_PING_US: f64 = 10_000.0;

/// 规模探针的可调边界（CLI 注入；同时写进报告，避免「默认值当成结论」）。
#[derive(Debug, Clone, Copy)]
pub struct ScaleLimits {
    /// 加压上限（个）
    pub max_apps: usize,
    /// 整进程 RSS 上限（MiB）
    pub rss_ceiling_mib: f64,
}

impl Default for ScaleLimits {
    fn default() -> Self {
        Self {
            max_apps: DEFAULT_MAX_APPS,
            rss_ceiling_mib: DEFAULT_RSS_CEILING_MIB,
        }
    }
}

struct Scenario {
    id: &'static str,
    group: &'static str,
    title: &'static str,
    smoke: bool,
}

fn all() -> Vec<Scenario> {
    vec![
        Scenario { id: "S1", group: "S", title: "同时常驻规模（4/8/16/32）", smoke: true },
        Scenario { id: "S2", group: "S", title: "N 实例并发命令扇出", smoke: false },
        Scenario { id: "S3", group: "S", title: "N 实例停用与 RSS 回收", smoke: false },
        Scenario { id: "S4", group: "S", title: "最大同时常驻（加压到边界）", smoke: false },
        Scenario { id: "F1", group: "F", title: "宿主→前端注册表投影（4/16/64/256）", smoke: true },
    ]
}

/// `--list`：稳定 id / 分组 / 标题
pub fn list() -> Vec<(&'static str, &'static str, &'static str)> {
    all()
        .into_iter()
        .map(|s| (s.id, s.group, s.title))
        .collect()
}

pub async fn run(
    full: bool,
    iters: usize,
    group_filter: Option<&str>,
    limits: &ScaleLimits,
    rep: &mut Report,
) -> anyhow::Result<()> {
    let selected = |id: &str, group: &str| match group_filter {
        None => true,
        Some(g) => group.eq_ignore_ascii_case(g) || id.contains(g),
    };
    // 冒烟只跑每个子模块的一条主链；S2/S3/S4 只在 --full 下出现。
    // 例外：`--group S4` 这种**精确点名**允许在冒烟模式单跑——只想量上限时
    // 不必把整份矩阵跑一遍（门禁仍只在 --full 下生效）。
    let named = |id: &str| group_filter.is_some_and(|g| id.eq_ignore_ascii_case(g));
    let enabled = |id: &str| {
        full || named(id) || all().iter().any(|s| s.id == id && s.smoke)
    };
    let do_s1 = enabled("S1") && selected("S1", "S");
    let do_s2 = enabled("S2") && selected("S2", "S");
    let do_s3 = enabled("S3") && selected("S3", "S");
    let do_s4 = enabled("S4") && selected("S4", "S");
    let do_f1 = enabled("F1") && selected("F1", "F");

    if do_s1 || do_s2 || do_s3 {
        run_scale(full, iters, do_s1, do_s2, do_s3, rep).await?;
    }
    if do_s4 {
        run_capacity(full, limits, rep).await?;
    }
    if do_f1 {
        run_projection(full, rep).await?;
    }
    Ok(())
}

// ==================== S · 宿主规模 ====================

async fn run_scale(
    full: bool,
    iters: usize,
    do_s1: bool,
    do_s2: bool,
    do_s3: bool,
    rep: &mut Report,
) -> anyhow::Result<()> {
    let max = if full { SCALE_PROBE_LIMIT } else { 4 };
    // 装配前基线：S1.rss 的增量把「建宿主 + N 次实例化 + N 次激活」一起算进去
    let rss_before = rss::current_rss_bytes().unwrap_or(0);
    let env = scale_env::build_wasm_instances(max).await?;

    let mut active: Vec<String> = Vec::with_capacity(max);
    let mut total_ms = 0.0;
    let mut last_success = 0usize;

    for (index, id) in env.ids.iter().enumerate() {
        let n = index + 1;
        let started = Instant::now();
        match env.host.activate_plugin(id, false).await {
            Ok(()) => {
                total_ms += micros(started.elapsed()) / 1000.0;
                active.push(id.clone());
                last_success = n;
            }
            Err(error) => {
                // 容量探针的边界不等于回归：超过门禁支撑数的失败只记事实，不把
                // 整份报告打红；门禁本身在 S1.capacity / S1.active-16 上。
                eprintln!("[boundary] S1 第 {n} 个实例激活失败: {error:#}");
                if !full || n <= SUPPORTED_SCALE {
                    rep.failures.push(format!("S1: 第 {n} 个实例激活失败: {error:#}"));
                }
                break;
            }
        }

        if do_s1 && is_checkpoint(n, full) {
            record_scale_checkpoint(&env, &active, n, total_ms, rss_before, full, rep).await?;
        }
    }

    if do_s1 {
        rep.record(
            "S1.capacity",
            "S 宿主规模",
            "本次最大同时常驻数",
            "个",
            vec![last_success as f64],
            if last_success == max {
                "达到本次探针上限（宿主无硬编码上限）"
            } else {
                "后续激活失败，容量边界已触达"
            },
        );
        if full {
            // 只有 full 跑得到 16；冒烟 4 个只做链路自检，不假装验证容量下限
            rep.budget(
                "S1.capacity",
                "至少支持 16 个应用同时常驻",
                last_success as f64,
                (SUPPORTED_SCALE as f64) - 0.5,
                "个",
                Cmp::Greater,
            );
        }
    }

    if do_s2 && !active.is_empty() {
        run_fanout(&env, &active, iters.max(1), rep).await?;
    }

    let rss_at_peak = rss::current_rss_bytes().unwrap_or(rss_before);
    if do_s3 && !active.is_empty() {
        let started = Instant::now();
        let failures = env.deactivate_all(&active).await;
        let teardown_ms = micros(started.elapsed()) / 1000.0;
        for failure in failures {
            rep.failures.push(format!("S3: {failure}"));
        }

        let after = rss::current_rss_bytes().unwrap_or(rss_at_peak);
        rep.record(
            "S3.teardown",
            "S 宿主规模",
            "N 实例顺序停用",
            "ms",
            vec![teardown_ms],
            &format!("{} 个实例全部 deactivate", active.len()),
        );
        rep.record(
            "S3.rss-after",
            "S 宿主规模",
            "停用后进程 RSS",
            "MiB",
            vec![rss::mib(after)],
            "与峰值对比观察回收；OS 未必立刻归还",
        );
        rep.record(
            "S3.reclaim",
            "S 宿主规模",
            "停用回收的 RSS",
            "MiB",
            vec![rss::mib(rss_at_peak.saturating_sub(after))],
            "峰值 RSS − 停用后 RSS",
        );

        let infos = env.host.list_plugins().await;
        let remaining = infos
            .iter()
            .filter(|p| matches!(p.state, PluginState::Activated))
            .count();
        rep.record(
            "S3.remaining",
            "S 宿主规模",
            "停用后仍 activated 的实例",
            "个",
            vec![remaining as f64],
            "回收完整性",
        );
        rep.budget(
            "S3.remaining",
            "停用后不得仍有 activated 实例",
            remaining as f64,
            1.0,
            "个",
            Cmp::Less,
        );
        rep.budget(
            "S3.teardown",
            "N 实例顺序停用不得数量级变慢",
            teardown_ms,
            60_000.0,
            "ms",
            Cmp::Less,
        );
    }

    // S3 未选中时也要收尾；已停用的实例再停用只会报“未激活”，忽略即可。
    let _ = env.deactivate_all(&active).await;
    env.cleanup();
    Ok(())
}

fn is_checkpoint(n: usize, full: bool) -> bool {
    if full {
        matches!(n, 4 | 8 | 16 | 32)
    } else {
        n == 4
    }
}

async fn record_scale_checkpoint(
    env: &ScaleEnv,
    active: &[String],
    n: usize,
    total_ms: f64,
    rss_before: u64,
    full: bool,
    rep: &mut Report,
) -> anyhow::Result<()> {
    // 每个已激活实例都下一次命令：证明「常驻」不是只建了实例，命令面确实可用。
    let mut pings = Vec::with_capacity(active.len());
    for id in active {
        let started = Instant::now();
        let value = env
            .host
            .invoke_rust_command(id, "bench.ping", json!({}))
            .await
            .map_err(|e| anyhow::anyhow!("S1 在 {n} 个常驻实例下 ping {id} 失败: {e}"))?;
        let duration = started.elapsed();
        anyhow::ensure!(
            value.get("pong").and_then(|v| v.as_bool()) == Some(true),
            "S1 在 {n} 个常驻实例下 ping {id} 回执异常: {value}"
        );
        pings.push(micros(duration));
    }

    let current = rss::current_rss_bytes().unwrap_or(rss_before);
    let ping_median = median(&pings);
    rep.record(
        &format!("S1.active-{n}"),
        "S 宿主规模",
        &format!("同时常驻 {n} 个"),
        "个",
        vec![active.len() as f64],
        "本档实际 activated 数",
    );
    rep.record(
        &format!("S1.activate-total-{n}"),
        "S 宿主规模",
        &format!("{n} 个顺序激活总耗时"),
        "ms",
        vec![total_ms],
        "guest activate（组件实例化已在装配期完成）",
    );
    rep.record(
        &format!("S1.activate-avg-{n}"),
        "S 宿主规模",
        &format!("{n} 个平均单应用激活"),
        "ms",
        vec![total_ms / n as f64],
        "总耗时 / N；不含装配期实例化",
    );
    rep.record(
        &format!("S1.rss-{n}"),
        "S 宿主规模",
        &format!("{n} 个常驻 RSS"),
        "MiB",
        vec![rss::mib(current)],
        &format!("相对装配前增量 {:.1} MiB", rss::mib(current.saturating_sub(rss_before))),
    );
    rep.record(
        &format!("S1.ping-{n}"),
        "S 宿主规模",
        &format!("{n} 个常驻下的单命令"),
        "µs",
        pings,
        "逐实例 bench.ping（跨实例调度）",
    );

    let gate = if full { SUPPORTED_SCALE } else { 4 };
    if n == gate {
        rep.budget(
            &format!("S1.active-{n}"),
            "常驻实例数达到门禁支撑数",
            active.len() as f64,
            (n as f64) - 0.5,
            "个",
            Cmp::Greater,
        );
        rep.budget(
            &format!("S1.activate-total-{n}"),
            "N 个顺序激活不得数量级变慢",
            total_ms,
            60_000.0,
            "ms",
            Cmp::Less,
        );
        rep.budget(
            &format!("S1.rss-{n}"),
            "N 个常驻 RSS 不得超过数量级门限",
            rss::mib(current),
            4096.0,
            "MiB",
            Cmp::Less,
        );
        rep.budget(
            &format!("S1.ping-{n}"),
            "N 个常驻下单命令不得数量级变慢",
            ping_median,
            10_000.0,
            "µs",
            Cmp::Less,
        );
    }
    Ok(())
}

async fn run_fanout(
    env: &ScaleEnv,
    active: &[String],
    rounds: usize,
    rep: &mut Report,
) -> anyhow::Result<()> {
    let n = active.len();
    let mut total_samples = Vec::with_capacity(rounds);
    let mut call_samples = Vec::with_capacity(n * rounds);
    let mut failures = 0usize;

    for _ in 0..rounds {
        let started = Instant::now();
        let calls = active.iter().map(|id| async move {
            let call_started = Instant::now();
            let result = env
                .host
                .invoke_rust_command(id, "bench.ping", json!({}))
                .await;
            (id, result, call_started.elapsed())
        });
        let results = join_all(calls).await;
        total_samples.push(micros(started.elapsed()) / 1000.0);

        for (id, result, duration) in results {
            match result {
                Ok(value) if value.get("pong").and_then(|v| v.as_bool()) == Some(true) => {
                    call_samples.push(micros(duration));
                }
                Ok(value) => {
                    failures += 1;
                    rep.failures
                        .push(format!("S2: {id} 回执异常: {value}"));
                }
                Err(error) => {
                    failures += 1;
                    rep.failures.push(format!("S2: {id} 调用失败: {error:#}"));
                }
            }
        }
    }

    let total_median = median(&total_samples);
    let call_median = median(&call_samples);

    rep.record(
        "S2.fanout-total",
        "S 宿主规模",
        &format!("{n} 实例并发扇出（一轮）"),
        "ms",
        total_samples,
        "N 条命令同时下发，等全部返回",
    );
    rep.record(
        "S2.fanout-call",
        "S 宿主规模",
        &format!("{n} 实例并发下的单命令"),
        "µs",
        call_samples,
        "跨实例调度 + 单实例锁竞争",
    );
    rep.record(
        "S2.failures",
        "S 宿主规模",
        "并发扇出失败数",
        "个",
        vec![failures as f64],
        &format!("{rounds} 轮 × {n} 条"),
    );
    rep.budget(
        "S2.fanout-total",
        "N 实例并发扇出不得数量级变慢",
        total_median,
        1_000.0,
        "ms",
        Cmp::Less,
    );
    rep.budget(
        "S2.fanout-call",
        "并发下单命令不得数量级变慢",
        call_median,
        50_000.0,
        "µs",
        Cmp::Less,
    );
    rep.budget(
        "S2.failures",
        "并发扇出不得失败",
        failures as f64,
        1.0,
        "个",
        Cmp::Less,
    );
    Ok(())
}

// ==================== S4 · 加压到边界 ====================

/// 子进程回传探针结果时 stdout 的标记行前缀（父进程按行扫描，日志前缀无影响）
pub const CAPACITY_PROBE_MARKER: &str = "WASMNEST_PERF_CAPACITY ";

/// 一档加压探针的结果
#[derive(Debug, Clone)]
struct CapacityStep {
    /// 不可用时的精确档位（激活失败记「第几个实例」，其余记尝试档位）
    apps: usize,
    /// 本档是否可用（装配成功 + 全部激活 + 回执正常 + RSS / 延迟在限内）
    usable: bool,
    /// 边界归因（`ok` / `assemble-failed` / `activation-failed` / `ping-failed`
    /// / `rss-ceiling` / `latency` / `probe-crashed`）
    reason: &'static str,
    /// 人读证据（失败原因或档位说明）
    detail: String,
    rss_mib: Option<f64>,
    rss_before_mib: Option<f64>,
}

/// 子进程单档探针的原始回执
#[derive(Debug, Clone, serde::Deserialize)]
struct ChildProbe {
    apps: usize,
    rss_before_mib: Option<f64>,
    rss_mib: Option<f64>,
    activated: usize,
    activate_ms: f64,
    #[serde(default)]
    ping_us: Vec<f64>,
    assemble_failure: Option<String>,
    activation_failure: Option<String>,
    /// 激活失败时是「第几个实例」（容量结论要精确序号）
    failed_at: Option<usize>,
    ping_failure: Option<String>,
}

/// 加压探针：从 4 起翻倍，直到某档不可用，再在区间内二分收敛。
///
/// 每档在**独立子进程**里装配并测量，理由见 [`capacity_probe_child`]：进程内
/// 连续装配多档时，宿主与 message bus 互相持有（`Arc` 环），停用后的实例不会
/// 释放，RSS 逐档叠加，容量判定会系统性偏小。子进程让每档都是「N 个应用同时
/// 常驻」的干净整进程口径，也顺带隔离高档位的 trap / OOM。
async fn run_capacity(full: bool, limits: &ScaleLimits, rep: &mut Report) -> anyhow::Result<()> {
    let ceiling = limits.max_apps.max(4);

    let mut last_good = 0usize;
    let mut last_good_rss: Option<(f64, f64)> = None;
    let mut first_bad: Option<CapacityStep> = None;

    let mut n = 4usize;
    while n <= ceiling {
        let step = probe_capacity_level(n, limits, rep).await?;
        if step.usable {
            last_good = n;
            last_good_rss = rss_pair(&step);
            n = n.saturating_mul(2);
        } else {
            first_bad = Some(step);
            break;
        }
    }

    // 翻倍只给到 [可用, 不可用] 区间；二分把它收敛到相邻整数。前提是「档位越大
    // 越差」（RSS 与激活成本单调；命令延迟有抖动）——因此每档实测都进报告，
    // 归因取**首个不可用档**的实测原因，而不是二分的推断。
    if let Some(mut bad) = first_bad.clone() {
        // `bad.apps > last_good + 1` 而不是相减：激活失败记的是精确序号，
        // 单调性被抖动打破时也不能让 usize 下溢。
        while bad.apps > last_good + 1 {
            let mid = last_good + (bad.apps - last_good) / 2;
            let step = probe_capacity_level(mid, limits, rep).await?;
            if step.usable {
                last_good = mid;
                last_good_rss = rss_pair(&step);
            } else {
                bad = step;
            }
        }
        first_bad = Some(bad);
    }

    let bound = first_bad
        .as_ref()
        .map(|s| s.reason)
        .unwrap_or("probe-limit");
    let bound_label = match bound {
        "probe-limit" => "达到本次加压上限（未触底，可 --max-apps 调高）",
        "assemble-failed" => "装配期实例化失败（扫描即实例化）",
        "activation-failed" => "激活失败",
        "ping-failed" => "命令回执失败",
        "rss-ceiling" => "整进程 RSS 超过上限",
        "latency" => "单命令中位数超过响应上限",
        "probe-crashed" => "探针子进程异常退出（trap / OOM / 崩溃）",
        _ => "未归因",
    };
    let mut note = format!(
        "边界归因：{bound_label}；加压上限 {ceiling} 个，RSS 上限 {:.0} MiB",
        limits.rss_ceiling_mib
    );
    if let Some(bad) = &first_bad {
        note.push_str(&format!("；首个不可用档 {} 个（{}）", bad.apps, bad.detail));
    }
    rep.record(
        "S4.max-apps",
        "S 宿主规模",
        "最大同时常驻（可用）",
        "个",
        vec![last_good as f64],
        &note,
    );
    if let Some(bad) = &first_bad {
        rep.record(
            "S4.first-bad",
            "S 宿主规模",
            "首个不可用档",
            "个",
            vec![bad.apps as f64],
            &bad.detail,
        );
    }
    // 单应用边际内存：给「本机还能塞多少」一个实测锚点（合成夹具口径）。
    if last_good > 0 {
        if let Some((rss_mib, rss_before_mib)) = last_good_rss {
            rep.record(
                "S4.rss-per-app",
                "S 宿主规模",
                "单应用边际常驻内存",
                "MiB",
                vec![(rss_mib - rss_before_mib) / last_good as f64],
                "（最大可用档 RSS − 同进程装配前 RSS）/ N；合成夹具，真实应用见 R1",
            );
        }
    }

    if full {
        // 与 S1 同一支撑下限：门禁只保「下限能跑」，上限本身是探针结果不是回归线。
        rep.budget(
            "S4.max-apps",
            "最大同时常驻不低于支撑下限 16",
            last_good as f64,
            (SUPPORTED_SCALE as f64) - 0.5,
            "个",
            Cmp::Greater,
        );
    }
    Ok(())
}

fn rss_pair(step: &CapacityStep) -> Option<(f64, f64)> {
    Some((step.rss_mib?, step.rss_before_mib?))
}

/// 拉一个子进程测一档，并把结果落成测点。
async fn probe_capacity_level(
    n: usize,
    limits: &ScaleLimits,
    rep: &mut Report,
) -> anyhow::Result<CapacityStep> {
    const GROUP: &str = "S 宿主规模";
    let probe = match spawn_capacity_child(n) {
        Ok(probe) => probe,
        Err(error) => {
            let detail = format!("探针子进程异常：{error:#}");
            eprintln!("[boundary] S4 {n} 档{detail}");
            rep.record(
                &format!("S4.step-{n}.crashed"),
                GROUP,
                &format!("{n} 档探针子进程"),
                "个",
                vec![n as f64],
                &detail,
            );
            return Ok(CapacityStep {
                apps: n,
                usable: false,
                reason: "probe-crashed",
                detail,
                rss_mib: None,
                rss_before_mib: None,
            });
        }
    };

    let (usable, reason, detail) = verdict_from_probe(&probe, limits);

    rep.record(
        &format!("S4.step-{n}.activated"),
        GROUP,
        &format!("{n} 档实际 activated"),
        "个",
        vec![probe.activated as f64],
        "独立子进程装配（不从上一档继承）",
    );
    rep.record(
        &format!("S4.step-{n}.activate-ms"),
        GROUP,
        &format!("{n} 档顺序激活总耗时"),
        "ms",
        vec![probe.activate_ms],
        "装配期实例化不计入",
    );
    if let Some(rss_mib) = probe.rss_mib {
        rep.record(
            &format!("S4.step-{n}.rss"),
            GROUP,
            &format!("{n} 档常驻 RSS"),
            "MiB",
            vec![rss_mib],
            &format!("子进程整进程 RSS；上限 {:.0} MiB", limits.rss_ceiling_mib),
        );
    }
    if !probe.ping_us.is_empty() {
        let note = if usable {
            "全部回执正常，延迟在响应上限内".to_string()
        } else {
            detail.clone()
        };
        rep.record(
            &format!("S4.step-{n}.ping"),
            GROUP,
            &format!("{n} 档单命令"),
            "µs",
            probe.ping_us.clone(),
            &note,
        );
    }

    Ok(CapacityStep {
        apps: probe.failed_at.unwrap_or(probe.apps),
        usable,
        reason,
        detail,
        rss_mib: probe.rss_mib,
        rss_before_mib: probe.rss_before_mib,
    })
}

/// 判定顺序：能不能跑（装配 / 激活 / 回执）→ 内存 → 响应。先报「跑不起来」，
/// 再报「跑得慢」，避免把激活失败误读成性能退化。
fn verdict_from_probe(probe: &ChildProbe, limits: &ScaleLimits) -> (bool, &'static str, String) {
    if let Some(detail) = &probe.assemble_failure {
        return (false, "assemble-failed", detail.clone());
    }
    if let Some(detail) = &probe.activation_failure {
        return (false, "activation-failed", detail.clone());
    }
    if let Some(detail) = &probe.ping_failure {
        return (false, "ping-failed", detail.clone());
    }
    if let Some(rss_mib) = probe.rss_mib {
        if rss_mib > limits.rss_ceiling_mib {
            return (
                false,
                "rss-ceiling",
                format!(
                    "整进程 RSS {rss_mib:.1} MiB > 上限 {:.1} MiB",
                    limits.rss_ceiling_mib
                ),
            );
        }
    }
    let ping_median = median(&probe.ping_us);
    if ping_median.is_finite() && ping_median > RESPONSIVE_PING_US {
        return (
            false,
            "latency",
            format!("单命令中位数 {ping_median:.1} µs > 响应上限 {RESPONSIVE_PING_US:.0} µs"),
        );
    }
    (true, "ok", String::new())
}

/// 父进程侧：起一个 `--capacity-probe <n>` 子进程并解析标记行。
fn spawn_capacity_child(n: usize) -> anyhow::Result<ChildProbe> {
    let exe = std::env::current_exe()
        .map_err(|e| anyhow::anyhow!("取当前可执行文件失败: {e}"))?;
    let mut command = std::process::Command::new(exe);
    command.arg("--capacity-probe").arg(n.to_string());
    // 子进程与父进程同一构建；debug 构建由父进程的 --allow-debug 放行过
    if cfg!(debug_assertions) {
        command.arg("--allow-debug");
    }
    let output = command
        .output()
        .map_err(|e| anyhow::anyhow!("启动探针子进程失败: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout
        .lines()
        .find_map(|line| line.strip_prefix(CAPACITY_PROBE_MARKER))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "未回传结果（exit={:?}）：{}",
                output.status.code(),
                tail(&String::from_utf8_lossy(&output.stderr), 800)
            )
        })?;
    let probe: ChildProbe = serde_json::from_str(line)
        .map_err(|e| anyhow::anyhow!("探针回执解析失败: {e}；原始行 {line}"))?;
    anyhow::ensure!(probe.apps == n, "探针回执档位不符: {} != {n}", probe.apps);
    Ok(probe)
}

fn tail(text: &str, chars: usize) -> String {
    let count = text.chars().count();
    text.chars().skip(count.saturating_sub(chars)).collect()
}

/// 子进程口径的单档探针（`--capacity-probe <N>`）：装配 → 全激活 → 全量 ping，
/// 把结果写成标记行到 stdout 后退出。
///
/// 为什么每档一个进程：`init_message_bus` 把 `Arc<PluginHost>` 的克隆装进
/// message bus 的 dispatcher 槽，host ↔ bus 成环，进程内停用后的宿主不会被
/// 释放（实测同一档位第二轮 RSS 比第一轮高几十 MiB）。连续装配多档会让上一档
/// 的实例驻留，容量判定系统性偏小；子进程把每档还原成「N 个应用同时常驻」的
/// 干净整进程口径。
pub async fn capacity_probe_child(n: usize) -> anyhow::Result<()> {
    let rss_before_mib = rss::current_rss_bytes().map(rss::mib);

    let payload = match scale_env::build_wasm_instances(n).await {
        Err(error) => serde_json::json!({
            "apps": n,
            "rss_before_mib": rss_before_mib,
            "rss_mib": serde_json::Value::Null,
            "activated": 0,
            "activate_ms": 0.0,
            "ping_us": Vec::<f64>::new(),
            "assemble_failure": format!("{error:#}"),
            "activation_failure": serde_json::Value::Null,
            "failed_at": serde_json::Value::Null,
            "ping_failure": serde_json::Value::Null,
        }),
        Ok(env) => {
            let mut active: Vec<String> = Vec::with_capacity(n);
            let mut activate_ms = 0.0;
            let mut activation_failure: Option<String> = None;
            let mut failed_at: Option<usize> = None;
            for id in &env.ids {
                let started = Instant::now();
                match env.host.activate_plugin(id, false).await {
                    Ok(()) => {
                        activate_ms += micros(started.elapsed()) / 1000.0;
                        active.push(id.clone());
                    }
                    Err(error) => {
                        failed_at = Some(active.len() + 1);
                        activation_failure = Some(format!(
                            "第 {} 个实例激活失败：{error:#}",
                            active.len() + 1
                        ));
                        break;
                    }
                }
            }

            let rss_mib = rss::current_rss_bytes().map(rss::mib);
            let mut ping_us = Vec::with_capacity(active.len());
            let mut ping_failure: Option<String> = None;
            if activation_failure.is_none() {
                for id in &active {
                    let started = Instant::now();
                    match env.host.invoke_rust_command(id, "bench.ping", json!({})).await {
                        Ok(value)
                            if value.get("pong").and_then(|v| v.as_bool()) == Some(true) =>
                        {
                            ping_us.push(micros(started.elapsed()));
                        }
                        Ok(value) => {
                            ping_failure = Some(format!("{id} 回执异常：{value}"));
                            break;
                        }
                        Err(error) => {
                            ping_failure = Some(format!("{id} 调用失败：{error:#}"));
                            break;
                        }
                    }
                }
            }

            let payload = serde_json::json!({
                "apps": n,
                "rss_before_mib": rss_before_mib,
                "rss_mib": rss_mib,
                "activated": active.len(),
                "activate_ms": activate_ms,
                "ping_us": ping_us,
                "assemble_failure": serde_json::Value::Null,
                "activation_failure": activation_failure,
                "failed_at": failed_at,
                "ping_failure": ping_failure,
            });
            let _ = env.deactivate_all(&active).await;
            env.cleanup();
            payload
        }
    };

    println!("{CAPACITY_PROBE_MARKER}{payload}");
    Ok(())
}

// ==================== F · 宿主 → 前端注册表投影 ====================

async fn run_projection(full: bool, rep: &mut Report) -> anyhow::Result<()> {
    let steps: &[usize] = if full {
        &[4, 16, 64, MANIFEST_PROBE_LIMIT]
    } else {
        &[4]
    };
    let max = *steps.last().expect("steps 非空");

    // 每个数量档独立装配：清单投影测的是「N 条记录」的取数与体积，不能让
    // 一次装配把 256 条都塞进注册表后只截取前 N 条（那测的是同一份大表）。
    for &n in steps {
        let env = scale_env::build_manifests(n).await?;
        let started = Instant::now();
        let plugins = env.host.list_plugins().await;
        let list_us = micros(started.elapsed());
        if plugins.len() != n {
            env.cleanup();
            anyhow::bail!("F1 清单投影数量不符: {} != {n}（有插件未装载）", plugins.len());
        }

        let payload = serde_json::to_vec(&plugins)?;
        let payload_kib = payload.len() as f64 / 1024.0;

        rep.record(
            &format!("F1.list-{n}"),
            "F 前端投影",
            &format!("list_plugins {n} 个"),
            "µs",
            vec![list_us],
            "plugin_list_loaded 的同源取数",
        );
        rep.record(
            &format!("F1.payload-{n}"),
            "F 前端投影",
            &format!("注册表 JSON {n} 个"),
            "KiB",
            vec![payload_kib],
            "Tauri IPC 前的序列化体积",
        );
        rep.record(
            &format!("F1.per-entry-{n}"),
            "F 前端投影",
            &format!("每插件 {n} 个"),
            "B/个",
            vec![payload.len() as f64 / n as f64],
            "平均单条清单体积",
        );

        if n == max {
            rep.budget(
                &format!("F1.list-{n}"),
                "注册表投影不得数量级变慢",
                list_us,
                100_000.0,
                "µs",
                Cmp::Less,
            );
            rep.budget(
                &format!("F1.payload-{n}"),
                "注册表 JSON 体积不得数量级膨胀",
                payload_kib,
                4096.0,
                "KiB",
                Cmp::Less,
            );
        }

        env.cleanup();
    }

    Ok(())
}

// ==================== 小工具 ====================

fn median(samples: &[f64]) -> f64 {
    if samples.is_empty() {
        return f64::NAN;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    sorted[sorted.len() / 2]
}
