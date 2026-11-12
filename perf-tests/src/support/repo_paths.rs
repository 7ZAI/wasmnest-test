//! 跨仓路径解析（R1 布局：四仓同放 `${WASMNEST}` 下）
//!
//! 与 `cross-end-tests/tests/common/repo_paths.rs` 同一纪律：找不到即 panic，
//! 绝不返回可能无效的路径——基准读不到产物时退化成空测比直接失败更危险。

use std::path::{Path, PathBuf};

const PERF_MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");

fn discover(env_key: &str, sibling: &str, what: &str) -> PathBuf {
    if let Ok(value) = std::env::var(env_key) {
        let path = PathBuf::from(&value);
        assert!(
            path.is_dir(),
            "{env_key} 指向的目录不存在: {}（{what}）",
            path.display()
        );
        return path;
    }
    let manifest_dir = Path::new(PERF_MANIFEST_DIR);
    for dir in manifest_dir.ancestors() {
        let candidate = dir.join(sibling);
        if candidate.is_dir() {
            return candidate;
        }
    }
    panic!(
        "{what}（{sibling}）未找到：已自 {} 向上逐级查找。\
         请按 R1 布局把四仓放在同一父目录下，或设置 {env_key} 指向它。",
        manifest_dir.display()
    );
}

/// 平台仓根（`<WASMNEST>/wasmnest`）。
pub fn platform_root() -> PathBuf {
    discover("WASMNEST_PLATFORM_ROOT", "wasmnest", "平台仓")
}

/// 能力仓根（`<WASMNEST>/wasmnest-lib`）。
pub fn lib_root() -> PathBuf {
    discover("WASMNEST_LIB_ROOT", "wasmnest-lib", "能力仓")
}

/// 应用仓根（`<WASMNEST>/wasmnest-apps`）。
pub fn apps_root() -> PathBuf {
    discover("WASMNEST_APPS_ROOT", "wasmnest-apps", "应用仓")
}

/// 测试容器仓根（本工程与 `cross-end-tests` 的共同上级）。
pub fn test_root() -> PathBuf {
    Path::new(PERF_MANIFEST_DIR)
        .parent()
        .expect("perf-tests 必有上级目录")
        .to_path_buf()
}

/// 桌面端工程根（`<platform>/wasmnest-desktop`）。
pub fn desktop_root() -> PathBuf {
    platform_root().join("wasmnest-desktop")
}

/// 移动端工程根（`<platform>/wasmnest-mobile`）。
pub fn mobile_root() -> PathBuf {
    platform_root().join("wasmnest-mobile")
}

/// 本工程 target 根（与 `.cargo/config.toml` 的 `target-dir` 必须一致）。
pub fn perf_target_dir() -> PathBuf {
    test_root().join("target/perf")
}

/// 桌面随包应用产物目录（`resources/plugins/desktop`）。
pub fn desktop_plugins_dir() -> PathBuf {
    desktop_root().join("src-tauri/resources/plugins/desktop")
}

/// 移动随包应用产物目录（`resources/plugins/mobile`）。
pub fn mobile_plugins_dir() -> PathBuf {
    mobile_root().join("src-tauri/resources/plugins/mobile")
}
