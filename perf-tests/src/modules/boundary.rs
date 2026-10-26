//! 宿主规模 / 前端注册表边界（headless）。
//!
//! 与 `bridge`（单实例原语）和 `apps`（四个真实应用）不同，本模块专门回答
//! **数量上限**问题：
//!
//! - **S 宿主规模**：同一 wasm 组件摆成 N 个属主，逐个真激活。记录每个数量档的
//!   激活耗时、常驻 RSS、跨实例命令延迟，以及停用后的回收情况。宿主编排层没有
//!   「最多 N 个应用」的硬编码常量——真实上限由内存 / 调度决定，故这里用
//!   **容量探针**（full 跑 4/8/16/32）测出本次环境的最大同时常驻数；16 个是
//!   交付门禁的支撑下限，32 个是只观察、不设门禁的探针。
//! - **F 前端投影**：`plugin_list_loaded` 返回的注册表清单（`list_plugins`）在
//!   N = 4/16/64/256 个插件时的取数耗时与 JSON 体积。这是宿主 → 前端边界的
//!   **数据面**；真实 WebView 的渲染 / 派发由平台 e2e 覆盖，不在无头进程里假装。
//!
//! 两个子环境都走生产同形装配与临时目录；N 个 wasm 实例使用 bench 夹具，
//! N 个清单使用 TS-only 目录（不实例化 wasm），把「运行时实例」与「清单体积」
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
/// full 模式的容量探针上限（观察值，不设门禁）
const SCALE_PROBE_LIMIT: usize = 32;
/// 注册表投影 full 模式的上限
const MANIFEST_PROBE_LIMIT: usize = 256;

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
    rep: &mut Report,
) -> anyhow::Result<()> {
    let selected = |id: &str, group: &str| match group_filter {
        None => true,
        Some(g) => group.eq_ignore_ascii_case(g) || id.contains(g),
    };
    // 冒烟只跑每个子模块的一条主链；S2/S3 只在 --full 下出现。
    let enabled = |id: &str| full || all().iter().any(|s| s.id == id && s.smoke);
    let do_s1 = enabled("S1") && selected("S1", "S");
    let do_s2 = enabled("S2") && selected("S2", "S");
    let do_s3 = enabled("S3") && selected("S3", "S");
    let do_f1 = enabled("F1") && selected("F1", "F");

    if do_s1 || do_s2 || do_s3 {
        run_scale(full, iters, do_s1, do_s2, do_s3, rep).await?;
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
