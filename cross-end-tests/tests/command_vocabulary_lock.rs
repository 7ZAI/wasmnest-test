//! 跨端生命周期命令面词汇锁（2026-10-17 desktop-host-lifecycle-sync · Seam 2）
//!
//! 桌面补齐移动端同款缺失臂后，「命令面词汇 / 语义双端一致」是跨端契约的一部分：
//! 双端壳动作（启停 / 偏好 / 运行态查询）调用**同名命令臂**，任何一端改名而另一端
//! 未跟，双端前端契约即断裂（`plugin_set_enabled` 改成 `plugin_setEnabled` 之类，
//! 壳的 toggle 在另一端就变 "command not found"）。
//!
//! 本锁只钉**命名一致性**（字面扫描两端生产源码），语义一致性由两端各自编排层
//! 测试覆盖（桌面 `lifecycle_orchestration.rs` / 移动 `manager.rs` 契约测试）；
//! 跨端行为链路（停用 → WS 回收 → 移动显性回落）由 `lifecycle_flow.rs` 承接。

use std::path::PathBuf;

// 跨仓路径解析（平台仓兄弟目录 / WASMNEST_PLATFORM_ROOT）
#[path = "common/repo_paths.rs"]
mod repo_paths;

/// 双端必须同名的生命周期命令臂（桌面 D3 补齐后与移动 commands.rs 同面）
const SHARED_LIFECYCLE_COMMANDS: [&str; 7] = [
    "plugin_is_enabled",
    "plugin_set_enabled",
    "plugin_is_autostart",
    "plugin_set_autostart",
    "plugin_preauthorize",
    "plugin_activate",
    "plugin_deactivate",
];

/// 桌面独有补充臂（D3：移动端运行态经 `MobilePluginInfo.state` 由 list/get_info
/// 下发，无独立命令臂；桌面壳状态 chip 用 `plugin_is_running` 单点查询）
const DESKTOP_ONLY_COMMANDS: [&str; 2] = ["plugin_is_running", "plugin_is_activated"];

/// 平台仓根（跨端测试仓的兄弟目录 `<WASMNEST>/wasmnest`）
fn repo_root() -> PathBuf {
    repo_paths::platform_root()
}

fn desktop_lifecycle_src() -> String {
    let p = repo_root().join("wasmnest-desktop/src-tauri/src/plugin/lifecycle.rs");
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("读桌面 lifecycle.rs 失败: {e}"))
}

/// 桌面命令面 = 编排层（lifecycle.rs，六臂） + wasm-core api_bridge（preauth/activate/deactivate 等既有臂）
fn desktop_srcs() -> String {
    let mut s = desktop_lifecycle_src();
    let bridge = repo_root().join("packages/wasmnest-wasm-core/src/manager/host/api_bridge.rs");
    s.push_str("\n");
    s.push_str(&std::fs::read_to_string(&bridge).unwrap_or_else(|e| panic!("读 api_bridge.rs 失败: {e}")));
    s
}

fn mobile_commands_src() -> String {
    let p = repo_root().join("wasmnest-mobile/src-tauri/src/plugin/commands.rs");
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("读移动 commands.rs 失败: {e}"))
}

#[test]
fn lifecycle_command_names_exist_in_both_ends() {
    let desktop = desktop_srcs();
    let mobile = mobile_commands_src();
    for name in SHARED_LIFECYCLE_COMMANDS {
        let in_desktop = desktop.contains(&format!("pub async fn {name}("))
            || desktop.contains(&format!("fn {name}("));
        let in_mobile = mobile.contains(&format!("pub async fn {name}("))
            || mobile.contains(&format!("fn {name}("));
        assert!(
            in_desktop && in_mobile,
            "命令臂 {name} 必须双端同名声明：桌面={in_desktop} 移动={in_mobile}——\
             任一端改名/退役都必须同步另一端（双端壳动作走同名 invoke）"
        );
    }
    // 桌面独有臂：只要求桌面声明（移动端运行态经 info payload 下发，无命令臂）
    for name in DESKTOP_ONLY_COMMANDS {
        assert!(
            desktop.contains(&format!("pub async fn {name}(")),
            "桌面独有命令臂 {name} 缺失声明"
        );
    }
}

#[test]
fn desktop_registers_all_lifecycle_commands() {
    // 桌面侧还要钉「声明即注册」（lib.rs generate_handler 漏注册 = 壳 invoke
    // "command not found"，与移动端 plugin_approve 漏注册教训同形态）
    let lib = std::fs::read_to_string(repo_root().join("wasmnest-desktop/src-tauri/src/lib.rs"))
        .unwrap_or_else(|e| panic!("读桌面 lib.rs 失败: {e}"));
    let start = lib.find("generate_handler![").expect("桌面 lib.rs 必须含 generate_handler");
    let block = &lib[start..];
    let mut in_block = true;
    let mut registered = String::new();
    for raw_line in block.lines() {
        let line = raw_line.split("//").next().unwrap_or("").trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.contains(']') {
            in_block = false;
            break;
        }
        registered.push_str(line);
        registered.push('\n');
    }
    assert!(
        !in_block,
        "generate_handler! 块未找到闭合 `]`——扫描写法失配（注册表有 cfg 注释行时需同步本锁）"
    );
    for name in SHARED_LIFECYCLE_COMMANDS.into_iter().chain(DESKTOP_ONLY_COMMANDS) {
        assert!(
            registered.contains(name),
            "命令臂 {name} 声明了但未在 lib.rs generate_handler! 注册（前端 invoke 会 command not found）"
        );
    }
}

#[test]
fn the_vocabulary_lock_itself_is_non_empty() {
    // 防判据自身失效：扫描写法坏掉会空跑成全绿
    let desktop = desktop_srcs();
    let mobile = mobile_commands_src();
    let found = SHARED_LIFECYCLE_COMMANDS
        .iter()
        .filter(|name| desktop.contains(&format!("pub async fn {name}(")))
        .count();
    assert!(found >= 7, "桌面声明解析失败（只数到 {found}）");
    let found_mobile = SHARED_LIFECYCLE_COMMANDS
        .iter()
        .filter(|name| mobile.contains(&format!("pub async fn {name}(")))
        .count();
    assert!(found_mobile >= 7, "移动声明解析失败（只数到 {found_mobile}）");
    let desktop_only = DESKTOP_ONLY_COMMANDS
        .iter()
        .filter(|name| desktop.contains(&format!("pub async fn {name}(")))
        .count();
    assert!(desktop_only >= 2, "桌面独有臂解析失败（只数到 {desktop_only}）");
}
