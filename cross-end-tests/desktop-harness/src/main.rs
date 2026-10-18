//! 两进程跨端 rig 的桌面进程入口
//!
//! 启动序列：装配守卫 → 无头 AppContext + 激活认证中心 → 自选端口起真服务器 →
//! stdout 首行 `READY <port>` → stdin/stdout JSON-RPC 控制面循环。
//!
//! 协议（spec §2.3）：一行一个 JSON；日志只走 stderr；`shutdown` 响应后优雅停
//! 服务器 + 清理临时目录 + 退出；stdin EOF（测试进程被强杀）也退出。
//!
//! 控制面方法集见 [`desktop_harness::rpc_dispatch`]。

use std::io::Write as _;

use serde_json::{json, Value};
use tokio::io::AsyncBufReadExt;

#[tokio::main]
async fn main() {
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(tracing::Level::DEBUG)
        .try_init();

    // 装配守卫：本进程只含桌面侧宿主模块（fail-visible，违反即不起）
    desktop_harness::guard_side();

    desktop_harness::init_app_context(true).await;
    let port = desktop_harness::pick_free_port();
    let (handle, task) = desktop_harness::start_server(port).await;

    println!("READY {port}");
    let _ = std::io::stdout().flush();

    // ==================== stdin/stdout JSON-RPC 控制面 ====================
    let stdin = tokio::io::stdin();
    let mut lines = tokio::io::BufReader::new(stdin).lines();
    loop {
        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            // EOF：测试进程被杀/正常退出，随进程退出（服务器由 OS 回收）
            Ok(None) => break,
            Err(e) => {
                tracing::error!(error = %e, "rpc stdin read failed");
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(error = %e, line = %line, "malformed rpc line");
                continue;
            }
        };
        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let method = req.get("method").and_then(Value::as_str).unwrap_or("");
        let params = req.get("params").cloned().unwrap_or(Value::Null);

        let resp = if method == "shutdown" {
            json!({ "jsonrpc": "2.0", "id": id, "result": Value::Null })
        } else {
            match desktop_harness::rpc_dispatch(method, params).await {
                Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                Err(message) => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32000, "message": message } }),
            }
        };
        println!("{}", serde_json::to_string(&resp).expect("serialize rpc response"));
        let _ = std::io::stdout().flush();

        if method == "shutdown" {
            break;
        }
    }

    // 收尾：优雅停服务器 + 清理临时目录
    desktop_harness::stop_server(handle, task).await;
    desktop_harness::cleanup_temp_dirs();
}
