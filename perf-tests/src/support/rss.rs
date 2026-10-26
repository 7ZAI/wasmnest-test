//! 进程内存指标。
//!
//! `VmRSS` 是当前驻留集，`VmHWM` 是内核记录的峰值驻留集（Linux）。峰值比
//! 轮询采样可靠——插件激活的瞬时峰值可能落在两次采样之间；非 Linux 平台
//! 退回 sysinfo 的当前值，并显式标注“peak 不可用”。

use std::path::Path;

/// 当前 RSS（字节）。
pub fn current_rss_bytes() -> Option<u64> {
    proc_status_bytes("/proc/self/status", "VmRSS:").or_else(sysinfo_rss)
}

/// 峰值 RSS（字节；Linux 之外返回 None）。
pub fn peak_rss_bytes() -> Option<u64> {
    proc_status_bytes("/proc/self/status", "VmHWM:")
}

fn proc_status_bytes(path: &str, key: &str) -> Option<u64> {
    if !Path::new(path).exists() {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    text.lines()
        .find_map(|line| line.strip_prefix(key))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|kb| kb.parse::<u64>().ok())
        .map(|kb| kb * 1024)
}

fn sysinfo_rss() -> Option<u64> {
    use sysinfo::{Pid, ProcessesToUpdate, System};
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::Some(&[Pid::from_u32(std::process::id())]), true);
    sys.process(Pid::from_u32(std::process::id()))
        .map(|p| p.memory())
}

/// 人读格式（MiB，一位小数）。
pub fn mib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}
