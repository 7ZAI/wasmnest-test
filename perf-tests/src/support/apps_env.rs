//! 真实应用基准环境。
//!
//! 与 bridge 的差别只在**被测物**：这里装载 `wasmnest-desktop` 随包的真实应用
//! 产物（ai-chatbox / agent-hub / terminal-session(BedCode) / file-transfer），
//! 命令走同一条生产命令面（`PluginHost::invoke_rust_command`）。
//!
//! 隔离策略：
//! - 临时 HOME（应用数据根 = `$HOME/.wasmnest/<app>`，见 [`crate::support::home`]）；
//! - 临时内核库 + 插件私有库根（SQLite schema、KV、会话登记都在里面）；
//! - 应用产物复制到临时插件目录后再扫描（平台仓 resources 只读）。
//!
//! 无头语义（与 bridge 一致）：无 AppHandle → 弹窗授权不可用；需要文件目录的
//! 应用用 `fs_granted_paths` 旧记录预授权，第一方目录清单按生产装配注入。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;
use wasmnest_desktop_lib::wasm_core::storage::PluginStorage;
use wasmnest_desktop_lib::wasm_core::PluginHost;
use wasmnest_wasm_core::security::fs_auth::TrustedDir;

use crate::support::home::HomeSandbox;
use crate::support::repo_paths;

pub const AI_CHATBOX: &str = "com.wasmnest.ai-chatbox";
pub const AGENT_HUB: &str = "com.wasmnest.agent-hub";
pub const BEDCODE: &str = "com.wasmnest.terminal-session";
pub const FILE_TRANSFER: &str = "com.wasmnest.file-transfer";

/// 被测应用（id = 产物目录名 = manifest id）；顺序即激活顺序。
pub const APPS: [(&str, &str); 4] = [
    (AI_CHATBOX, "AI Chatbox"),
    (AGENT_HUB, "Agent Hub"),
    (BEDCODE, "BedCode"),
    (FILE_TRANSFER, "File Transfer"),
];

/// ai-chatbox 预置会话 id（场景共用，避免每轮新建导致索引无限增长）
pub const CHAT_CONVERSATION_ID: &str = "perf-conversation";
/// agent-hub 预置 provider 名
pub const AGENT_HUB_PRESET: &str = "perf-preset";
/// agent-hub 预置 skill 目录名
pub const AGENT_HUB_SKILL_DIR: &str = "perf-skill";

/// 单应用激活结果
#[derive(Debug, Clone)]
pub struct ActivationRecord {
    pub id: &'static str,
    pub label: &'static str,
    pub millis: f64,
    pub ok: bool,
    pub error: Option<String>,
}

pub struct AppsEnv {
    host: Arc<PluginHost>,
    /// 激活成功、可接受命令的应用 id
    active: Vec<&'static str>,
    pub activation: Vec<ActivationRecord>,
    /// 沙箱安装前的 RSS（内存增量基准）
    pub rss_before_bytes: u64,
    /// 工作目录（BedCode 配置、文件传输快照的形态来源）
    pub workspace: PathBuf,
    temp_root: PathBuf,
    _sandbox: HomeSandbox,
}

impl AppsEnv {
    pub fn is_active(&self, id: &str) -> bool {
        self.active.iter().any(|a| *a == id)
    }

    /// 环境自述（报告头部用）
    pub fn describe(&self) -> String {
        let active: Vec<&str> = self
            .activation
            .iter()
            .filter(|r| r.ok)
            .map(|r| r.label)
            .collect();
        format!(
            "随包应用 [{}]，工作目录 {}，临时根 {}",
            active.join(" / "),
            self.workspace.display(),
            self.temp_root.display()
        )
    }

    pub fn ensure_active(&self, id: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.is_active(id),
            "应用 {id} 未成功激活（见 H1 激活记录）"
        );
        Ok(())
    }

    pub async fn call(
        &self,
        plugin_id: &str,
        command: &str,
        args: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        self.host
            .invoke_rust_command(plugin_id, command, args)
            .await
            .map_err(|e| anyhow::anyhow!("invoke {plugin_id}/{command} 失败: {e}"))
    }

    pub async fn call_timed(
        &self,
        plugin_id: &str,
        command: &str,
        args: serde_json::Value,
    ) -> anyhow::Result<(serde_json::Value, Duration)> {
        let t0 = Instant::now();
        let value = self.call(plugin_id, command, args).await?;
        Ok((value, t0.elapsed()))
    }

    /// 停用全部已激活应用并清理临时目录（HOME 沙箱由 Drop 恢复/清理）
    pub async fn shutdown(self) {
        for id in &self.active {
            let _ = self.host.deactivate_plugin(id, false).await;
        }
        let _ = std::fs::remove_dir_all(&self.temp_root);
    }
}

/// 构建应用环境：HOME 沙箱 → 复制产物 → 装配宿主 → 预置数据/授权 → 逐应用激活。
pub async fn build() -> anyhow::Result<AppsEnv> {
    let rss_before_bytes = crate::support::rss::current_rss_bytes().unwrap_or(0);
    let sandbox = HomeSandbox::new()?;
    let temp_root = sandbox.root().join("perf");
    std::fs::create_dir_all(&temp_root)?;

    let workspace = temp_root.join("workspace");
    std::fs::create_dir_all(&workspace)?;

    // 应用产物先复制：loader 只读扫描，但复制保证任何写都不会落到平台仓
    let plugins_dir = temp_root.join("plugins");
    stage_apps(&plugins_dir)?;

    // 生产同形的第一方免弹窗目录（agent-hub 技能库依赖）
    let first_party: Vec<(&'static str, Vec<TrustedDir>)> =
        wasmnest_desktop_lib::first_party_dirs::first_party_dirs();
    let assembled = crate::support::host::assemble(&temp_root, &plugins_dir, first_party).await?;
    let host = assembled.host;

    seed_fs_grant(&assembled.db, sandbox.root()).await?;
    seed_agent_hub_skill(sandbox.root())?;

    let activation = activate_all(&host).await;

    // 会话数据必须在激活之后写入：ai-chatbox 的数据根在 activate 期才解析并缓存
    if activation
        .iter()
        .any(|r| r.id == AI_CHATBOX && r.ok)
    {
        seed_chat_data(&host).await?;
    } else {
        println!("[apps] ai-chatbox 未激活：跳过会话数据预置（相关场景将失败）");
    }

    Ok(AppsEnv {
        host,
        active: activation
            .iter()
            .filter(|r| r.ok)
            .map(|r| r.id)
            .collect(),
        activation,
        rss_before_bytes,
        workspace,
        temp_root,
        _sandbox: sandbox,
    })
}

/// 激活前预授权：无头上下文没有弹窗通道，按 `fs_granted_paths` 旧记录
/// 预授权 ai-chatbox 的数据目录（与 bridge 的 fs 场景同法）。
async fn seed_fs_grant(
    db: &Arc<std::sync::Mutex<wasmnest_desktop_lib::db::Database>>,
    home: &Path,
) -> anyhow::Result<()> {
    let storage = PluginStorage::new(db.clone());
    let chatbox_dir = home.join(".wasmnest/ai-chatbox");
    std::fs::create_dir_all(&chatbox_dir)?;
    storage
        .set(
            AI_CHATBOX,
            "fs_granted_paths",
            json!([chatbox_dir.to_string_lossy()]),
        )
        .await?;
    Ok(())
}

/// 激活后预置业务数据：ai-chatbox 一个会话 + 一条消息，给后续 save-message
/// 的整文件读改写一个稳定基线。
async fn seed_chat_data(host: &Arc<PluginHost>) -> anyhow::Result<()> {
    host.invoke_rust_command(
        AI_CHATBOX,
        "ai-chatbox.save-conversation",
        json!({
            "conversation": {
                "id": CHAT_CONVERSATION_ID,
                "title": "Perf Conversation",
                "createdAt": "2026-01-01T00:00:00Z",
                "updatedAt": "2026-01-01T00:00:00Z",
                "providerId": "perf",
                "providerName": "Perf",
                "model": "perf-model"
            }
        }),
    )
    .await
    .map_err(|e| anyhow::anyhow!("预置 ai-chatbox 会话失败: {e}"))?;
    host.invoke_rust_command(
        AI_CHATBOX,
        "ai-chatbox.save-message",
        json!({
            "conversationId": CHAT_CONVERSATION_ID,
            "role": "user",
            "content": "perf baseline",
            "timestamp": "2026-01-01T00:00:01Z"
        }),
    )
    .await
    .map_err(|e| anyhow::anyhow!("预置 ai-chatbox 消息失败: {e}"))?;
    Ok(())
}

/// agent-hub `read-skill` 的读取目标：规范库根下的单 skill 目录。
fn seed_agent_hub_skill(home: &Path) -> anyhow::Result<()> {
    let dir = home.join(".agents/skills").join(AGENT_HUB_SKILL_DIR);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        dir.join("SKILL.md"),
        "---\nname: Perf Skill\ndescription: perf harness fixture\n---\n\
         # Perf Skill\n\n固定内容的技能夹具，用于测量 SKILL.md 读取路径。\n",
    )?;
    Ok(())
}

/// 激活全部应用：先做一次「装载 + 激活 + 停用」预热（wasmtime 引擎/组件缓存
/// 初始化只发生一次），再逐个测量。激活失败不中断其余应用。
async fn activate_all(host: &Arc<PluginHost>) -> Vec<ActivationRecord> {
    // 预热：首实例化的引擎成本不计入单应用数字
    let _ = host.activate_plugin(AI_CHATBOX, false).await;
    let _ = host.deactivate_plugin(AI_CHATBOX, false).await;

    let mut records = Vec::with_capacity(APPS.len());
    for (id, label) in APPS {
        let t0 = Instant::now();
        let result = host.activate_plugin(id, false).await;
        let millis = t0.elapsed().as_secs_f64() * 1e3;
        match result {
            Ok(()) => {
                println!("[apps] 已激活 {label}（{millis:.0} ms）");
                records.push(ActivationRecord {
                    id,
                    label,
                    millis,
                    ok: true,
                    error: None,
                });
            }
            Err(e) => {
                let error = format!("{e:#}");
                eprintln!("[apps] 激活 {label} 失败: {error}");
                records.push(ActivationRecord {
                    id,
                    label,
                    millis,
                    ok: false,
                    error: Some(error),
                });
            }
        }
    }
    records
}

/// 把随包应用目录复制到临时插件目录（目录名必须 = manifest id）。
fn stage_apps(plugins_dir: &Path) -> anyhow::Result<()> {
    let src_root = repo_paths::desktop_plugins_dir();
    std::fs::create_dir_all(plugins_dir)?;
    for (id, label) in APPS {
        let src = src_root.join(id);
        anyhow::ensure!(
            src.is_dir(),
            "{label} 产物缺失: {}（先构建桌面端随包应用）",
            src.display()
        );
        copy_dir(&src, &plugins_dir.join(id))?;
    }
    Ok(())
}

fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}
