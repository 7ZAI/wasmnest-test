//! BedCode 应用身份锁 V3（ADR 0056 D3 豁免三件 + D5 id 家族全改，spec §6 V3）
//!
//! ## 锁什么 / 为什么
//!
//! ADR 0056 把「BedCode」从平台名降级为**双端目录下的同一个 wasm-app**：
//! 平台品牌全量改 `wasmnest`，而该应用只保留**机器身份三件豁免**——
//! ① 显示名 `BedCode`、② rust lib 名 `bedcode_plugin_terminal_session`、
//! ③ 目录 slug `wasm-apps/bedcode/`；其余（plugin id、author 等平台引用）**照改**
//! （D5：`com.bedcode` 家族整体进替换流，豁免 Id 已作废）。
//!
//! 这三件豁免与「id/author 已改」互为反向约束：任一端把某个字段漏改（还是旧
//! `com.bedcode.*`）或**过度改**（把豁免件也改成 `wasmnest`，应用即失去身份、
//! 产物目录 / rust 库名解析断链、已持久化的授权记录集体成孤儿）都会在双端之一
//! 显性红。锁文件只读静态清单（双端 `plugin.json`）与目录在场性，不启运行时。
//!
//! ## 行为契约表（来源 = ADR 0056 D3/D5 + spec §6 V3）
//!
//! | 契约 | 来源 | 规则 | 预期 |
//! | --- | --- | --- | --- |
//! | C1 | V3 / D5 | 双端 `plugin.json` 的 `id` = `com.wasmnest.bedcode`，且双端同值 | 相等（正例）/ 旧 `com.bedcode.*` 即红（反例） |
//! | C2 | V3 / D3 ① | 双端 `name` = `BedCode`（显示名豁免，**不得**被改成 `WasmNest`） | 逐字相等 |
//! | C3 | V3 / D3 ② | 双端 `rustLibrary` = `bedcode_plugin_terminal_session`（豁免保留） | 逐字相等 |
//! | C4 | V3 / D3 ③ | 双端目录 slug `wasmnest-{desktop,mobile}/wasm-apps/bedcode/` 在场且含 `plugin.json` | 目录是目录、清单是文件 |
//! | C5 | D5 反例 | 双端清单**原文**不含旧 id 家族字面 `com.bedcode` | 零命中 |
//! | C6 | 逻辑一致性 | 双端清单声明的每条 `api`（缺 `api` 字段的一端按空集）均以 `<id>.` 为前缀（非空侧非空以证不空跑） | 全部前缀命中 |
//! | C7 | D3 反面 | `author` = `WasmNest`（平台作者，**非**豁免）——证明豁免是字面量级、非目录级 | 逐字相等 |
//! | C8 | 防空跑 | 双端清单可读、可解析为 JSON 对象且四个身份字段类型正确 | 全成立 |

use std::path::PathBuf;

use serde_json::Value;

#[path = "common/app_paths.rs"]
mod app_paths;

/// 双端（三仓拆分 §5-E7：应用源码住 `<appsRoot>/apps/bedcode/{desktop,mobile}`）
const END_DIRS: [&str; 2] = ["desktop", "mobile"];
/// 终态 plugin id（D5：旧 `com.bedcode.terminal-session` 已作废；2026-10-25 复由 `com.wasmnest.terminal-session` 改名至今值）
const ID: &str = "com.wasmnest.bedcode";
/// 显示名（D3 ① 豁免）
const DISPLAY_NAME: &str = "BedCode";
/// rust lib 名（D3 ② 豁免；其 kebab 派生形 `bedcode-plugin-terminal-session` 由
/// 桌面 app 的 `Cargo.toml` 持有，由 `plugin-contract.test.ts` 的 C1 单独钉）
const RUST_LIB: &str = "bedcode_plugin_terminal_session";
/// 平台作者（D3 反面：`author` 不是豁免，随品牌改）
const AUTHOR: &str = "WasmNest";

/// 某端的应用工程根：`<appsRoot>/apps/bedcode/<end>`
fn app_root(end: &str) -> PathBuf {
    app_paths::app_dir("bedcode", end)
}

/// 读取并解析某端的 `plugin.json`，同时返回原文（C5 需要原文扫描）
fn read_manifest(end_dir: &str) -> (PathBuf, Value, String) {
    let path = app_root(end_dir).join("plugin.json");
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("读应用仓 apps/bedcode/{end_dir}/plugin.json 失败: {e}"));
    let json: Value = serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("解析 {end_dir} 的 plugin.json 失败（清单必须是合法 JSON）: {e}"));
    (path, json, raw)
}

/// 取字符串字段（缺字段 / 类型不符即红——防空跑，防止断言对象消失后静默通过）
fn field<'a>(json: &'a Value, end_dir: &str, key: &str) -> &'a str {
    json.get(key)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("{end_dir} 的 plugin.json 缺字符串字段 `{key}`"))
}

#[test]
fn c4_both_ends_keep_the_bedcode_app_dir_slug() {
    // D3 ③：目录 slug 是应用身份的一部分，应用仓两端都必须保留 `bedcode` 目录，
    // 且其下确有清单文件（目录在场但清单没了 = 断链，不能只判目录存在）。
    // 三仓拆分后目录为 `apps/bedcode/{desktop,mobile}`——slug 段仍是 `bedcode`。
    for end_dir in END_DIRS {
        let root = app_root(end_dir);
        assert!(
            root.is_dir(),
            "apps/bedcode/{end_dir}/ 目录不在场——应用目录 slug 是 ADR 0056 D3 ③ 的豁免件，改名即断链"
        );
        assert!(
            root.join("plugin.json").is_file(),
            "apps/bedcode/{end_dir}/plugin.json 不在场（目录在场但清单缺失 = 断链）"
        );
    }
}

#[test]
fn c1_plugin_id_is_wasmnest_terminal_session_on_both_ends() {
    // 跨迭代持有上端的 id 做同值比对：`json` 每轮即 drop，故拷贝成 `String`
    // （借用会触发 E0597：借出的值活不过本轮作用域）
    let mut seen: Option<String> = None;
    for end_dir in END_DIRS {
        let (_, json, _) = read_manifest(end_dir);
        let id = field(&json, end_dir, "id");
        assert_eq!(
            id, ID,
            "{end_dir} 的 plugin id 应为终态 `{ID}`（ADR 0056 D5：旧 `com.bedcode.terminal-session` 已作废）\
             ——漏改即与宿主按 id 绑定的门禁/授权记录失配"
        );
        // C1 内的交叉项：双端同 id（移动端是同 id 不同职责的应用，id 必须逐字一致）
        if let Some(prev) = &seen {
            assert_eq!(id, prev, "双端 plugin id 必须同值（当前 {end_dir}={id} vs {prev}）");
        }
        seen = Some(id.to_string());
    }
}

#[test]
fn c2_display_name_stays_bedcode_on_both_ends() {
    for end_dir in END_DIRS {
        let (_, json, _) = read_manifest(end_dir);
        assert_eq!(
            field(&json, end_dir, "name"),
            DISPLAY_NAME,
            "{end_dir} 的显示名必须是 `{DISPLAY_NAME}`（ADR 0056 D3 ① 豁免）——\
             被改成 `WasmNest` 即应用丢失用户可见品牌身份"
        );
    }
}

#[test]
fn c3_rust_library_keeps_legacy_lib_name_on_both_ends() {
    for end_dir in END_DIRS {
        let (_, json, _) = read_manifest(end_dir);
        assert_eq!(
            field(&json, end_dir, "rustLibrary"),
            RUST_LIB,
            "{end_dir} 的 rustLibrary 必须是 `{RUST_LIB}`（ADR 0056 D3 ② 豁免）——\
             宿主按该库名解析 wasm 产物，改名即加载失败"
        );
    }
}

#[test]
fn c5_no_legacy_com_bedcode_id_family_in_manifests() {
    // D5 反例：豁免后 `com.bedcode` 家族清零；清单原文扫（而非仅 id 字段），
    // 以覆盖 api 列表 / contributes 等任何位置复燃的旧 id 字面
    for end_dir in END_DIRS {
        let (_, _, raw) = read_manifest(end_dir);
        assert!(
            !raw.contains("com.bedcode"),
            "{end_dir} 的 plugin.json 仍含旧 id 家族字面 `com.bedcode`——\
             ADR 0056 D5 已取消 id 豁免，任何位置复燃都会与宿主新 id 失配"
        );
    }
}

#[test]
fn c6_declared_api_entries_are_namespaced_under_the_new_id() {
    let mut total = 0usize;
    for end_dir in END_DIRS {
        let (_, json, _) = read_manifest(end_dir);
        let api = json
            .get("api")
            .and_then(Value::as_array)
            // 移动端清单当前不带 `api` 面（它的命令面走 `contributes.commands`，无互调
            // 声明）；缺字段按空集处理，由下方总量下限保证本锁不空跑
            .cloned()
            .unwrap_or_default();
        let prefix = format!("{ID}.");
        for entry in api {
            let entry = entry
                .as_str()
                .unwrap_or_else(|| panic!("{end_dir} 的 api 项必须是字符串"));
            assert!(
                entry.starts_with(&prefix),
                "{end_dir} 的 api 项 `{entry}` 未以 `{prefix}` 为前缀——\
                 声明面与 plugin id 不同源会让宿主「未声明 api 不可调」门禁整片拒绝"
            );
            total += 1;
        }
    }
    // 防空跑：桌面端清单确有非空 api 声明（终端会话应用的核心契约面）；
    // 两端口径不同（移动端当前无 api 面），故只对总量设下限，避免脆弱耦合
    assert!(
        total >= 20,
        "担保解析只数到 {total} 条 api——扫描写法失配或声明面被清空（本锁将空跑）"
    );
}

#[test]
fn c7_author_follows_the_platform_brand_on_both_ends() {
    // D3 反面：`author` 是平台（作者=WasmNest）不是豁免件。若某端把它也留在
    // 旧品牌或误当豁免保留，说明「豁免是字面量级而非目录级」的口径被破坏
    for end_dir in END_DIRS {
        let (_, json, _) = read_manifest(end_dir);
        assert_eq!(
            field(&json, end_dir, "author"),
            AUTHOR,
            "{end_dir} 的 author 应为平台作者 `{AUTHOR}`（ADR 0056 D3：author 不属于豁免三件）"
        );
    }
}

#[test]
fn c8_identity_lock_itself_is_non_empty() {
    // 防空跑自检：两份清单都能读到、都解析出四个身份字段。前面任一断言若因
    // 「路径写错 → 读到别的文件」而假绿，这里会先红（文件不存在即 panic）
    for end_dir in END_DIRS {
        let (path, json, raw) = read_manifest(end_dir);
        assert!(
            !raw.trim().is_empty(),
            "{end_dir} 的 plugin.json 为空: {}",
            path.display()
        );
        for key in ["id", "name", "rustLibrary", "author"] {
            assert!(
                json.get(key).and_then(Value::as_str).is_some_and(|s| !s.is_empty()),
                "{end_dir} 的 plugin.json 字段 `{key}` 缺失或为空: {}",
                path.display()
            );
        }
    }
}
