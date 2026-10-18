//! 跨仓路径解析（跨端测试仓 `wasmnest-test` 的集中 resolver）
//!
//! ## 为什么必须有
//!
//! 本测试工程原为平台仓一级目录 `cross-end-tests/`，路径基准直接是「manifest 目录的
//! 上一级」（= 平台仓根）。2026-10-18 迁出为**测试容器仓** `wasmnest-test/cross-end-tests/`
//! （容器仓与平台仓 / 能力仓 / 应用仓平级；容器下可并列多个测试工程）后，平台仓不再是
//! 本工程的上级目录，`env!("CARGO_MANIFEST_DIR")` 的相邻层级已非平台仓——继续沿用旧
//! 基准会指向不存在的路径，锁读不到文件退化成「扫不到即无命中」的假绿灯，是拆仓最危险
//! 的失效形态。
//!
//! ## 基准与规则（fail-visible）
//!
//! - `platform_root()`：平台仓根（`<WASMNEST>/wasmnest`），内含双端宿主与能力仓
//!   submodule 挂载点 `packages/`。
//!   1. `WASMNEST_PLATFORM_ROOT` 显式指定（CI / 非常规布局）；
//!   2. 未设时自 `CARGO_MANIFEST_DIR` 向上逐级找**兄弟目录** `wasmnest`
//!      （R1 布局：四仓同放 `${WASMNEST}` 下，本工程在其一子目录中亦可命中）；
//!   3. 都找不到即 panic，绝不返回可能无效的路径。
//! - `lib_root()`：能力仓根 = 平台仓 `packages/` submodule 挂载点。**必须经此读取**
//!   （而非兄弟目录 `wasmnest-lib/`）：平台仓双端宿主的 Cargo path 依赖写死
//!   `../../packages/<crate>`，本仓只有指到同一物理路径，依赖图里能力 crate 才
//!   只有一份、类型同一性才成立。
//!
//! 应用仓（`wasmnest-apps`）的解析见 `tests/common/app_paths.rs`（`WASMNEST_APPS_ROOT` +
//! 兄弟目录发现，自平台仓迁出前同款）。
#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// 平台仓根（`<WASMNEST>/wasmnest`）。解析失败即 panic。
pub fn platform_root() -> PathBuf {
    if let Ok(value) = std::env::var("WASMNEST_PLATFORM_ROOT") {
        let path = PathBuf::from(&value);
        assert!(
            path.is_dir(),
            "WASMNEST_PLATFORM_ROOT 指向的目录不存在: {}（当前工作区未按 R1 布局装配？）",
            path.display()
        );
        return path;
    }
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    for dir in manifest_dir.ancestors() {
        let candidate = dir.join("wasmnest");
        // 兄弟目录名 `wasmnest` 也是父目录自身的末段名（`${WASMNEST}` 的路径末段
        // 是 `WasmNest`，大小写不同，不会自指），故只需判目录存在。
        if candidate.is_dir() {
            return candidate;
        }
    }
    panic!(
        "平台仓（wasmnest）未找到：已自 {} 向上逐级查找兄弟目录。\
         请按 R1 布局把平台仓放到跨端测试仓的父目录下，\
         或设置 WASMNEST_PLATFORM_ROOT 指向它。",
        manifest_dir.display()
    );
}

/// 能力仓根：平台仓 `packages/` submodule 挂载点。
pub fn lib_root() -> PathBuf {
    platform_root().join("packages")
}

/// 某能力 crate 根：`<libRoot>/<crate_name>`（不存在即 panic，防空转）。
pub fn lib_crate_root(crate_name: &str) -> PathBuf {
    let dir = lib_root().join(crate_name);
    assert!(
        dir.is_dir(),
        "能力 crate 目录不存在: {}（平台仓 packages/ submodule 未初始化？\
         先 `git submodule update --init`）",
        dir.display()
    );
    dir
}

/// 桌面端工程根：`<platformRoot>/wasmnest-desktop`。
pub fn desktop_root() -> PathBuf {
    platform_root().join("wasmnest-desktop")
}

/// 移动端工程根：`<platformRoot>/wasmnest-mobile`。
pub fn mobile_root() -> PathBuf {
    platform_root().join("wasmnest-mobile")
}
