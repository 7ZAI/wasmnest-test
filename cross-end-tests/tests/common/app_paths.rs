//! 平台侧测试读取应用仓（wasmnest-apps）源码的集中 resolver —— 三仓拆分 spec §5-E7。
//!
//! **为什么必须有**：app 源码自三仓拆分起住独立应用仓，平台侧多条治理锁仍要读它们
//! （身份锁 / 退役面锁 / 空目录锁 / 前端一致性锁）。路径解析散落硬编码会随布局漂移
//! 静默失效——锁读不到文件退化成「扫不到即无命中」的假绿灯，是拆仓最危险的失效形态。
//!
//! 规则（fail-visible）：
//! 1. `WASMNEST_APPS_ROOT` 显式指定（CI / 非常规布局）；
//! 2. 未设时自 `CARGO_MANIFEST_DIR` 向上逐级找**兄弟目录** `wasmnest-apps`
//!    （R1 钉死布局：四仓同放 `${WASMNEST}` 下）；
//! 3. 都找不到即 panic，绝不返回可能无效的路径。
//!
//! 应用仓布局：`<appsRoot>/apps/<slug>/{desktop,mobile}`。
#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// 应用仓根。
pub fn apps_root() -> PathBuf {
    if let Ok(value) = std::env::var("WASMNEST_APPS_ROOT") {
        let path = PathBuf::from(&value);
        assert!(
            path.is_dir(),
            "WASMNEST_APPS_ROOT 指向的目录不存在: {}（当前工作区未按 R1 布局装配？）",
            path.display()
        );
        return path;
    }
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    for dir in manifest_dir.ancestors() {
        let candidate = dir.join("wasmnest-apps");
        if candidate.is_dir() {
            return candidate;
        }
    }
    panic!(
        "应用仓（wasmnest-apps）未找到：已自 {} 向上逐级查找。\
         请按 R1 布局把 wasmnest-apps 放到三仓父目录 WasmNest/ 下，\
         或设置 WASMNEST_APPS_ROOT 指向它。",
        manifest_dir.display()
    )
}

/// 单个应用工程目录：`<appsRoot>/apps/<slug>/<end>`；不存在即 panic。
pub fn app_dir(slug: &str, end: &str) -> PathBuf {
    let dir = apps_root().join("apps").join(slug).join(end);
    assert!(
        dir.is_dir(),
        "应用目录不存在: {}（检查 slug/end 拼写与 apps 仓内容）",
        dir.display()
    );
    dir
}
