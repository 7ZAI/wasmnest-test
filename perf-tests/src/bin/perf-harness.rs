//! 性能测试统一入口。
//!
//! 默认 smoke（秒级）；`--full` 跑完整矩阵。模块用 `--module` 选择。
//! 性能测量只允许 release 构建：debug 数值没有产品意义，`--allow-debug`
//! 仅用于开发 harness 本身。

use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use perf_tests::env::Environment;
use perf_tests::modules::{apps, bridge};
use perf_tests::report::Report;
use perf_tests::support::repo_paths;

fn main() -> ExitCode {
    // apps 模块会临时改写 HOME（应用数据根经宿主 dirs::home_dir 解析）；
    // 原始值必须在任何沙箱安装之前固定，供夹具子进程显式恢复。
    perf_tests::support::home::capture_original();
    init_tracing();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let full = args.iter().any(|a| a == "--full");
    let allow_debug = args.iter().any(|a| a == "--allow-debug");
    let module_filter = arg_value(&args, "--module").unwrap_or_else(|| "all".to_string());
    let group_filter = arg_value(&args, "--group");
    let iters = arg_value(&args, "--iters")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(if full { 5 } else { 1 });
    let json_out = arg_optional_value(&args, "--json").unwrap_or_else(default_json_path);
    let baseline = arg_value(&args, "--compare");
    let list_only = args.iter().any(|a| a == "--list");

    let modules = Modules::parse(&module_filter);
    if let Err(msg) = modules {
        eprintln!("[perf] {msg}");
        return ExitCode::FAILURE;
    }
    let modules = modules.expect("checked above");

    if list_only {
        if modules.bridge {
            println!("[bridge] 迁移自平台仓 wasm_bridge_bench");
            for (id, group, title) in bridge::list() {
                println!("  [{group}] {id:<28} {title}");
            }
        }
        if modules.apps {
            println!("[apps] 真实业务应用");
            for (id, group, title) in apps::list() {
                println!("  [{group}] {id:<28} {title}");
            }
        }
        return ExitCode::SUCCESS;
    }

    if cfg!(debug_assertions) && !allow_debug {
        eprintln!(
            "[perf] 拒绝在 debug 构建下测量：数字不代表产品。\n\
             [perf]   cargo run --release --bin perf-harness -- [--full] [--module bridge|apps|all]\n\
             [perf] 仅开发 harness 本身时可加 --allow-debug。"
        );
        return ExitCode::FAILURE;
    }

    let environment = Environment::capture();
    println!("[perf] 环境：{}", environment.describe());
    println!(
        "[perf] 模式：{}（每测点 {iters} 次，模块 {module_filter}）",
        if full { "full" } else { "smoke" }
    );

    let mut report = Report::new(if full { "full" } else { "smoke" }, iters);
    report.set_env(environment.describe());
    report.set_environment(environment.to_json());

    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("[perf] tokio runtime 构建失败: {e}");
            return ExitCode::FAILURE;
        }
    };

    if modules.bridge {
        if let Err(e) = rt.block_on(bridge::run(full, iters, group_filter.as_deref(), &mut report)) {
            eprintln!("[perf] bridge 模块失败: {e:#}");
            report.failures.push(format!("bridge: {e:#}"));
        }
    }
    if modules.apps {
        if let Err(e) = rt.block_on(apps::run(full, iters, group_filter.as_deref(), &mut report)) {
            eprintln!("[perf] apps 模块失败: {e:#}");
            report.failures.push(format!("apps: {e:#}"));
        }
    }

    println!("\n{}", report.render_table());
    report.print_budgets();

    if let Err(e) = report.write_json(&json_out) {
        eprintln!("[perf] JSON 报告写入失败: {e}");
        report.failures.push(format!("json: {e}"));
    } else {
        println!("[perf] JSON 报告: {json_out}");
    }

    if let Some(path) = baseline {
        if let Err(e) = report.compare(&path) {
            eprintln!("[perf] 基线对比失败: {e}");
        }
    }

    if report.failures.is_empty() {
        ExitCode::SUCCESS
    } else {
        eprintln!("[perf] 失败 {} 项", report.failures.len());
        for failure in &report.failures {
            eprintln!("  - {failure}");
        }
        ExitCode::FAILURE
    }
}

/// 宿主诊断日志（trap / 自动重载 / 停用）默认静默，`RUST_LOG` 打开后走 stderr：
/// 基准失败先看日志判定是测点问题还是台子问题。
fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(true)
        .try_init();
}

#[derive(Clone, Copy)]
struct Modules {
    bridge: bool,
    apps: bool,
}

impl Modules {
    fn parse(value: &str) -> Result<Self, String> {
        match value.to_ascii_lowercase().as_str() {
            "all" => Ok(Self {
                bridge: true,
                apps: true,
            }),
            "bridge" => Ok(Self {
                bridge: true,
                apps: false,
            }),
            "apps" => Ok(Self {
                bridge: false,
                apps: true,
            }),
            other => Err(format!(
                "未知模块 {other}；可用：all / bridge / apps（services / crossend 为第二批）"
            )),
        }
    }
}

fn default_json_path() -> String {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    repo_paths::perf_target_dir()
        .join("results")
        .join(format!("perf-{stamp}.json"))
        .display()
        .to_string()
}

/// 取 `--key value` 形态参数值。
fn arg_value(args: &[String], key: &str) -> Option<String> {
    let pos = args.iter().position(|a| a == key)?;
    args.get(pos + 1).cloned()
}

/// 取 `--json [path]`：无路径时返回 None（由调用方落默认路径）。
fn arg_optional_value(args: &[String], key: &str) -> Option<String> {
    let pos = args.iter().position(|a| a == key)?;
    match args.get(pos + 1) {
        Some(next) if !next.starts_with("--") => Some(next.clone()),
        _ => None,
    }
}
