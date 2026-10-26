//! 真实应用基准（apps 模块）。
//!
//! 被测物是 `wasmnest-desktop` 随包的四个业务应用（ai-chatbox / agent-hub /
//! BedCode / file-transfer），命令全部经生产同形的 `invoke_rust_command` 下发。
//! 与 bridge 夹具的差别：这里没有「可调的合成载荷」，测的是**真实命令链路**
//! （参数解析 → 私有库/文件 IO → 序列化回执），载荷尺寸只按业务形态选两档。
//!
//! 场景组：
//! - H 装载：逐应用冷激活（含组件实例化）；
//! - I AI Chatbox：JSONL 对话读改写 + 索引重写；
//! - J Agent Hub：供应商预设 / 技能读取 / 用量统计；
//! - K BedCode：会话配置、预设任务、任务历史；
//! - L File Transfer：设置、设备快照、任务/历史列表；
//! - R 内存：四应用常驻后的 RSS（最后执行）。
//!
//! 无头语义见 [`crate::support::apps_env`]；任何应用激活失败只影响依赖它的
//! 场景（记录为失败），不吞掉其余测点。

use std::time::Instant;

use serde_json::{json, Value};

use crate::report::{micros, Cmp, Report};
use crate::support::apps_env::{
    AppsEnv, AGENT_HUB, AGENT_HUB_PRESET, AGENT_HUB_SKILL_DIR, AI_CHATBOX, BEDCODE,
    CHAT_CONVERSATION_ID, FILE_TRANSFER,
};

/// 应用激活的数量级门限（含 wasmtime 实例化；四应用中最重的 BedCode 也在秒级）
const ACTIVATION_BUDGET_MS: f64 = 30_000.0;
/// 常驻内存门限：四应用装载后的当前 RSS
const RESIDENT_RSS_BUDGET_MIB: f64 = 4096.0;

struct Scenario {
    id: &'static str,
    group: &'static str,
    title: &'static str,
    smoke: bool,
}

fn all() -> Vec<Scenario> {
    vec![
        Scenario { id: "H1", group: "H", title: "四应用冷激活", smoke: true },
        Scenario { id: "I1", group: "I", title: "save-message 1 KiB", smoke: true },
        Scenario { id: "I2", group: "I", title: "save-message 64 KiB", smoke: false },
        Scenario { id: "I3", group: "I", title: "get-messages（含 64 KiB 行）", smoke: false },
        Scenario { id: "I4", group: "I", title: "list-conversations", smoke: false },
        Scenario { id: "I5", group: "I", title: "save-conversation 更新", smoke: false },
        Scenario { id: "I6", group: "I", title: "delete-conversation", smoke: false },
        Scenario { id: "J1", group: "J", title: "save-preset 更新", smoke: true },
        Scenario { id: "J2", group: "J", title: "get-providers-state", smoke: false },
        Scenario { id: "J3", group: "J", title: "read-skill（SKILL.md）", smoke: false },
        Scenario { id: "J4", group: "J", title: "get-usage-stats", smoke: false },
        Scenario { id: "K1", group: "K", title: "session.config.upsert 更新", smoke: true },
        Scenario { id: "K2", group: "K", title: "session.config.list", smoke: false },
        Scenario { id: "K3", group: "K", title: "preset-create / preset-delete", smoke: false },
        Scenario { id: "K4", group: "K", title: "history-list", smoke: false },
        Scenario { id: "K5", group: "K", title: "history-stats", smoke: false },
        Scenario { id: "K6", group: "K", title: "supported-agents", smoke: false },
        Scenario { id: "L1", group: "L", title: "get-settings", smoke: true },
        Scenario { id: "L2", group: "L", title: "save-device-snapshot 64 条", smoke: false },
        Scenario { id: "L2b", group: "L", title: "save-device-snapshot 512 条", smoke: false },
        Scenario { id: "L3", group: "L", title: "get-device-snapshot 64 条", smoke: false },
        Scenario { id: "L4", group: "L", title: "list-tasks", smoke: false },
        Scenario { id: "L5", group: "L", title: "list-history", smoke: false },
        Scenario { id: "R1", group: "R", title: "常驻 RSS / 峰值", smoke: true },
    ]
}

pub fn list() -> Vec<(&'static str, &'static str, &'static str)> {
    all()
        .into_iter()
        .map(|s| (s.id, s.group, s.title))
        .collect()
}

pub async fn run(
    full: bool,
    iters: usize,
    group_filter: Option<&str>,
    rep: &mut Report,
) -> anyhow::Result<()> {
    let selected: Vec<Scenario> = all()
        .into_iter()
        .filter(|s| match group_filter {
            None => true,
            Some(g) => s.group.eq_ignore_ascii_case(g) || s.id.contains(g),
        })
        .filter(|s| full || s.smoke)
        .collect();

    if selected.is_empty() {
        println!("[apps] 无匹配场景（group={group_filter:?}, full={full}）");
        return Ok(());
    }

    let env = crate::support::apps_env::build().await?;
    println!("[apps] 环境就绪：{}", env.describe());

    macro_rules! scenario {
        ($id:literal, $future:expr) => {
            if selected.iter().any(|s| s.id == $id) {
                let t0 = Instant::now();
                match $future.await {
                    Ok(()) => {}
                    Err(e) => {
                        eprintln!("[apps] 场景 {} 失败: {e:#}", $id);
                        rep.failures.push(format!("{}: {e:#}", $id));
                    }
                }
                println!(
                    "[apps] ── {} 完成（{:.0} ms）",
                    $id,
                    t0.elapsed().as_secs_f64() * 1e3
                );
            }
        };
    }

    // H：激活记录在 env 构建期产生，这里只负责落表
    if selected.iter().any(|s| s.id == "H1") {
        record_activation(&env, rep);
    }

    scenario!("I1", i1_save_message(&env, 1024, iters, rep));
    scenario!("I2", i2_save_message(&env, 65536, iters, rep));
    scenario!("I3", i3_get_messages(&env, iters, rep));
    scenario!("I4", i4_list_conversations(&env, iters, rep));
    scenario!("I5", i5_save_conversation(&env, iters, rep));
    scenario!("I6", i6_delete_conversation(&env, iters, rep));

    scenario!("J1", j1_save_preset(&env, iters, rep));
    scenario!("J2", j2_providers_state(&env, iters, rep));
    scenario!("J3", j3_read_skill(&env, iters, rep));
    scenario!("J4", j4_usage_stats(&env, iters, rep));

    scenario!("K1", k1_config_upsert(&env, iters, rep));
    scenario!("K2", k2_config_list(&env, iters, rep));
    scenario!("K3", k3_preset_roundtrip(&env, iters, rep));
    scenario!("K4", k4_history_list(&env, iters, rep));
    scenario!("K5", k5_history_stats(&env, iters, rep));
    scenario!("K6", k6_supported_agents(&env, iters, rep));

    scenario!("L1", l1_get_settings(&env, iters, rep));
    scenario!("L2", l2_save_snapshot(&env, 64, "L2", iters, rep));
    scenario!("L2b", l2_save_snapshot(&env, 512, "L2b", iters, rep));
    scenario!("L3", l3_get_snapshot(&env, iters, rep));
    scenario!("L4", l4_list_tasks(&env, iters, rep));
    scenario!("L5", l5_list_history(&env, iters, rep));

    // R：必须最后测量（前面场景的分配都计入常驻内存）
    if selected.iter().any(|s| s.id == "R1") {
        r1_memory(&env, rep);
    }

    env.shutdown().await;
    Ok(())
}

// ==================== H · 装载 ====================

fn record_activation(env: &AppsEnv, rep: &mut Report) {
    if env.activation.is_empty() {
        rep.failures.push("H1: 无激活记录".to_string());
        return;
    }
    for record in &env.activation {
        let short = record.id.rsplit('.').next().unwrap_or(record.id);
        let id = format!("H1.activate.{short}");
        let note = if record.ok {
            "冷激活（组件实例化 + activate）"
        } else {
            "激活失败（原因见 failures）"
        };
        rep.record(
            &id,
            "H 装载",
            &format!("{} 激活", record.label),
            "ms",
            vec![record.millis],
            note,
        );
        if record.ok {
            rep.budget(
                &id,
                &format!("{} 激活", record.label),
                record.millis,
                ACTIVATION_BUDGET_MS,
                "ms",
                Cmp::Less,
            );
        } else {
            rep.failures.push(format!(
                "{id}: {}",
                record.error.as_deref().unwrap_or("激活失败")
            ));
        }
    }
}

// ==================== I · AI Chatbox ====================

/// I1 / I2：save-message 的载荷尺寸两档。命令本体是「读整份 JSONL → 追加 →
/// 写回」，耗时随会话文件体积增长，故两档分别看固定开销与线性项。
async fn i1_save_message(env: &AppsEnv, bytes: usize, iters: usize, rep: &mut Report) -> anyhow::Result<()> {
    env.ensure_active(AI_CHATBOX)?;
    let (id, label, budget_us) = if bytes <= 1024 {
        ("I1.save-message-1k", "save-message 1 KiB", 50_000.0)
    } else {
        ("I2.save-message-64k", "save-message 64 KiB", 200_000.0)
    };
    let content = "x".repeat(bytes);
    let args = || {
        json!({
            "conversationId": CHAT_CONVERSATION_ID,
            "role": "assistant",
            "content": content,
            "timestamp": "2026-01-01T00:00:02Z",
            "replaceLastAssistant": true
        })
    };
    // 预热（首调含会话文件首次打开 + JSONL 解析缓存）
    ensure_success(
        &env.call(AI_CHATBOX, "ai-chatbox.save-message", args()).await?,
        "save-message 预热",
    )?;
    let mut samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let (value, duration) = env
            .call_timed(AI_CHATBOX, "ai-chatbox.save-message", args())
            .await?;
        ensure_success(&value, "save-message")?;
        samples.push(micros(duration));
    }
    rep.record(
        id,
        "I AI Chatbox",
        label,
        "µs",
        samples.clone(),
        "JSONL 读改写 + 索引重写",
    );
    rep.budget(id, label, median(&samples), budget_us, "µs", Cmp::Less);
    Ok(())
}

async fn i2_save_message(env: &AppsEnv, bytes: usize, iters: usize, rep: &mut Report) -> anyhow::Result<()> {
    i1_save_message(env, bytes, iters, rep).await
}

async fn i3_get_messages(env: &AppsEnv, iters: usize, rep: &mut Report) -> anyhow::Result<()> {
    env.ensure_active(AI_CHATBOX)?;
    // 前置一份 64 KiB 行：读取路径要真的搬大字符串
    ensure_success(
        &env.call(
            AI_CHATBOX,
            "ai-chatbox.save-message",
            json!({
                "conversationId": CHAT_CONVERSATION_ID,
                "role": "user",
                "content": "y".repeat(65536),
                "timestamp": "2026-01-01T00:00:03Z"
            }),
        )
        .await?,
        "get-messages 前置写入",
    )?;
    let args = || json!({ "conversationId": CHAT_CONVERSATION_ID });
    env.call(AI_CHATBOX, "ai-chatbox.get-messages", args()).await?;
    let mut samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let (value, duration) = env
            .call_timed(AI_CHATBOX, "ai-chatbox.get-messages", args())
            .await?;
        let messages = json_array(&value, "messages", "get-messages")?;
        anyhow::ensure!(messages.len() >= 2, "get-messages 返回不足 2 条: {}", messages.len());
        samples.push(micros(duration));
    }
    rep.record(
        "I3.get-messages",
        "I AI Chatbox",
        "get-messages（64 KiB 行）",
        "µs",
        samples.clone(),
        "整份 JSONL 读取 + 逐行解析",
    );
    rep.budget("I3.get-messages", "get-messages", median(&samples), 200_000.0, "µs", Cmp::Less);
    Ok(())
}

async fn i4_list_conversations(env: &AppsEnv, iters: usize, rep: &mut Report) -> anyhow::Result<()> {
    env.ensure_active(AI_CHATBOX)?;
    let args = || json!({});
    env.call(AI_CHATBOX, "ai-chatbox.list-conversations", args()).await?;
    let mut samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let (value, duration) = env
            .call_timed(AI_CHATBOX, "ai-chatbox.list-conversations", args())
            .await?;
        let conversations = json_array(&value, "conversations", "list-conversations")?;
        anyhow::ensure!(
            conversations
                .iter()
                .any(|c| c.get("id").and_then(|v| v.as_str()) == Some(CHAT_CONVERSATION_ID)),
            "list-conversations 未包含预置会话"
        );
        samples.push(micros(duration));
    }
    rep.record(
        "I4.list-conversations",
        "I AI Chatbox",
        "list-conversations",
        "µs",
        samples.clone(),
        "索引读取 + 逐条存在性判定",
    );
    rep.budget("I4.list-conversations", "list-conversations", median(&samples), 100_000.0, "µs", Cmp::Less);
    Ok(())
}

async fn i5_save_conversation(env: &AppsEnv, iters: usize, rep: &mut Report) -> anyhow::Result<()> {
    env.ensure_active(AI_CHATBOX)?;
    let args = || conversation_payload("Perf Conversation (updated)");
    env.call(AI_CHATBOX, "ai-chatbox.save-conversation", args()).await?;
    let mut samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let (value, duration) = env
            .call_timed(AI_CHATBOX, "ai-chatbox.save-conversation", args())
            .await?;
        ensure_success(&value, "save-conversation")?;
        samples.push(micros(duration));
    }
    rep.record(
        "I5.save-conversation",
        "I AI Chatbox",
        "save-conversation 更新",
        "µs",
        samples.clone(),
        "meta 首行重写 + 索引更新",
    );
    rep.budget("I5.save-conversation", "save-conversation", median(&samples), 50_000.0, "µs", Cmp::Less);
    Ok(())
}

async fn i6_delete_conversation(env: &AppsEnv, iters: usize, rep: &mut Report) -> anyhow::Result<()> {
    env.ensure_active(AI_CHATBOX)?;
    let mut samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        // 每轮先重建（不计时），保证删除路径命中真实文件
        ensure_success(
            &env.call(
                AI_CHATBOX,
                "ai-chatbox.save-conversation",
                conversation_payload("Perf Conversation"),
            )
            .await?,
            "delete 前置重建",
        )?;
        let (value, duration) = env
            .call_timed(
                AI_CHATBOX,
                "ai-chatbox.delete-conversation",
                json!({ "conversationId": CHAT_CONVERSATION_ID }),
            )
            .await?;
        ensure_success(&value, "delete-conversation")?;
        samples.push(micros(duration));
    }
    rep.record(
        "I6.delete-conversation",
        "I AI Chatbox",
        "delete-conversation",
        "µs",
        samples.clone(),
        "删会话文件 + 索引移除",
    );
    rep.budget("I6.delete-conversation", "delete-conversation", median(&samples), 100_000.0, "µs", Cmp::Less);
    // 复位：后续场景（或下一次运行前的调试）仍能拿到预置会话
    let _ = env
        .call(
            AI_CHATBOX,
            "ai-chatbox.save-conversation",
            conversation_payload("Perf Conversation"),
        )
        .await;
    Ok(())
}

fn conversation_payload(title: &str) -> Value {
    json!({
        "conversation": {
            "id": CHAT_CONVERSATION_ID,
            "title": title,
            "createdAt": "2026-01-01T00:00:00Z",
            "updatedAt": "2026-01-01T00:00:02Z",
            "providerId": "perf",
            "providerName": "Perf",
            "model": "perf-model"
        }
    })
}

// ==================== J · Agent Hub ====================

async fn j1_save_preset(env: &AppsEnv, iters: usize, rep: &mut Report) -> anyhow::Result<()> {
    env.ensure_active(AGENT_HUB)?;
    // 前置：建一个同名预设，随后计时「更新」路径（新建只在首启发生）
    let create = env
        .call(
            AGENT_HUB,
            "agent-hub.save-preset",
            json!({
                "name": AGENT_HUB_PRESET,
                "baseUrl": "http://127.0.0.1:9/v1",
                "apiStyle": "openai",
                "models": ["perf-model"]
            }),
        )
        .await?;
    ensure_saved(&create, "save-preset 前置新建")?;
    let id = find_preset_id(env).await?;
    let args = || {
        json!({
            "id": id,
            "name": AGENT_HUB_PRESET,
            "baseUrl": "http://127.0.0.1:9/v1",
            "apiStyle": "openai",
            "models": ["perf-model"]
        })
    };
    env.call(AGENT_HUB, "agent-hub.save-preset", args()).await?;
    let mut samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let (value, duration) = env
            .call_timed(AGENT_HUB, "agent-hub.save-preset", args())
            .await?;
        ensure_saved(&value, "save-preset")?;
        samples.push(micros(duration));
    }
    rep.record(
        "J1.save-preset",
        "J Agent Hub",
        "save-preset 更新",
        "µs",
        samples.clone(),
        "SQLite UPDATE + 状态重读 + 事件推送",
    );
    rep.budget("J1.save-preset", "save-preset 更新", median(&samples), 100_000.0, "µs", Cmp::Less);
    let _ = env
        .call(AGENT_HUB, "agent-hub.delete-preset", json!({ "id": id }))
        .await;
    Ok(())
}

async fn find_preset_id(env: &AppsEnv) -> anyhow::Result<i64> {
    let state = env
        .call(AGENT_HUB, "agent-hub.get-providers-state", json!({}))
        .await?;
    state
        .get("state")
        .and_then(|s| s.get("presets"))
        .and_then(|p| p.as_array())
        .and_then(|presets| {
            presets
                .iter()
                .find(|p| p.get("name").and_then(|v| v.as_str()) == Some(AGENT_HUB_PRESET))
        })
        .and_then(|p| p.get("id"))
        .and_then(|v| v.as_i64())
        .ok_or_else(|| anyhow::anyhow!("get-providers-state 未返回预置预设 {AGENT_HUB_PRESET}"))
}

async fn j2_providers_state(env: &AppsEnv, iters: usize, rep: &mut Report) -> anyhow::Result<()> {
    env.ensure_active(AGENT_HUB)?;
    let args = || json!({});
    env.call(AGENT_HUB, "agent-hub.get-providers-state", args()).await?;
    let mut samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let (value, duration) = env
            .call_timed(AGENT_HUB, "agent-hub.get-providers-state", args())
            .await?;
        let state = value
            .get("state")
            .ok_or_else(|| anyhow::anyhow!("get-providers-state 缺少 state: {value}"))?;
        anyhow::ensure!(
            state.get("presets").and_then(|v| v.as_array()).is_some(),
            "get-providers-state 缺少 presets 数组"
        );
        samples.push(micros(duration));
    }
    rep.record(
        "J2.providers-state",
        "J Agent Hub",
        "get-providers-state",
        "µs",
        samples.clone(),
        "预设列表 + 本机 CLI 视图探活",
    );
    rep.budget("J2.providers-state", "get-providers-state", median(&samples), 1_000_000.0, "µs", Cmp::Less);
    Ok(())
}

async fn j3_read_skill(env: &AppsEnv, iters: usize, rep: &mut Report) -> anyhow::Result<()> {
    env.ensure_active(AGENT_HUB)?;
    let args = || json!({ "dir": AGENT_HUB_SKILL_DIR });
    env.call(AGENT_HUB, "agent-hub.read-skill", args()).await?;
    let mut samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let (value, duration) = env
            .call_timed(AGENT_HUB, "agent-hub.read-skill", args())
            .await?;
        let content = value
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        anyhow::ensure!(content.contains("Perf Skill"), "read-skill 内容不含夹具标记");
        samples.push(micros(duration));
    }
    rep.record(
        "J3.read-skill",
        "J Agent Hub",
        "read-skill（SKILL.md）",
        "µs",
        samples.clone(),
        "fs_read + frontmatter 解析",
    );
    rep.budget("J3.read-skill", "read-skill", median(&samples), 100_000.0, "µs", Cmp::Less);
    Ok(())
}

async fn j4_usage_stats(env: &AppsEnv, iters: usize, rep: &mut Report) -> anyhow::Result<()> {
    env.ensure_active(AGENT_HUB)?;
    let args = || json!({ "days": 0 });
    env.call(AGENT_HUB, "agent-hub.get-usage-stats", args()).await?;
    let mut samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let (value, duration) = env
            .call_timed(AGENT_HUB, "agent-hub.get-usage-stats", args())
            .await?;
        anyhow::ensure!(
            value.get("total").and_then(|v| v.as_object()).is_some(),
            "get-usage-stats 缺少 total 对象"
        );
        samples.push(micros(duration));
    }
    rep.record(
        "J4.usage-stats",
        "J Agent Hub",
        "get-usage-stats（全量）",
        "µs",
        samples.clone(),
        "六条聚合 SQL（空库基线）",
    );
    rep.budget("J4.usage-stats", "get-usage-stats", median(&samples), 5_000_000.0, "µs", Cmp::Less);
    Ok(())
}

// ==================== K · BedCode ====================

async fn k1_config_upsert(env: &AppsEnv, iters: usize, rep: &mut Report) -> anyhow::Result<()> {
    env.ensure_active(BEDCODE)?;
    let environment = if cfg!(windows) { "windows" } else { "linux" };
    let command = if cfg!(windows) { "powershell" } else { "bash" };
    let working_dir = env.workspace.to_string_lossy().to_string();
    let create = env
        .call(
            BEDCODE,
            "session.config.upsert",
            json!({
                "name": "perf-config",
                "environment": environment,
                "workingDir": working_dir,
                "command": command,
                "autoStart": false
            }),
        )
        .await?;
    let id = create
        .get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("session.config.upsert 未返回 id: {create}"))?;
    let args = || {
        json!({
            "id": id,
            "name": "perf-config",
            "environment": environment,
            "workingDir": working_dir,
            "command": command,
            "autoStart": false
        })
    };
    env.call(BEDCODE, "session.config.upsert", args()).await?;
    let mut samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let (value, duration) = env
            .call_timed(BEDCODE, "session.config.upsert", args())
            .await?;
        anyhow::ensure!(
            value.get("id").and_then(|v| v.as_str()) == Some(id.as_str()),
            "session.config.upsert 返回 id 不一致: {value}"
        );
        samples.push(micros(duration));
    }
    rep.record(
        "K1.config-upsert",
        "K BedCode",
        "session.config.upsert 更新",
        "µs",
        samples.clone(),
        "私有库 UPDATE（配置域真源）",
    );
    rep.budget("K1.config-upsert", "config upsert", median(&samples), 100_000.0, "µs", Cmp::Less);
    Ok(())
}

async fn k2_config_list(env: &AppsEnv, iters: usize, rep: &mut Report) -> anyhow::Result<()> {
    env.ensure_active(BEDCODE)?;
    // 前置：确保至少有一条配置（单独跑 K2 时 K1 可能未执行）
    env.call(
        BEDCODE,
        "session.config.upsert",
        json!({
            "name": "perf-config",
            "environment": if cfg!(windows) { "windows" } else { "linux" },
            "workingDir": env.workspace.to_string_lossy(),
            "command": if cfg!(windows) { "powershell" } else { "bash" },
            "autoStart": false
        }),
    )
    .await?;
    let args = || json!({});
    env.call(BEDCODE, "session.config.list", args()).await?;
    let mut samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let (value, duration) = env
            .call_timed(BEDCODE, "session.config.list", args())
            .await?;
        let list = value
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("session.config.list 非数组: {value}"))?;
        anyhow::ensure!(!list.is_empty(), "session.config.list 为空");
        samples.push(micros(duration));
    }
    rep.record(
        "K2.config-list",
        "K BedCode",
        "session.config.list",
        "µs",
        samples.clone(),
        "配置表全量读 + camelCase 序列化",
    );
    rep.budget("K2.config-list", "config list", median(&samples), 500_000.0, "µs", Cmp::Less);
    Ok(())
}

async fn k3_preset_roundtrip(env: &AppsEnv, iters: usize, rep: &mut Report) -> anyhow::Result<()> {
    env.ensure_active(BEDCODE)?;
    env.call(
        BEDCODE,
        "session.task.preset-create",
        json!({ "prompt": "perf warmup" }),
    )
    .await?;
    let mut create_samples = Vec::with_capacity(iters);
    let mut delete_samples = Vec::with_capacity(iters);
    for i in 0..iters {
        let (created, create_time) = env
            .call_timed(
                BEDCODE,
                "session.task.preset-create",
                json!({ "prompt": format!("perf preset {i}") }),
            )
            .await?;
        let preset_id = created
            .get("preset_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow::anyhow!("preset-create 未返回 preset_id: {created}"))?;
        let (deleted, delete_time) = env
            .call_timed(
                BEDCODE,
                "session.task.preset-delete",
                json!({ "preset_id": preset_id }),
            )
            .await?;
        ensure_true(&deleted, "deleted", "preset-delete")?;
        create_samples.push(micros(create_time));
        delete_samples.push(micros(delete_time));
    }
    rep.record(
        "K3.preset-create",
        "K BedCode",
        "preset-create",
        "µs",
        create_samples.clone(),
        "预设任务 INSERT",
    );
    rep.budget("K3.preset-create", "preset-create", median(&create_samples), 200_000.0, "µs", Cmp::Less);
    rep.record(
        "K3.preset-delete",
        "K BedCode",
        "preset-delete",
        "µs",
        delete_samples.clone(),
        "预设任务 DELETE",
    );
    rep.budget("K3.preset-delete", "preset-delete", median(&delete_samples), 200_000.0, "µs", Cmp::Less);
    Ok(())
}

async fn k4_history_list(env: &AppsEnv, iters: usize, rep: &mut Report) -> anyhow::Result<()> {
    env.ensure_active(BEDCODE)?;
    let args = || json!({ "limit": 50, "offset": 0 });
    env.call(BEDCODE, "session.task.history-list", args()).await?;
    let mut samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let (value, duration) = env
            .call_timed(BEDCODE, "session.task.history-list", args())
            .await?;
        json_array(&value, "tasks", "history-list")?;
        anyhow::ensure!(
            value.get("total").and_then(|v| v.as_i64()).is_some(),
            "history-list 缺少 total"
        );
        samples.push(micros(duration));
    }
    rep.record(
        "K4.history-list",
        "K BedCode",
        "history-list",
        "µs",
        samples.clone(),
        "任务历史分页 + 工作目录回填 + 计数",
    );
    rep.budget("K4.history-list", "history-list", median(&samples), 500_000.0, "µs", Cmp::Less);
    Ok(())
}

async fn k5_history_stats(env: &AppsEnv, iters: usize, rep: &mut Report) -> anyhow::Result<()> {
    env.ensure_active(BEDCODE)?;
    let args = || json!({});
    env.call(BEDCODE, "session.task.history-stats", args()).await?;
    let mut samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let (value, duration) = env
            .call_timed(BEDCODE, "session.task.history-stats", args())
            .await?;
        anyhow::ensure!(
            value.get("total").and_then(|v| v.as_i64()).is_some(),
            "history-stats 缺少 total"
        );
        anyhow::ensure!(
            value.get("by_status").and_then(|v| v.as_object()).is_some(),
            "history-stats 缺少 by_status"
        );
        samples.push(micros(duration));
    }
    rep.record(
        "K5.history-stats",
        "K BedCode",
        "history-stats",
        "µs",
        samples.clone(),
        "按状态聚合 + 时长均值",
    );
    rep.budget("K5.history-stats", "history-stats", median(&samples), 500_000.0, "µs", Cmp::Less);
    Ok(())
}

async fn k6_supported_agents(env: &AppsEnv, iters: usize, rep: &mut Report) -> anyhow::Result<()> {
    env.ensure_active(BEDCODE)?;
    let args = || json!({});
    env.call(BEDCODE, "session.task.supported-agents", args()).await?;
    let mut samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let (value, duration) = env
            .call_timed(BEDCODE, "session.task.supported-agents", args())
            .await?;
        let agents = json_array(&value, "agents", "supported-agents")?;
        anyhow::ensure!(!agents.is_empty(), "supported-agents 为空");
        samples.push(micros(duration));
    }
    rep.record(
        "K6.supported-agents",
        "K BedCode",
        "supported-agents",
        "µs",
        samples.clone(),
        "纯计算面（agent registry）",
    );
    rep.budget("K6.supported-agents", "supported-agents", median(&samples), 100_000.0, "µs", Cmp::Less);
    Ok(())
}

// ==================== L · File Transfer ====================

async fn l1_get_settings(env: &AppsEnv, iters: usize, rep: &mut Report) -> anyhow::Result<()> {
    env.ensure_active(FILE_TRANSFER)?;
    let args = || json!({});
    env.call(FILE_TRANSFER, "file-transfer.get-settings", args()).await?;
    let mut samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let (value, duration) = env
            .call_timed(FILE_TRANSFER, "file-transfer.get-settings", args())
            .await?;
        anyhow::ensure!(
            value.get("policy_mode").and_then(|v| v.as_str()).is_some(),
            "get-settings 缺少 policy_mode"
        );
        samples.push(micros(duration));
    }
    rep.record(
        "L1.get-settings",
        "L File Transfer",
        "get-settings",
        "µs",
        samples.clone(),
        "设置读取 + 共享根注册表装载",
    );
    rep.budget("L1.get-settings", "get-settings", median(&samples), 100_000.0, "µs", Cmp::Less);
    Ok(())
}

/// L2 / L2b：设备快照写入。载荷是前端「最近可见设备」缓存的真实形状。
async fn l2_save_snapshot(
    env: &AppsEnv,
    count: usize,
    id: &'static str,
    iters: usize,
    rep: &mut Report,
) -> anyhow::Result<()> {
    env.ensure_active(FILE_TRANSFER)?;
    let entries = snapshot_entries(count);
    let args = || json!({ "devices": entries });
    let label = format!("save-device-snapshot {count} 条");
    ensure_true(
        &env.call(FILE_TRANSFER, "file-transfer.save-device-snapshot", args())
            .await?,
        "ok",
        "save-device-snapshot 预热",
    )?;
    let mut samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let (value, duration) = env
            .call_timed(FILE_TRANSFER, "file-transfer.save-device-snapshot", args())
            .await?;
        ensure_true(&value, "ok", "save-device-snapshot")?;
        samples.push(micros(duration));
    }
    let measurement_id = format!("{id}.save-device-snapshot-{count}");
    rep.record(
        &measurement_id,
        "L File Transfer",
        &label,
        "µs",
        samples.clone(),
        "快照序列化 + 存储写入",
    );
    let budget = if count <= 64 { 100_000.0 } else { 500_000.0 };
    rep.budget(&measurement_id, &label, median(&samples), budget, "µs", Cmp::Less);
    Ok(())
}

async fn l3_get_snapshot(env: &AppsEnv, iters: usize, rep: &mut Report) -> anyhow::Result<()> {
    env.ensure_active(FILE_TRANSFER)?;
    let entries = snapshot_entries(64);
    ensure_true(
        &env.call(
            FILE_TRANSFER,
            "file-transfer.save-device-snapshot",
            json!({ "devices": entries }),
        )
        .await?,
        "ok",
        "get-device-snapshot 前置写入",
    )?;
    let args = || json!({});
    env.call(FILE_TRANSFER, "file-transfer.get-device-snapshot", args()).await?;
    let mut samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let (value, duration) = env
            .call_timed(FILE_TRANSFER, "file-transfer.get-device-snapshot", args())
            .await?;
        let devices = json_array(&value, "devices", "get-device-snapshot")?;
        anyhow::ensure!(
            devices.len() == 64,
            "get-device-snapshot 条数不符: {} != 64",
            devices.len()
        );
        samples.push(micros(duration));
    }
    rep.record(
        "L3.get-device-snapshot",
        "L File Transfer",
        "get-device-snapshot 64 条",
        "µs",
        samples.clone(),
        "存储读取 + 反序列化 + 回执",
    );
    rep.budget("L3.get-device-snapshot", "get-device-snapshot", median(&samples), 100_000.0, "µs", Cmp::Less);
    Ok(())
}

async fn l4_list_tasks(env: &AppsEnv, iters: usize, rep: &mut Report) -> anyhow::Result<()> {
    env.ensure_active(FILE_TRANSFER)?;
    let args = || json!({});
    env.call(FILE_TRANSFER, "file-transfer.list-tasks", args()).await?;
    let mut samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let (value, duration) = env
            .call_timed(FILE_TRANSFER, "file-transfer.list-tasks", args())
            .await?;
        anyhow::ensure!(value.is_array(), "list-tasks 非数组: {value}");
        samples.push(micros(duration));
    }
    rep.record(
        "L4.list-tasks",
        "L File Transfer",
        "list-tasks",
        "µs",
        samples.clone(),
        "活跃发送视图（空表基线）",
    );
    rep.budget("L4.list-tasks", "list-tasks", median(&samples), 100_000.0, "µs", Cmp::Less);
    Ok(())
}

async fn l5_list_history(env: &AppsEnv, iters: usize, rep: &mut Report) -> anyhow::Result<()> {
    env.ensure_active(FILE_TRANSFER)?;
    let args = || json!({});
    env.call(FILE_TRANSFER, "file-transfer.list-history", args()).await?;
    let mut samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let (value, duration) = env
            .call_timed(FILE_TRANSFER, "file-transfer.list-history", args())
            .await?;
        anyhow::ensure!(value.is_array(), "list-history 非数组: {value}");
        samples.push(micros(duration));
    }
    rep.record(
        "L5.list-history",
        "L File Transfer",
        "list-history",
        "µs",
        samples.clone(),
        "终态历史视图（空表基线）",
    );
    rep.budget("L5.list-history", "list-history", median(&samples), 100_000.0, "µs", Cmp::Less);
    Ok(())
}

fn snapshot_entries(count: usize) -> Vec<Value> {
    (0..count)
        .map(|i| {
            json!({
                "nodeId": format!("perf-node-{i:04}"),
                "deviceName": format!("Perf Device {i}"),
                "addr": "127.0.0.1",
                "port": 9000 + (i % 1000) as u16,
                "capabilitiesHex": "1",
                "instanceName": "perf-harness",
                "lastSeenMs": 1_700_000_000_000u64 + i as u64,
            })
        })
        .collect()
}

// ==================== R · 内存 ====================

fn r1_memory(env: &AppsEnv, rep: &mut Report) {
    let Some(current) = crate::support::rss::current_rss_bytes() else {
        rep.failures.push("R1: 当前 RSS 不可读".to_string());
        return;
    };
    let delta = current.saturating_sub(env.rss_before_bytes);
    rep.record(
        "R1.rss",
        "R 内存",
        "四应用常驻 RSS",
        "MiB",
        vec![crate::support::rss::mib(current)],
        &format!(
            "相对装配前增量 {:.1} MiB",
            crate::support::rss::mib(delta)
        ),
    );
    rep.budget(
        "R1.rss",
        "四应用常驻 RSS",
        crate::support::rss::mib(current),
        RESIDENT_RSS_BUDGET_MIB,
        "MiB",
        Cmp::Less,
    );
    match crate::support::rss::peak_rss_bytes() {
        Some(peak) => rep.record(
            "R1.peak-rss",
            "R 内存",
            "进程峰值 RSS",
            "MiB",
            vec![crate::support::rss::mib(peak)],
            "VmHWM（内核对峰值驻留集的记录）",
        ),
        None => rep.record(
            "R1.peak-rss",
            "R 内存",
            "进程峰值 RSS",
            "MiB",
            vec![crate::support::rss::mib(current)],
            "非 Linux：峰值不可用，退回当前值",
        ),
    }
}

// ==================== 小工具 ====================

fn median(samples: &[f64]) -> f64 {
    if samples.is_empty() {
        return f64::NAN;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    sorted[sorted.len() / 2]
}

fn json_array<'a>(value: &'a Value, key: &str, ctx: &str) -> anyhow::Result<&'a Vec<Value>> {
    value
        .get(key)
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow::anyhow!("{ctx}: 缺少数组字段 {key}: {value}"))
}

fn ensure_success(value: &Value, ctx: &str) -> anyhow::Result<()> {
    ensure_true(value, "success", ctx)
}

fn ensure_saved(value: &Value, ctx: &str) -> anyhow::Result<()> {
    ensure_true(value, "saved", ctx)
}

fn ensure_true(value: &Value, key: &str, ctx: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        value.get(key).and_then(|v| v.as_bool()) == Some(true),
        "{ctx}: 回执缺少 {key}=true: {value}"
    );
    Ok(())
}
