//! mDNS 浏览单例源码锁（三仓拆分 L2/E2b · 票 08 自能力仓迁入平台仓；2026-10-18
//! 随跨端测试面再迁出为独立仓 `wasmnest-test`；不变量真源 =
//! ADR 0054 / `.scratch/2026-10-08-capability-crates-unbind-desktop`）
//!
//! ## 锁的是什么
//!
//! mdns-sd 的共享守护对**每个服务类型只有一个 listener 槽**：重复 `daemon.browse`
//! 是「后到覆盖先到」，且旧 listener 的发送端被指数重传链永久保活——旧 receiver
//! 从此收不到事件、不报错、也不触发 `browse loop exited`。共享守护（一个进程一个
//! `ServiceDaemon`，ADR 0042）把 peer-net 的周期重查与各插件 browse 放进同一守护后，
//! **任何一处再直调 `daemon.browse` 都会静默杀死别人的事件流**——这正是双端
//! file-transfer 互相发现失败的根因。
//!
//! 修复后全进程对同一服务类型只允许**类型中枢**
//!（`packages/wasmnest-discovery-engine/src/type_hub.rs`）调用 `daemon.browse` /
//! `daemon.stop_browse`；守护实例本身仍只允许该 crate 的 `engine.rs` 创建一次。
//! 本锁把这两条不变量变成可执行判据：谁把它们接回别处，测试直接红。
//!
//! ## 为什么锁在跨端测试仓
//!
//! 扫描面横跨**能力仓（`packages/**/src`）+ 双端宿主（`wasmnest-*/src-tauri/src`）**
//! ——本仓经平台仓根同时看到两侧（能力仓经平台仓 `packages/` submodule 挂载，V1 布局）。
//! 票 08 把锁自能力仓迁出；2026-10-18 跨端测试面独立成仓后，路径基准改为
//! [`repo_paths::platform_root`]（标签仍为 `packages/<crate>/…` 与
//! `wasmnest-{desktop,mobile}/…`，与拆仓前逐字一致）。
//!
//! ## 判据
//!
//! 1. `.browse(` / `.stop_browse(` **仅**允许出现在
//!    `packages/wasmnest-discovery-engine/src/type_hub.rs`（中枢的唯一 browse 出口）；
//! 2. `ServiceDaemon::new` **仅**允许出现在
//!    `packages/wasmnest-discovery-engine/src/engine.rs`（共享守护唯一创建点）；
//! 3. **跳过纯注释行**（行首去空白后以 `//` 开头，含 `///` / `//!`）——文档里提到
//!    `host-mdns.browse(service-type)` / `ServiceDaemon::new` 是合法散文，不是调用点。
//!
//! 白名单精确到**文件**（不是目录）：新增调用点必须显式改本表并写明理由，改动必然
//! 出现在 diff 里。哨兵文件 + 扫描文件数下限防「枚举为空 → 断言恒真」。
//!
//! 锁自身文件不参与扫描（它携带全部禁用字面量，同既有结构锁的自锁规避口径）。

use std::path::{Path, PathBuf};

// 跨仓路径解析（平台仓兄弟目录 / WASMNEST_PLATFORM_ROOT）
#[path = "common/repo_paths.rs"]
mod repo_paths;

// ==================== 扫描面与白名单 ====================

/// 双端宿主源码根（相对平台仓根；整树纳入扫描）
const HOST_SRC_ROOTS: &[&str] = &[
    "wasmnest-desktop/src-tauri/src",
    "wasmnest-mobile/src-tauri/src",
];

/// 扫描文件数下限（实测 484：`packages/*/src` 312 + 桌面 53 + 移动 119；下调留余量）
const MIN_FILES: usize = 400;

/// 哨兵文件：枚举失败 / 路径写错时防「清单为空 → 断言恒真」
const SENTINEL_FILES: &[&str] = &[
    "packages/wasmnest-discovery-engine/src/type_hub.rs",
    "packages/wasmnest-discovery-engine/src/engine.rs",
    "wasmnest-desktop/src-tauri/src/lib.rs",
    "wasmnest-mobile/src-tauri/src/lib.rs",
];

/// `.browse(` / `.stop_browse(` 白名单（精确到文件）：唯一合法调用点 = 类型中枢。
///
/// 中枢是该服务类型 listener 槽的唯一所有者（建档 / 重查 / 后缘补齐都经它），
/// 其它任何调用点都会覆盖同一槽位、静默杀死既有订阅者的事件流。
const BROWSE_ALLOWLIST: &[&str] = &["packages/wasmnest-discovery-engine/src/type_hub.rs"];

/// `ServiceDaemon::new` 白名单（精确到文件）：唯一合法创建点 = 共享守护初始化。
///
/// 进程内第二个守护会与共享守护争抢同类型 listener（多守护各有自己的槽表），
/// 破坏「一个进程一个守护」的 ADR 0042 不变量。
const DAEMON_NEW_ALLOWLIST: &[&str] = &["packages/wasmnest-discovery-engine/src/engine.rs"];

/// 浏览调用判据针脚（`.browse(` / `.stop_browse(` 同属浏览面）
const BROWSE_NEEDLES: &[&str] = &[".browse(", ".stop_browse("];

/// 守护创建判据针脚
const DAEMON_NEW_NEEDLE: &str = "ServiceDaemon::new";

// ==================== 路径与扫描工具 ====================

/// 平台仓根（跨端测试仓的兄弟目录 `<WASMNEST>/wasmnest`）
fn platform_root() -> PathBuf {
    repo_paths::platform_root()
}

fn read_file(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("结构锁无法读取 {}：{e}", path.display()))
}

/// 纯注释行判定：行首去空白后以 `//` 开头（覆盖 `//` / `///` / `//!` 三种形态）
///
/// 文档注释里提到 `host-mdns.browse(...)` / `ServiceDaemon::new` 是合法散文，不是
/// 调用点；不跳过会把它们误报成回接。
fn is_comment_line(line: &str) -> bool {
    line.trim_start().starts_with("//")
}

/// 逐行扫描一份源码，返回 `(相对路径, 行号, 行内容)` 命中（跳过纯注释行）。
///
/// **判据与锁本体共用本函数**：契约例自己重写一遍匹配循环，就等于给漂移开了后门。
fn scan_source(rel: &str, src: &str, needles: &[&str]) -> Vec<(String, usize, String)> {
    let mut hits = Vec::new();
    for (idx, line) in src.lines().enumerate() {
        if is_comment_line(line) {
            continue;
        }
        for needle in needles {
            if line.contains(needle) {
                hits.push((
                    rel.to_string(),
                    idx + 1,
                    line.trim_end_matches('\r').to_string(),
                ));
            }
        }
    }
    hits
}

/// 扫描文件清单（读盘 → [`scan_source`]），返回排序去重后的命中
fn scan_files(files: &[String], needles: &[&str]) -> Vec<(String, usize, String)> {
    let root = platform_root();
    let mut hits = Vec::new();
    for rel in files {
        let src = read_file(&root.join(rel));
        hits.extend(scan_source(rel, &src, needles));
    }
    hits.sort();
    hits.dedup();
    hits
}

/// 递归枚举目录下的 `.rs`，路径相对 `repo_root` 记为带 `/` 的正斜杠形态。
///
/// `require_src_segment`：`packages/` 只收**位于 `src` 目录内**的文件（crate 源码面），
/// 双端宿主 `src-tauri/src` 整树收。`target/` 构建产物一律跳过；锁自身文件跳过。
fn walk(dir: &Path, repo_root: &Path, require_src_segment: bool, out: &mut Vec<String>) {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("结构锁无法枚举 {}：{e}", dir.display()));
    for entry in entries {
        let entry = entry.unwrap_or_else(|e| panic!("结构锁 read_dir 条目错误：{e}"));
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().and_then(|s| s.to_str()) == Some("target") {
                continue;
            }
            walk(&path, repo_root, require_src_segment, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            let rel = path
                .strip_prefix(repo_root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            // 锁自身携带全部禁用字面量，不参与扫描
            if rel.ends_with("mdns_browse_singleton_lock.rs") {
                continue;
            }
            if require_src_segment && !rel.contains("/src/") {
                continue;
            }
            out.push(rel);
        }
    }
}

/// 枚举全部扫描文件（能力仓 `packages/**/src`（经 submodule）+ 双端 `src-tauri/src`）
fn collect_rs_files() -> Vec<String> {
    // 标签基准统一取平台仓根：能力 crate 记为 `packages/<crate>/…`，宿主记为
    // `wasmnest-{desktop,mobile}/src-tauri/src/…`（与拆仓前的能力仓标签逐字一致）。
    let label_root = platform_root();
    let mut out = Vec::new();
    walk(&label_root.join("packages"), &label_root, true, &mut out);
    for rel in HOST_SRC_ROOTS {
        walk(&label_root.join(rel), &label_root, false, &mut out);
    }
    out.sort();
    out
}

/// 扫描面防空转：文件数下限 + 哨兵在场（枚举失败 / 路径写错时立刻红，不静默恒真）
fn assert_files_are_not_vacuous(files: &[String]) {
    assert!(
        files.len() >= MIN_FILES,
        "结构锁空转：只枚举到 {} 个 .rs（下限 {MIN_FILES}）——路径错或 read_dir 异常被吞",
        files.len()
    );
    for sentinel in SENTINEL_FILES {
        assert!(
            files.iter().any(|p| p == sentinel),
            "结构锁空转：枚举清单缺少哨兵文件 {sentinel}（实际 {} 个文件）",
            files.len()
        );
    }
}

fn format_hits(hits: &[(String, usize, String)]) -> String {
    hits.iter()
        .map(|(f, l, c)| format!("  {f}:{l}: {c}"))
        .collect::<Vec<_>>()
        .join("\n")
}

// ==================== 锁本体 ====================

/// 锁本体 A：`.browse(` / `.stop_browse(` 只由类型中枢调用
///
/// 变异自检：删掉 [`BROWSE_ALLOWLIST`] 条目（或在别处加一处 `.browse(`）⇒ 中枢
/// 自身的命中变成「预期外命中」，本用例立刻红。
#[test]
fn browse_and_stop_browse_are_only_called_by_the_type_hub() {
    let files = collect_rs_files();
    assert_files_are_not_vacuous(&files);

    let hits = scan_files(&files, BROWSE_NEEDLES);
    assert!(
        hits.iter()
            .any(|(f, _, _)| BROWSE_ALLOWLIST.contains(&f.as_str())),
        "白名单悬空：{} 里没有任何 `.browse(` / `.stop_browse(` 命中——白名单条目应删除或\
         本锁扫描面写错",
        BROWSE_ALLOWLIST.join(", ")
    );
    let unexpected: Vec<(String, usize, String)> = hits
        .iter()
        .filter(|(f, _, _)| !BROWSE_ALLOWLIST.contains(&f.as_str()))
        .cloned()
        .collect();
    assert!(
        unexpected.is_empty(),
        "mDNS 单 listener 不变量被绕过：`daemon.browse` / `daemon.stop_browse` 只允许由\
         类型中枢（{}）调用；其余调用点会覆盖同一服务类型的唯一 listener、静默杀死既有\
         订阅者的事件流（ADR 0054）\n{}",
        BROWSE_ALLOWLIST.join(", "),
        format_hits(&unexpected)
    );
}

/// 锁本体 B：`ServiceDaemon::new` 只在共享守护初始化处
///
/// 变异自检：删掉 [`DAEMON_NEW_ALLOWLIST`] 条目（或在别处 new 一个守护）⇒ 立刻红。
#[test]
fn service_daemon_is_only_created_in_the_shared_engine() {
    let files = collect_rs_files();
    assert_files_are_not_vacuous(&files);

    let hits = scan_files(&files, &[DAEMON_NEW_NEEDLE]);
    assert!(
        hits.iter()
            .any(|(f, _, _)| DAEMON_NEW_ALLOWLIST.contains(&f.as_str())),
        "白名单悬空：{} 里没有 `ServiceDaemon::new`——白名单条目应删除或本锁扫描面写错",
        DAEMON_NEW_ALLOWLIST.join(", ")
    );
    let unexpected: Vec<(String, usize, String)> = hits
        .iter()
        .filter(|(f, _, _)| !DAEMON_NEW_ALLOWLIST.contains(&f.as_str()))
        .cloned()
        .collect();
    assert!(
        unexpected.is_empty(),
        "共享守护唯一创建点被破坏：`ServiceDaemon::new` 只允许出现在 {}（ADR 0042/0054）\n{}",
        DAEMON_NEW_ALLOWLIST.join(", "),
        format_hits(&unexpected)
    );
}

/// 判据自身的契约例（防匹配规则被改坏后假绿）
///
/// 覆盖两件事：① 纯注释行（`//` / `///` / `//!`，含缩进形态）必须跳过——否则文档里的
/// 散文会误报；② 代码行必须命中——否则锁退化成恒绿。
#[test]
fn comment_lines_are_skipped_and_code_lines_are_caught() {
    let src = "//! doc: daemon.browse(x) 是散文\n\
               /// doc: ServiceDaemon::new()\n\
               // plain comment: .stop_browse(y)\n\
               let a = daemon.browse(ty);\n\
               let b = daemon.stop_browse(ty);\n\
               let c = ServiceDaemon::new();\n";
    let browse_hits = scan_source("x.rs", src, BROWSE_NEEDLES);
    assert_eq!(
        browse_hits.iter().map(|(_, l, _)| *l).collect::<Vec<_>>(),
        vec![4, 5],
        "注释行必须跳过、代码行必须命中（第 1/2/3 行是注释）"
    );
    let daemon_hits = scan_source("x.rs", src, &[DAEMON_NEW_NEEDLE]);
    assert_eq!(
        daemon_hits.iter().map(|(_, l, _)| *l).collect::<Vec<_>>(),
        vec![6],
        "`ServiceDaemon::new` 的注释行（第 2 行）必须跳过、代码行（第 6 行）必须命中"
    );

    // 缩进的注释行同样跳过（行首空白后再判 `//`）
    let indented = "    // .browse(ty) 在缩进注释里\n    let a = daemon.browse(ty);\n";
    assert_eq!(
        scan_source("y.rs", indented, BROWSE_NEEDLES)
            .iter()
            .map(|(_, l, _)| *l)
            .collect::<Vec<_>>(),
        vec![2],
        "缩进注释行必须跳过"
    );
}
