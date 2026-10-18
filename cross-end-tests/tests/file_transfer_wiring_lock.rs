//! 单产物接线防漂移锁（本 crate 的**对外契约**：核是单点，产物只有一份）。
//!
//! ## 为什么需要它
//!
//! 本重构的全部价值在「可共享面只有一份、产物只有一份」。而它最可能的失效形态不是崩溃，
//! 而是**慢性回流**：某天有人在端内「顺手补一个判据」，或在移动端重建一份 rust crate，
//! 双份实现就此复活，且没有任何测试会红——两份实现各自自洽时，所有单元测试都绿。
//!
//! 本锁把「核内判据不得回流 / 双适配器在场 / 移动端不得重建产物源」变成可执行断言。
//! 六条判据（单产物后从「双端各一份」改写为「一份产物 + 桌面/移动两套端口适配」）：
//!
//! 1. **零回流**：核内 23 个纯逻辑函数名，在共享产物 crate 的 `rust/src/**` 里
//!    **不得有同名 `fn` 定义**；
//! 2. **台账是纯转出**：共享 crate 的 `transfer_store.rs` 必须含逐字相同的转出行；
//! 3. **双适配器在场**：`adapters.rs`（桌面行为）与 `adapters_mobile.rs`（移动行为）
//!    都必须实现核的**已消费端口**（实现即接线证据；缺一侧 = 平台分支被拆）；
//! 4. **端口面登记在册**：核 `ports.rs` 的 `pub trait` 集合与钉死清单精确相等——新增差异面
//!    必须回到本锁改清单（一次显式决定），而不是悄悄多一个没人实现的 trait；
//! 5. **移动端产物源已退役**：`apps/file-transfer/mobile/rust/` 不得存在
//!    （重建 = 双产物复活），且共享 crate **只链桌面 SDK**（`Cargo.toml` 不得出现
//!    `wasmnest-plugin-api-mobile`——「移动实现」是桌面 SDK 上的第二套端口行为，
//!    不是第二套 SDK）；
//! 6. **双端包版本对齐受控**：共享产物被双端加载，import 名带
//!    `package wasmnest:plugin@<version>` ⇒ 双端版本必须相等，否则一端实例化失败。
//!    原「协商 ABI 受控」判据（`wasm_entry_with_abi!` + `SHARED_ABI_VERSION ≤ 双端
//!    ABI_VERSION`）随整数 ABI 退役（ADR 0051 D2）整体退役，改由本判据断言**双端
//!    端清单 package 版本与仓根真源逐字一致**（共享产物场景的唯一跨端约束）。
//! 7. **events 并集签名受控**：桌面 wit 的 `interface events` 必须恰好等于
//!    「桌面词汇 2 + 移动词汇 5」，且移动 5 函数签名与移动 wit **逐字一致**
//!    ——两端宿主都按函数名派发，缺函数或签名漂移都不会编译报错，只在该端运行时
//!    派发那一刻才炸。
//!
//! 扫描纪律（与宿主侧同款锁一致）：**剥行注释**（注释里点名函数解释「为什么」是正常的）、
//! 引号感知剥注释、needle 取不跨行的最短片段；测试子树（路径含 `tests` 段）跳过。
//! 判据 7 扫的是 **wit 文本**（文档注释前缀是 `///` 而非 `//`，按整行跳过）。
//!
//! 位置说明（三仓拆分票 08 批次三 → 2026-10-18 跨端测试面独立成仓）：本锁自应用仓
//! `apps/file-transfer/rust/src/wiring_lock.rs` 迁入平台仓 cross-end 测试面，
//! 2026-10-18 随该测试面再迁出为独立仓 `wasmnest-test`——它扫的是双端 SDK compose/WIT、
//! 仓根 contracts 与应用树，主题属平台契约（spec §5-E2b #9 / §7）。应用仓侧不再保留
//! 该测试（app 测试面至此零平台源码读取）。
//!
//! 路径基准：平台仓根经 `tests/common/repo_paths.rs` 解析（兄弟目录 `../wasmnest`
//! 或 `WASMNEST_PLATFORM_ROOT`）；应用仓经 `WASMNEST_APPS_ROOT` / 兄弟目录 resolver
//! （`tests/common/app_paths.rs`）。

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

// 平台侧测试读应用仓的集中 resolver（三仓拆分 §5-E7）
#[path = "common/app_paths.rs"]
mod app_paths;
// 跨仓路径解析（平台仓兄弟目录 / WASMNEST_PLATFORM_ROOT）
#[path = "common/repo_paths.rs"]
mod repo_paths;

/// 核内纯逻辑函数：共享产物 crate **不得**再定义同名 `fn`
///
/// 注意**不包含**端内合法的包装名（`load` / `save_and_push` / `load_all` / `ensure_table` /
/// `load_snapshot` / `save_snapshot` / `apply_and_push`）——那些正是适配层的职责。
const CORE_ONLY_FNS: &[&str] = &[
    "fnv1a",
    "root_id",
    "upsert",
    "remove",
    "merge_snapshot",
    "terminal_status_of",
    "reduce_event",
    "insert_active_projections",
    "prune_absent",
    "reconcile_diff",
    "mark_active_interrupted",
    "evict_overflow",
    "clear_terminal",
    "active_send_entries",
    "active_receive_entries",
    "mark_cancelled",
    "mark_paused",
    "apply_retry",
    "retry_source",
    "send_slot_open",
    "push_pull_intent",
    "take_pull_intent",
    "entry_from_dto",
];

/// 共享 crate 的 `transfer_store.rs` 必须逐字含本行（台账 = 纯转出）
const TRANSFER_STORE_REEXPORT: &str = "pub(crate) use wasmnest_file_transfer_core::transfer::*;";

/// 核内端口面（`ports.rs` 的 `pub trait`）登记清单——新增差异面必须回本锁登记
const REGISTERED_PORTS: &[&str] = &[
    "BusPort",
    "ConsentGate",
    "EventPort",
    "KvStore",
    "LogPort",
    "MdnsPort",
    "NodePower",
    "PeerPort",
    "PlatformPort",
    "PluginProfile",
    "RootWireCodec",
    "RootsStore",
];

/// 两个适配器文件必须实现的**已消费端口**（缺一即接线不完整）
///
/// `ConsentGate` 在册的依据：编排层 `lib.rs::consent_gate()` 按平台选实现，三个
/// 消费点（respond-consent / list-trusted / peer:consent 预检）全经端口——它一度
/// 是「有 trait、有单侧实现、零消费方」的死面，接线后才是真差异面。
const ADAPTER_MUST_IMPLEMENT: &[&str] =
    &["KvStore", "PeerPort", "RootWireCodec", "RootsStore", "PluginProfile", "ConsentGate"];

/// 平台行为 → 适配器文件（桌面行为 / 移动行为各一份，`platform` 探测选择）
const ADAPTER_FILES: &[(&str, &str)] = &[("desktop", "adapters.rs"), ("mobile", "adapters_mobile.rs")];

// ==================== 扫描器 ====================

/// 应用工程根（`<appsRoot>/apps/file-transfer`）
fn app_root() -> PathBuf {
    app_paths::apps_root().join("apps/file-transfer")
}

/// 共享产物 crate 根（`apps/file-transfer/rust`；核内自读 `src/ports.rs` 等在此）。
fn crate_root() -> PathBuf {
    app_root().join("rust")
}

/// 平台仓根（跨端测试仓的兄弟目录 `<WASMNEST>/wasmnest`）。
fn platform_root() -> PathBuf {
    repo_paths::platform_root()
}

/// 共享产物 crate 的源码目录（单产物唯一源：`apps/file-transfer/desktop/rust/src`）。
fn app_src_dir() -> PathBuf {
    app_root().join("desktop/rust/src")
}

/// 剥行注释（引号感知：字符串内的 `//` 不是注释起点）
fn strip_line_comments(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut out = String::with_capacity(line.len());
    let mut i = 0usize;
    let (mut in_str, mut in_char, mut escaped) = (false, false, false);
    while i < bytes.len() {
        let b = bytes[i];
        if escaped {
            out.push(b as char);
            escaped = false;
            i += 1;
            continue;
        }
        match b {
            b'\\' if in_str || in_char => {
                out.push(b as char);
                escaped = true;
            }
            b'"' if !in_char => {
                in_str = !in_str;
                out.push(b as char);
            }
            b'\'' if !in_str => {
                in_char = !in_char;
                out.push(b as char);
            }
            b'/' if !in_str && !in_char && i + 1 < bytes.len() && bytes[i + 1] == b'/' => break,
            _ => out.push(b as char),
        }
        i += 1;
    }
    out
}

/// 该行是否开启 `#[cfg(test…)]` 区（其后的夹具按设计要复刻形状，不参与判据）
fn opens_cfg_test(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("#[cfg(") && t.contains("test")
}

/// 收集目录下全部 `.rs`（路径含 `tests` 段跳过），返回 (相对路径, 生产代码行)
fn collect_prod(dir: &Path) -> Vec<(String, Vec<String>)> {
    let mut files = Vec::new();
    walk(dir, dir, &mut files);
    files.sort_by(|a, b| a.0.cmp(&b.0));
    files
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<String>)>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    let mut paths: Vec<PathBuf> = entries.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    paths.sort();
    for p in paths {
        if p.is_dir() {
            // 路径含 `tests` 段（目录化的测试子树）整个跳过
            if p.file_name().and_then(|n| n.to_str()) == Some("tests") {
                continue;
            }
            walk(root, &p, out);
        } else if p.extension().and_then(|e| e.to_str()) == Some("rs") {
            let Ok(text) = fs::read_to_string(&p) else { continue };
            let mut lines = Vec::new();
            for line in text.lines() {
                if opens_cfg_test(line) {
                    break;
                }
                lines.push(strip_line_comments(line.trim_end_matches('\r')));
            }
            let rel = p.strip_prefix(root).unwrap_or(&p).to_string_lossy().replace('\\', "/");
            out.push((rel, lines));
        }
    }
}

/// 是否定义了名为 `name` 的函数（`fn name(`，允许 `pub(crate) fn` / `async fn` 前缀）
fn defines_fn(lines: &[String], name: &str) -> bool {
    let needle = format!("fn {name}(");
    lines.iter().any(|l| l.contains(&needle))
}

/// 是否出现某一 needle
fn contains(lines: &[String], needle: &str) -> bool {
    lines.iter().any(|l| l.contains(needle))
}

/// 读文件的生产代码行（到首个 `#[cfg(test…)]` 为止，剥行注释）
fn prod_lines(path: &Path) -> Vec<String> {
    let text = fs::read_to_string(path).unwrap_or_default();
    text.lines()
        .take_while(|l| !opens_cfg_test(l))
        .map(|l| strip_line_comments(l.trim_end_matches('\r')))
        .collect()
}

// ==================== 用例 ====================

/// 扫描器自检：既不能假绿（漏真回流），也不能假红（把注释当代码）
#[test]
fn scanner_is_not_vacuous() {
    let sample =
        vec!["// fn reduce_event(...) 解释性注释".to_string(), "let s = \"fn upsert(\";".to_string()];
    // 保守方向：抹平「字符串字面量里出现 `fn upsert(`」也照报（宁吵不静守——静守的代价是回流被漏掉）
    assert!(
        defines_fn(&sample, "upsert"),
        "字符串字面量里的函数名照报（保守方向：宁可假警报，不可静默失守）"
    );
    assert!(
        !defines_fn(&[strip_line_comments("// fn reduce_event(x: u8) {")], "reduce_event"),
        "注释里的函数名不得算定义，否则锁会被解释性注释淹没"
    );
    let kept = strip_line_comments("fn a(); // fn b()");
    assert!(kept.contains("fn a()"), "注释前的真代码必须保留");
    assert!(!kept.contains("fn b()"), "注释必须被剥掉");
    // 共享产物源码目录必须真实存在（判据路径漂移 = 扫描器空转）
    assert!(app_src_dir().is_dir(), "共享产物源码目录不存在：{}", app_src_dir().display());
}

/// 判据 1：零回流——核内判据不得在共享产物 crate 里重新长出定义
#[test]
fn pure_logic_is_not_reimplemented_in_shared_app() {
    let dir = app_src_dir();
    assert!(dir.is_dir(), "共享产物源码目录不存在：{}（锁的判据路径已漂移）", dir.display());
    let mut violations = Vec::new();
    for (rel, lines) in collect_prod(&dir) {
        for name in CORE_ONLY_FNS {
            if defines_fn(&lines, name) {
                violations.push(format!("shared-app/{rel}: 重新定义了核内判据 `fn {name}(`"));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "共享核的判据在产物 crate 里复活（两份实现的漂移就从这里开始，且各自单测都会绿）：\n  - {}",
        violations.join("\n  - ")
    );
}

/// 判据 2：共享 crate 的 `transfer_store.rs` 是纯转出
#[test]
fn transfer_store_stays_a_pure_reexport() {
    let path = app_src_dir().join("transfer_store.rs");
    let prod = prod_lines(&path);
    assert!(
        contains(&prod, TRANSFER_STORE_REEXPORT),
        "台账不再是纯转出（产物 crate 又自带实现 ⇒ 判据有两个真源）：{}",
        path.display()
    );
}

/// 判据 3：桌面/移动两个适配器都在场且实现已消费端口（拆一侧 = 平台分支被拆）
#[test]
fn both_platform_adapters_ship_consumed_ports() {
    let mut problems = Vec::new();
    for (platform, file) in ADAPTER_FILES {
        let path = app_src_dir().join(file);
        let Ok(text) = fs::read_to_string(&path) else {
            problems.push(format!("{platform}: 缺 {file}（{}）", path.display()));
            continue;
        };
        let prod: Vec<String> = text
            .lines()
            .take_while(|l| !opens_cfg_test(l))
            .map(|l| strip_line_comments(l.trim_end_matches('\r')))
            .collect();
        for port in ADAPTER_MUST_IMPLEMENT {
            // 端口是**被实现的 trait**（在 `for` 之前），不是泛型约束：`impl<H: SdkTrait> Port for X`
            let needle = format!("{port} for ");
            if !contains(&prod, &needle) {
                problems.push(format!(
                    "{platform}/{file}: 未见端口 `{port}` 的实现（找 `{needle}`）"
                ));
            }
        }
        if !contains(&prod, "PortError") {
            problems.push(format!(
                "{platform}/{file}: 未见 `PortError`（错误类型未转换 ⇒ 适配层形态不对）"
            ));
        }
    }
    assert!(
        problems.is_empty(),
        "适配器面不完整（平台分支被拆或未跟上核的端口面）：\n  - {}",
        problems.join("\n  - ")
    );
}

/// 判据 4：核端口面登记在册——`ports.rs` 的 `pub trait` 集合与清单精确相等
#[test]
fn core_port_surface_is_registered() {
    let ports = crate_root().join("src/ports.rs");
    let text = fs::read_to_string(&ports).expect("读核 ports.rs");
    let prod: Vec<String> = text
        .lines()
        .take_while(|l| !opens_cfg_test(l))
        .map(|l| strip_line_comments(l.trim_end_matches('\r')))
        .collect();

    let mut observed: BTreeSet<String> = BTreeSet::new();
    for line in &prod {
        let Some(rest) = line.trim().strip_prefix("pub trait ") else { continue };
        let name: String =
            rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
        if !name.is_empty() {
            observed.insert(name);
        }
    }
    let expected: BTreeSet<String> = REGISTERED_PORTS.iter().map(|s| s.to_string()).collect();

    assert!(!observed.is_empty(), "端口扫描器空转（`pub trait` 一个都没扫到）—— 判据失效须修锁");
    assert_eq!(
        observed,
        expected,
        "核端口面与登记清单不一致：新增/改名/删除端口都必须回到本锁改清单——\
         差异面是**显式决定**，不是随手加一个没人实现的 trait"
    );
    assert_eq!(REGISTERED_PORTS.len(), expected.len(), "登记清单自身有重复项");
}

/// 判据 5a：移动端产物源已退役——不得重建 rust crate（重建 = 双产物复活）
#[test]
fn mobile_artifact_source_stays_retired() {
    let retired = app_root().join("mobile/rust");
    assert!(
        !retired.exists(),
        "移动端 file-transfer rust/ 重新出现（{}）：双端单产物的产物源只有桌面 \
         wasm-apps/file-transfer/rust——移动端差异是同一 crate 内的平台分支，不是第二份产物源",
        retired.display()
    );
}

/// 判据 5b：共享产物 crate 只链桌面 SDK
///
/// 「移动实现」不是第二套 SDK，而是桌面 SDK（core interface 双端提供）上的第二套
/// 端口行为。链移动 SDK 会让产物 import 移动 cap 界面 ⇒ 桌面宿主实例化失败。
#[test]
fn shared_app_depends_on_desktop_sdk_only() {
    let manifest = app_root().join("desktop/rust/Cargo.toml");
    let text = fs::read_to_string(&manifest).expect("读共享 crate Cargo.toml");
    let prod: Vec<String> = text
        .lines()
        .take_while(|l| !opens_cfg_test(l))
        .map(|l| strip_line_comments(l.trim_end_matches('\r')))
        .collect();
    assert!(
        contains(&prod, "wasmnest-plugin-api"),
        "共享 crate 必须依赖桌面 SDK（wasmnest-plugin-api）"
    );
    assert!(
        !contains(&prod, "wasmnest-plugin-api-mobile"),
        "共享 crate 出现了移动 SDK 依赖（wasmnest-plugin-api-mobile）：产物将 import \
         移动 cap 界面、桌面宿主无法实例化——移动行为应经桌面 SDK 的 core interface 表达"
    );
}

/// 判据 6：双端包版本对齐受控——共享产物跨双端加载，两端版本必须相等
///
/// **为什么这是共享产物场景的唯一跨端约束**：契约版本进入 import 名后，wasmtime 按
/// 含版本的完整名精确匹配 ⇒ 双端版本不等即有一端实例化期被拒。非共享产物场景双端各自
/// bump 即可（各自 host↔guest 自洽），不受本判据约束（ADR 0051 D3）。
#[test]
fn shared_artifact_ends_align_on_one_package_version() {
    // 仓根真源（Rust 常量文件，票 03 单点化）
    let source = fs::read_to_string(platform_root().join("packages/contracts/wasmnest-plugin-package.rs"))
        .expect("读仓根契约包真源 contracts/wasmnest-plugin-package.rs");
    let const_of = |name: &str| -> String {
        let needle = format!("pub const {name}: &str = \"");
        let start = source
            .find(&needle)
            .unwrap_or_else(|| panic!("真源缺常量 {name}"))
            + needle.len();
        let rest = &source[start..];
        let end = rest.find('"').expect("常量未闭合");
        rest[..end].to_string()
    };
    let (name, version) = (const_of("PACKAGE_NAME"), const_of("PACKAGE_VERSION"));
    assert_eq!(
        const_of("PACKAGE"),
        format!("{name}@{version}"),
        "真源自相矛盾：PACKAGE != PACKAGE_NAME@PACKAGE_VERSION"
    );

    // 双端端清单只声明包名，版本由拼装脚本注入 ⇒ 两端清单必须同指一个包名
    for end in [
        "wasmnest-desktop/packages/plugin-sdk-desktop/compose.json",
        "wasmnest-mobile/packages/plugin-sdk-mobile/compose.json",
    ] {
        let text = fs::read_to_string(platform_root().join(end)).expect("读端清单");
        let declared = text
            .split_once("\"package\": \"")
            .and_then(|(_, rest)| rest.split_once('"'))
            .map(|(value, _)| value)
            .unwrap_or_else(|| panic!("{end} 缺 package 字段"));
        assert_eq!(
            declared, name,
            "{end} 的 package 名与仓根真源不一致——共享产物按一端 world 构建，\
             双端包名/版本必须同源"
        );
    }

    // 退役面不得复活：协商 ABI 机制（宏 + 常量）已随整数 ABI 退役
    let prod = prod_lines(&app_src_dir().join("lib.rs"));
    for banned in ["wasm_entry_with_abi!", "SHARED_ABI_VERSION"] {
        assert!(
            !contains(&prod, banned),
            "`{banned}` 已随整数 ABI 退役（ADR 0051 D2）——版本一致性由双端包版本\
             对齐承担，不得复活产物自报协商值机制"
        );
    }
}

// ==================== 判据 7：events 并集签名锁 ====================

/// 桌面 `events` 自有词汇（桌面 dispatcher 按名调用）
const DESKTOP_EVENT_VOCAB: &[&str] = &["on-message", "on-process-done"];

/// 移动 `events` 词汇（移动 dispatcher 按名调用；共享产物按桌面 world 构建，
/// 故这 5 个函数必须**逐字**出现在桌面 wit 里——签名漂移则移动宿主派发时才发现）
const MOBILE_EVENT_VOCAB: &[&str] = &[
    "on-bus-message",
    "on-auth-success",
    "on-disconnect",
    "on-session-created",
    "on-session-stopped",
];

/// 从 wit 文本抽出 `interface events` 的函数声明（`name: func(...) -> ...;`）
///
/// WIT 文档注释用 `///`（与 Rust 的 `//` 不同），这里按整行前缀跳过；
/// 函数声明在本仓 wit 里一律单行，故按分号切分即可。
fn wit_event_signatures(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut inside = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with("interface events") {
            inside = true;
            continue;
        }
        if inside {
            if line.starts_with('}') {
                break;
            }
            if line.starts_with("///") || line.is_empty() || line.starts_with("//") {
                continue;
            }
            // `name: func(args) -> result<_, string>;`
            let decl = line.trim_end_matches(';').trim();
            let Some((name, sig)) = decl.split_once(':') else { continue };
            let name = name.trim();
            if !name.is_empty() && sig.trim_start().starts_with("func") {
                out.insert(name.to_string(), sig.trim().to_string());
            }
        }
    }
    out
}

/// 判据 7：`events` 并集导出受控——桌面 wit 的 `events` = 桌面词汇 ∪ 移动词汇，
/// 且移动 5 函数签名与移动 wit **逐字一致**
///
/// **为什么必须锁**：两端宿主都按**函数名**派发（桌面调 `on-message`，移动调
/// `on-bus-message`），wit-bindgen 对导出做按名懒查找。少一个函数、或签名漂移，
/// 都不会编译报错——只在该端**运行时派发那一刻**才炸，且报错形态是「导出不存在」，
/// 极难反推到「是共享产物少导了移动词汇」。
#[test]
fn desktop_events_interface_is_union_of_both_vocabularies() {
    let desktop_wit = platform_root()
        .join("wasmnest-desktop/packages/plugin-sdk-desktop/rust/wit-src/cap-desktop.wit");
    let mobile_wit = platform_root()
        .join("wasmnest-mobile/packages/plugin-sdk-mobile/rust/wit-src/cap-mobile.wit");

    let desktop =
        wit_event_signatures(&fs::read_to_string(&desktop_wit).expect("读桌面 cap-desktop.wit"));
    let mobile =
        wit_event_signatures(&fs::read_to_string(&mobile_wit).expect("读移动 cap-mobile.wit"));

    assert!(
        !desktop.is_empty() && !mobile.is_empty(),
        "events 扫描器空转（桌面 {} 项 / 移动 {} 项）——判据路径或解析规则已漂移",
        desktop.len(),
        mobile.len()
    );

    let mut problems = Vec::new();
    // 移动词汇逐字一致（签名漂移 ⇒ 移动宿主派发面与产物导出面不匹配）
    for name in MOBILE_EVENT_VOCAB {
        let Some(mobile_sig) = mobile.get(*name) else {
            problems.push(format!("移动 wit 的 events 缺 `{name}`（词汇真源被改名/删除）"));
            continue;
        };
        match desktop.get(*name) {
            None => problems.push(format!(
                "桌面 wit 的 events 缺移动词汇 `{name}`：共享产物按桌面 world 构建，\
                 少导该函数 ⇒ 移动宿主派发时才发现"
            )),
            Some(desktop_sig) if desktop_sig != mobile_sig => problems.push(format!(
                "`{name}` 签名漂移：桌面 `{desktop_sig}` ≠ 移动 `{mobile_sig}`"
            )),
            Some(_) => {}
        }
    }
    // 桌面自有词汇仍在（并集不是替换：桌面 dispatcher 仍按名调那 2 个）
    for name in DESKTOP_EVENT_VOCAB {
        if !desktop.contains_key(*name) {
            problems.push(format!("桌面 wit 的 events 缺桌面自有词汇 `{name}`"));
        }
    }
    // 并集恰好等于两侧词汇之和（多出的函数 = 未登记的第三端词汇，须显式决定）
    let expected: BTreeSet<&str> = DESKTOP_EVENT_VOCAB
        .iter()
        .chain(MOBILE_EVENT_VOCAB.iter())
        .copied()
        .collect();
    let observed: BTreeSet<&str> = desktop.keys().map(String::as_str).collect();
    if observed != expected {
        problems.push(format!(
            "桌面 events 不是登记并集：多出 {:?} / 缺失 {:?}——新增端词汇必须回到本锁登记",
            observed.difference(&expected).collect::<Vec<_>>(),
            expected.difference(&observed).collect::<Vec<_>>()
        ));
    }

    assert!(
        problems.is_empty(),
        "events 并集面漂移（共享产物按桌面 world 构建，vocabulary 缺失只在该端派发时才炸）：\n  - {}",
        problems.join("\n  - ")
    );
}
