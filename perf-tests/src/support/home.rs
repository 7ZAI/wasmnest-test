//! 进程级 HOME 沙箱。
//!
//! 真实应用经 `host-config.home_dir` 取数据根（宿主实现 = `dirs::home_dir()`），
//! 无法按插件注入。性能测量又必须把应用数据写到临时目录：这里在进程内把
//! `HOME` 指到沙箱根，退出时恢复。
//!
//! 纪律：
//! - 原始 HOME 在任何改写之前捕获（[`capture_original`]），子进程（夹具编译）
//!   用它显式恢复，避免「应用模块跑过之后 cargo 找不到用户缓存」；
//! - 只允许一个沙箱（[`HomeSandbox::new`] 断言），嵌套改 HOME 会让恢复顺序失真。

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static ORIGINAL_HOME: OnceLock<Option<OsString>> = OnceLock::new();
static SANDBOX_ACTIVE: OnceLock<()> = OnceLock::new();

/// 在**任何 HOME 改写之前**调用一次，固定原始值。
///
/// 入口在进程启动时调用；迟于沙箱创建调用会捕获到临时 HOME（断言兜底）。
pub fn capture_original() {
    let captured = ORIGINAL_HOME.get_or_init(|| std::env::var_os("HOME"));
    debug_assert!(
        SANDBOX_ACTIVE.get().is_none() || captured.is_some(),
        "original HOME must be captured before the sandbox is installed"
    );
}

/// 原始 HOME（未捕获时惰性捕获；仅用于子进程环境恢复）。
pub fn original_home() -> Option<OsString> {
    ORIGINAL_HOME.get_or_init(|| std::env::var_os("HOME")).clone()
}

/// 临时 HOME：创建后进程内 `dirs::home_dir()` 指向它，Drop 时恢复并清理。
pub struct HomeSandbox {
    root: PathBuf,
    original: Option<OsString>,
}

impl HomeSandbox {
    pub fn new() -> anyhow::Result<Self> {
        anyhow::ensure!(
            SANDBOX_ACTIVE.set(()).is_ok(),
            "HOME 沙箱已存在：同一进程只允许一个（嵌套会破坏恢复顺序）"
        );
        let original = original_home();
        let root = std::env::temp_dir().join(format!(
            "wasmnest_perf_home_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&root)?;
        std::env::set_var("HOME", &root);
        Ok(Self { root, original })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl Drop for HomeSandbox {
    fn drop(&mut self) {
        match &self.original {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
