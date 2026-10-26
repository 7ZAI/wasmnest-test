//! 权限词汇漂移锁（三仓拆分 L2/E2b · 票 08 自能力仓迁入平台仓；2026-10-18 随跨端
//! 测试面再迁出为独立仓 `wasmnest-test`）
//!
//! ## 锁什么
//!
//! 权限词汇的**单一真源**是能力仓 `wasmnest-wasm-core::permission` 的
//! `PERMISSION_VOCABULARY` / `VALID_PERMISSIONS` / `PERMISSION_API_MAP`（wasm-core
//! 公开面，宿主与打包链都消费）。本锁把真源钉到平台侧的五处事实：
//!
//! 1. 打包 CLI 生成物 `permission-vocabulary.json` —— 与真源**集合相等**（非包含），
//!    API 映射逐条相等（生成器漏跑 / 手改生成物即红）；
//! 2. 生成物自称生成物、且 `manifest-validate` 确实消费它（不是手抄清单）；
//! 3. 每条词汇都有门禁落点（能力仓 wasm-core 源码 + 桌面宿主源码里出现常量标识或
//!    权限串，或在前端 API 映射里有方法可门）——没有「无人执行」的死权限位；
//! 4. 生产 manifest（应用仓桌面端 + 平台 SDK 夹具）只声明词汇表内权限；
//! 5. `wasmnest-plugin validate` 挂在插件构建链上（校验规则写在 CLI 里却没人跑 =
//!    给后人「有锁」的错觉）。
//!
//! ## 为什么在跨端测试仓
//!
//! 真源在能力仓，但真源本身是**公开常量**（本仓经 dev-dep `use` 直接读，不读源
//! 文件、不做解析）；而比对对象（SDK 生成物 / 宿主源码 / 构建脚本）与扫描面
//! （应用仓 manifest + 平台夹具）全在平台侧。票 08 把锁迁出能力仓：
//! 能力仓单克隆不再读平台文件，判据与断言逐字保留。2026-10-18 跨端测试面独立
//! 成仓后，平台仓 / 能力仓 / 应用仓均经 [`repo_paths`] / [`app_paths`] 解析。
//!
//! 换行符归一化口径与其余跨仓锁一致；读文件失败一律 panic（防空转）。
//!
//! 2026-10-18：本锁随跨端测试面自平台仓迁入独立仓 `wasmnest-test`，路径基准改为
//! [`repo_paths::platform_root`]（平台仓兄弟目录 / `WASMNEST_PLATFORM_ROOT`）。

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use wasmnest_wasm_core::permission::{PERMISSION_API_MAP, PERMISSION_VOCABULARY, VALID_PERMISSIONS};

#[path = "common/app_paths.rs"]
mod app_paths;
// 跨仓路径解析（平台仓 / 能力仓 submodule / 应用仓）——本测试仓已自平台仓迁出
#[path = "common/repo_paths.rs"]
mod repo_paths;

/// 平台仓根（跨端测试仓的兄弟目录 `<WASMNEST>/wasmnest`）
fn repo_root() -> PathBuf {
    repo_paths::platform_root()
}

/// 桌面工程根（SDK / 夹具 / 宿主 src 的公共前綴）
fn desktop_root() -> PathBuf {
    repo_root().join("wasmnest-desktop")
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| {
        panic!(
            "{} 不可读: {e}（生成物缺失？重跑 SDK 的 pnpm run gen:permissions）",
            path.display()
        )
    })
}

/// 剥掉纯注释行（整行 `//` / `*` / `/*` 前缀）
///
/// 漂移锁扫描「源码/生成物里出现权限词即算落点」时，注释/文档里的权限词不该算数
/// ——纯注释行剥掉后再扫，只有真实代码引用才算门禁落点；生成物解析同理。
/// 只剥**整行注释**，不动行内注释与字符串（避免误伤 `"http://..."`）。
fn strip_comment_lines(text: &str) -> String {
    text.lines()
        .filter(|line| {
            let t = line.trim_start();
            !(t.starts_with("//") || t.starts_with("/*") || t.starts_with("*/") || t.starts_with("*"))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 解析打包 CLI 生成物：`(权限词汇, API 映射表)`
fn parse_generated_json(text: &str) -> (BTreeSet<String>, BTreeMap<String, Vec<String>>) {
    let value: serde_json::Value =
        serde_json::from_str(text).expect("CLI 生成物 permission-vocabulary.json 不是合法 JSON");
    let permissions = value["permissions"]
        .as_array()
        .expect("生成物缺 permissions 数组")
        .iter()
        .map(|p| p.as_str().expect("权限必须是字符串").to_string())
        .collect();
    let mut api_map = BTreeMap::new();
    if let Some(map) = value["apiMap"].as_object() {
        for (perm, methods) in map {
            api_map.insert(
                perm.clone(),
                methods
                    .as_array()
                    .expect("apiMap 值必须是数组")
                    .iter()
                    .map(|m| m.as_str().expect("方法名必须是字符串").to_string())
                    .collect(),
            );
        }
    }
    (permissions, api_map)
}

/// 真源词汇集合（wasm-core 公开面）
fn sdk_vocabulary() -> BTreeSet<String> {
    VALID_PERMISSIONS.iter().map(|p| (*p).to_string()).collect()
}

/// 真源 API 映射（wasm-core 公开面）
fn sdk_api_map() -> BTreeMap<String, Vec<String>> {
    PERMISSION_API_MAP
        .iter()
        .map(|(perm, apis)| ((*perm).to_string(), apis.iter().map(|a| (*a).to_string()).collect()))
        .collect()
}

/// 递归收集目录下的源码文件
fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("宿主插件目录可读") {
        let path = entry.expect("目录条目可读").path();
        if path.is_dir() {
            collect_rs(&path, out);
        } else if path.extension().map(|e| e == "rs").unwrap_or(false) {
            out.push(path);
        }
    }
}

/// 递归收集目录下全部 `*.json` 文件（R-13：嵌套目录中的 manifest 不得逃逸扫描）
///
/// 跳过 `node_modules` / `target`（第三方依赖与构建产物里的 JSON 不是 plugin
/// manifest；tsconfig 等带注释的 JSON 也会让解析直接崩掉）
fn collect_json(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("目录可读") {
        let path = entry.expect("目录条目可读").path();
        if path.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str());
            // node_modules / target：第三方依赖与构建产物；template：脚手架模板
            // （含 `${...}` 占位符，不是合法 JSON 也不是真实 manifest）
            if matches!(name, Some("node_modules" | "target" | "template" | "templates")) {
                continue;
            }
            collect_json(&path, out);
        } else if path.extension().is_some_and(|e| e == "json") {
            out.push(path);
        }
    }
}

// ==================== 两副本集合相等 ====================

/// 漂移锁①：真源（能力仓 wasm-core 公开常量）== 打包 CLI 生成物（集合相等，非包含）
///
/// 生成物由 SDK 的 `gen_permission_vocabulary` 出，正常情况下恒等；本断言存在的
/// 意义是**手改生成物即转红**，以及生成物缺文件/解析空转不再静默通过。
#[test]
fn permission_vocabulary_is_equal_across_copies() {
    let json = read(&desktop_root().join("packages/plugin-sdk-desktop/bin/permission-vocabulary.json"));

    let (json_perms, json_api) = parse_generated_json(&json);
    let sdk_perms = sdk_vocabulary();

    assert!(
        sdk_perms.len() >= 30,
        "SDK 词汇表实测 {n} 条，少于既有基线——解析或真源出问题",
        n = sdk_perms.len()
    );
    let missing_in_cli: Vec<&String> = sdk_perms.difference(&json_perms).collect();
    let extra_in_cli: Vec<&String> = json_perms.difference(&sdk_perms).collect();
    assert_eq!(
        (missing_in_cli, extra_in_cli),
        (vec![], vec![]),
        "权限词汇两副本漂移（wasm-core 真源 / CLI 生成物）"
    );

    // API 映射同样逐字比对：CLI manifest-gen 按这张表派生权限门。
    // 比对前先按生成器的口径补齐「无 API 方法的权限 → 空数组」——生成器就是把
    // 全部词汇都写进映射表，右值只列有方法的权限。
    let mut sdk_api = sdk_api_map();
    for perm in VALID_PERMISSIONS {
        sdk_api.entry((*perm).to_string()).or_default();
    }
    assert_eq!(json_api, sdk_api, "CLI 生成物的 PERMISSION_API_MAP 与真源不一致");
}

/// 漂移锁②：生成物自称生成物，且打包 CLI 确实消费它（不再手抄清单）
#[test]
fn generated_copies_are_marked_and_consumed_not_copied() {
    let sdk = desktop_root().join("packages/plugin-sdk-desktop");
    let json = read(&sdk.join("bin/permission-vocabulary.json"));
    assert!(json.contains("生成物，勿手改"), "CLI 生成物缺少生成物标注");

    let validate = read(&sdk.join("bin/manifest-validate.js"));
    assert!(
        validate.contains("permission-vocabulary.json"),
        "CLI manifest-validate 必须读词汇生成物，而不是自带权限清单"
    );
}

// ==================== 每条词汇都有门禁落点 ====================

/// 漂移锁③：词汇表里没有「无人执行」的死权限位
///
/// 判定：要么 host 侧源码引用了该权限常量/字面量（`check_permission` /
/// `permission().check`），要么它在前端 API 映射里有方法可门（纯前端贡献面）。
/// 两者皆无 = 声明了也不会被任何一处执行。
#[test]
fn every_permission_has_an_enforcement_point() {
    // 扫描面 = 能力仓 wasm-core 源码（机制层门禁落点）+ 桌面宿主源码（端侧域门禁
    // 落点）+ 移动宿主源码（移动专属位门禁落点；批 2 收口 issue #36：`notify` 位
    // 上提双端真源后其唯一落点在移动宿主 `plugin/notify.rs`，扫描面随之扩入）。
    let root = repo_root();
    let mut files = Vec::new();
    collect_rs(&repo_paths::lib_root().join("wasmnest-wasm-core/src"), &mut files);
    collect_rs(&root.join("wasmnest-desktop/src-tauri/src"), &mut files);
    collect_rs(&root.join("wasmnest-mobile/src-tauri/src"), &mut files);
    assert!(files.len() > 20, "宿主插件源码文件数异常，扫描未生效");

    let mut referenced: BTreeSet<String> = BTreeSet::new();
    for file in &files {
        // 真源文件按名字引用权限串，不能算作门禁落点
        if file.file_name().map(|n| n == "permission.rs").unwrap_or(false) {
            continue;
        }
        // 剥掉纯注释行（R-12）：注释/文档里出现权限词不算门禁落点——
        // 变异判据：把某权限的 check_permission 落点改成注释即转红
        let text = strip_comment_lines(&read(file));
        for (ident, perm) in PERMISSION_VOCABULARY {
            if text.contains(ident) || text.contains(&format!("\"{perm}\"")) {
                referenced.insert((*perm).to_string());
            }
        }
    }

    let api_gated: BTreeSet<String> = PERMISSION_API_MAP
        .iter()
        .filter(|(_, apis)| !apis.is_empty())
        .map(|(perm, _)| (*perm).to_string())
        .collect();

    for perm in VALID_PERMISSIONS {
        assert!(
            referenced.contains(*perm) || api_gated.contains(*perm),
            "权限 {perm} 没有任何门禁落点（host 源码未引用、前端 API 映射也无对应方法）",
        );
    }
}

// ==================== 生产 manifest 无死词汇 ====================

/// 漂移锁④：桌面生产插件与测试 fixture 的 manifest 只声明合法权限
///
/// SDK 授权时按词汇表过滤，未列入的声明等于没写。
///
/// **扫描范围**（三仓拆分 §5-E7）：两处根目录——
///  ① 应用仓桌面端生产插件：`<appsRoot>/apps/<slug>/desktop`（移动端 manifest 有
///     合法词汇分叉（ADR 0018），不在本锁范围）；
///  ② 平台仓 SDK 夹具：`wasmnest-desktop/packages/`。
/// 目录下**全部** `*.json`（递归，R-13——旧实现只读直接子级，嵌套目录里的
/// manifest 声明词汇表外权限可逃逸），靠「有 `id` 字段」筛出 plugin manifest。
#[test]
fn production_manifests_declare_only_known_vocabulary() {
    let mut checked = 0usize;

    // ① 应用仓桌面端：apps/<slug>/desktop（缺失即 panic，不静默跳过）
    let apps_dir = app_paths::apps_root().join("apps");
    let mut app_roots: Vec<PathBuf> = Vec::new();
    for entry in fs::read_dir(&apps_dir).expect("应用仓 apps/ 可读") {
        let desktop = entry.expect("目录条目可读").path().join("desktop");
        if desktop.is_dir() {
            app_roots.push(desktop);
        }
    }
    assert!(
        app_roots.len() >= 4,
        "应用仓桌面端工程数异常（{} < 4），扫描范围可疑",
        app_roots.len()
    );

    // ② 平台仓 SDK 夹具（仍是「夹具根下逐个 crate 目录」的形状）
    let fixture_root = desktop_root().join("packages");
    for entry in fs::read_dir(&fixture_root).expect("夹具目录可读") {
        let entry_path = entry.expect("目录条目可读").path();
        if entry_path.is_dir() {
            app_roots.push(entry_path);
        }
    }

    // 逐个根目录递归扫（collect_json 内部跳过 node_modules / target / template——
    // 所以必须把每个根目录整体交给它，而不是先枚举它的子项）
    for base in app_roots {
        let mut candidates = Vec::new();
        collect_json(&base, &mut candidates);
        candidates.sort();
        for manifest in candidates {
            let text = read(&manifest);
            let value: serde_json::Value = serde_json::from_str(&text)
                .unwrap_or_else(|e| panic!("{} 不是合法 JSON: {e}", manifest.display()));
            // 非 plugin manifest（package.json / tsconfig.json）——按 id 筛除
            if value.get("id").is_none() {
                continue;
            }
            let permissions = value["permissions"]
                .as_array()
                .unwrap_or_else(|| panic!("{} 缺 permissions 数组", manifest.display()));
            for perm in permissions {
                let perm = perm.as_str().expect("权限必须是字符串");
                assert!(
                    VALID_PERMISSIONS.contains(&perm),
                    "{} 声明了词汇表外的权限 {perm}（宿主授权时会被静默过滤）",
                    manifest.display()
                );
            }
            checked += 1;
        }
    }
    // 下限 10 = 4 个桌面 wasm 应用（应用仓 apps/<slug>/desktop）+ bench-test +
    // SDK 夹具合集 5 份（http/plugin/pty/task/ws）。递归扫描只可能让实测数更多；
    // 若变小先查扫描范围。（原为 11：含 wasi-test 夹具，该夹具已随三仓拆分批次三
    // 删除 `plugin-wasi-test` 而消失，基线同步下调。）
    assert!(
        checked >= 10,
        "实测仅校验了 {checked} 份 manifest（预期 >= 10），扫描范围可疑"
    );
}

// ==================== 校验锁确实挂在链上 ====================

/// 漂移锁⑤：`wasmnest-plugin validate` 挂在插件构建链上
///
/// 校验规则写在 CLI 里却没人跑 = 给后人「有锁」的错觉。
#[test]
fn manifest_validation_runs_in_the_plugin_build_chain() {
    let sdk = desktop_root();
    // 剥注释再断言调用（R-12）：`validateManifest(` 若只出现在注释/字符串里
    // 不算挂在链上——注释里的调用点不能当作强制执行证据
    let build = strip_comment_lines(&read(&sdk.join("scripts/plugin-build.js")));
    assert!(
        build.contains("validateManifest("),
        "插件构建链必须调用 manifest 校验（scripts/plugin-build.js）"
    );
    let vocabulary = read(&sdk.join("packages/plugin-sdk-desktop/bin/permission-vocabulary.json"));
    let validate = read(&sdk.join("packages/plugin-sdk-desktop/bin/manifest-validate.js"));
    assert!(
        validate.contains("permission-vocabulary.json"),
        "CLI 校验必须读权限词汇生成物，而不是手抄清单"
    );
    assert!(vocabulary.contains("\"permissions\""));
}
