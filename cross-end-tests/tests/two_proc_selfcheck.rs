//! 两进程 rig 台子自检（场景红了先排除台子坏了；单进程 rig 的 harness_selfcheck 同款职责）
//!
//! 本文件只在本包 `two-proc` 构建模式编译运行（`cargo test -p cross-end-tests
//! --no-default-features --features two-proc ...`）：
//! ① 装配守卫：本测试二进制的 `inventory` 收集必须只含移动宿主模块、不得含桌面独有
//!    模块——`two-proc` 模式的「测试进程只链移动侧」不变量（桌面 lib 一旦被误放进
//!    依赖图/被混用 feature 开启，移动真插件又会跑在桌面宿主实现上——单进程 rig 的病）。
//! ② 驱动自检：spawn harness → READY → 一次轻量 rpc 往返 → 优雅关停。
//!
//! harness 二进制由 default 模式预先构建（`cargo build -p wasmnest-crossend-desktop-harness`，
//! 见 `desktop::harness_bin_path` 的显性失败说明）。

mod two_proc_common;

use two_proc_common::desktop::DesktopHarness;

/// 装配守卫——移动独有必须在场、桌面独有必须缺席
///
/// 名字清单与 harness 的 `desktop_harness::guard_side` 同表（各取双方独有名字做判据；
/// server-crate 绑定模块（http / websocket / discovery / peer-net）在本模式下
/// `common-host` 未开 ⇒ 不提交，判据里不需要它们）。
#[test]
fn test_process_inventory_is_mobile_only() {
    let names: Vec<String> = wasmnest_host_kit::ModuleRegistry::collected()
        .descs()
        .iter()
        .map(|d| d.name.to_string())
        .collect();
    let joined = names.join(", ");
    for must in [
        "mobile-host-core",
        "mobile-terminal-stream",
        "mobile-fs",
        "mobile-auth",
        "mobile-connection",
        "mobile-notify",
    ] {
        assert!(
            names.iter().any(|n| n == must),
            "测试进程缺少移动宿主模块 {must}（inventory 收集异常）：{joined}"
        );
    }
    for forbid in [
        "fs-desktop",
        "process",
        "task",
        "platform-desktop",
        "api_call",
        "crypto",
        "app",
        "timer",
    ] {
        assert!(
            !names.iter().any(|n| n == forbid),
            "测试进程混入桌面宿主模块 {forbid}（桌面 lib 在 two-proc 模式被链入？feature 混用/依赖图被破坏）：{joined}"
        );
    }
}

/// 驱动自检：spawn → READY → 轻量 rpc 往返 → 优雅关停
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn harness_spawn_ready_rpc_shutdown() {
    if tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::DEBUG)
        .try_init()
        .is_err()
    {
        tracing::debug!("tracing subscriber already initialized");
    }

    let mut desktop = DesktopHarness::spawn_and_ready().await;
    assert!(desktop.port > 0, "READY 必须报告端口");

    // 轻量 rpc 往返：链路加密计数器（本场景未开加密 ⇒ 恒零；顺带验证 rpc 通道）
    let counters = desktop.link_crypto_counters().await;
    assert_eq!(counters, (0, 0, 0), "刚启动不应有加密计数");

    desktop.shutdown().await;
}