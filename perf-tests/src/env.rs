//! 运行环境快照：机器、工具链、四仓 HEAD 与脏标记。
//!
//! 性能数字脱离环境没有意义。每次报告头部与 JSON 都带这份快照，跨机器
//! 对比时先看环境是否同一档；门禁只做数量级，也是因为这里无法保证同机。

use std::path::Path;
use std::process::Command;

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct RepoState {
    pub name: String,
    pub root: String,
    pub head: Option<String>,
    pub dirty: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Environment {
    pub os: String,
    pub kernel: String,
    pub arch: String,
    pub cpu: String,
    pub cores: usize,
    pub total_memory_bytes: u64,
    pub rustc: String,
    pub wasmtime: String,
    pub profile: String,
    pub repos: Vec<RepoState>,
}

impl Environment {
    pub fn capture() -> Self {
        use sysinfo::System;

        let mut sys = System::new_all();
        sys.refresh_all();
        let cpu = sys
            .cpus()
            .first()
            .map(|c| c.brand().trim().to_string())
            .unwrap_or_else(|| "unknown".to_string());

        let repos = vec![
            repo_state("wasmnest", &crate::support::repo_paths::platform_root()),
            repo_state("wasmnest-lib", &crate::support::repo_paths::lib_root()),
            repo_state("wasmnest-apps", &crate::support::repo_paths::apps_root()),
            repo_state("wasmnest-test", &crate::support::repo_paths::test_root()),
        ];

        Self {
            os: System::long_os_version().unwrap_or_else(|| "unknown".to_string()),
            kernel: System::kernel_version().unwrap_or_else(|| "unknown".to_string()),
            arch: std::env::consts::ARCH.to_string(),
            cpu,
            cores: sys.cpus().len(),
            total_memory_bytes: sys.total_memory(),
            rustc: command_version("rustc", &["--version"]),
            // wasmtime 48 不再导出 VERSION 常量；锁定文件是该进程实际链接的版本
            wasmtime: lockfile_version("wasmtime").unwrap_or_else(|| "unknown".to_string()),
            profile: if cfg!(debug_assertions) { "debug" } else { "release" }.to_string(),
            repos,
        }
    }

    /// 报告头部/表格用的一行摘要。
    pub fn describe(&self) -> String {
        let repos = self
            .repos
            .iter()
            .map(|r| match &r.head {
                Some(head) if r.dirty => format!("{}@{head}+dirty", r.name),
                Some(head) => format!("{}@{head}", r.name),
                None => format!("{}=unknown", r.name),
            })
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "{} {} | {} x{} | RAM {:.1} GiB | rustc {} | wasmtime {} | {} | {}",
            self.os,
            self.kernel,
            self.cpu,
            self.cores,
            self.total_memory_bytes as f64 / 1024.0 / 1024.0 / 1024.0,
            self.rustc,
            self.wasmtime,
            self.profile,
            repos,
        )
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }
}

fn repo_state(name: &str, root: &Path) -> RepoState {
    RepoState {
        name: name.to_string(),
        root: root.display().to_string(),
        head: git(root, &["rev-parse", "--short", "HEAD"]),
        dirty: git(root, &["status", "--porcelain"]).is_some_and(|s| !s.is_empty()),
    }
}

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn command_version(cmd: &str, args: &[&str]) -> String {
    Command::new(cmd)
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// 从本 workspace 的 `Cargo.lock` 读某个 crate 的锁定版本。
fn lockfile_version(crate_name: &str) -> Option<String> {
    let lock = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.lock");
    let text = std::fs::read_to_string(lock).ok()?;
    for block in text.split("[[package]]").skip(1) {
        let mut name = None;
        let mut version = None;
        for line in block.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                break;
            }
            if let Some(rest) = line.strip_prefix("name = ") {
                name = Some(rest.trim_matches('"').to_string());
            } else if let Some(rest) = line.strip_prefix("version = ") {
                version = Some(rest.trim_matches('"').to_string());
            }
        }
        if name.as_deref() == Some(crate_name) {
            return version;
        }
    }
    None
}
