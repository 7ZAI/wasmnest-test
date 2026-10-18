//! 场景 9：事件通道 `session-control` 闭环（移动端 UI 的会话/任务信号面）
//!
//! 覆盖的盲区：**事件帧的真实往返**。移动端既有测试直接喂 `PluginEventRouter`
//! （帧是本仓按文档手写的），桌面端既有测试只验插件自己广播出去的帧壳——
//! 「桌面真实插件的广播 → 宿主 WS 端点 → 移动端真实 WS 客户端 → 事件路由 →
//! `MobileEvent`」这条真实链路从未被两端同时跑过。生产上，移动端会话列表、
//! 任务队列、任务历史的实时更新**全靠这条通道**；它静默失效时 UI 不会报错，
//! 只会「一直少一次刷新」。
//!
//! 驱动侧的口径：会话生命周期经**移动端真实插件**（`terminal-session.*` 命令 → HTTP 下令，
//! 与生产同一条路）；任务/模式/定时域经**桌面命令面 `plugin_command`**
//! （= 桌面 UI 那条路，`api_bridge::plugin_invoke` 的下游）——这些端点在移动端
//! **没有 Rust 客户端**（只经前端 HTTP 代理，见 `http_proxy_flow.rs`），故不存在
//! 「移动端真实客户端驱动」这一形态，断言面（移动端事件路由）不受影响。
//!
//! ## 就绪判据（为什么不靠 sleep）
//!
//! 事件通道有三条独立的时序：移动端 `ws_event_channel_ready`（首帧**发出后**
//! 即发射）、插件零客户端早退（看 `clientCount > 0`）、宿主注册表认证态（首帧
//! **被接受后**置位）。只有第三条是「这一帧真的会被广播出去」的终点，故就绪等待
//! 取它（`desktop_ctx::wait_plugin_ws_endpoint_authenticated`），其余两条都在它
//! 之前——用事件断言去等就绪会偶发丢首帧。
//!
//! ## 行为契约（unit-test-discipline G1：每条有来源）
//!
//! | 契约 | 来源（代码证据） | 行为 | 场景 |
//! |---|---|---|---|
//! | E-001a | `launch.rs::spawn_session` → `events::publish_created`；`sessions_http::start_session` 取 JWT claims 的 `deviceName` 作 `source_device` | 移动端 HTTP 建会话 → 移动端事件通道收到 `session:created` 并解析出**同一** `session.id`、`status == running`、`source_device` = 配对时申报的设备名（证明身份链真的跨过了 WS 面） | 正例 |
//! | E-001b | `session::on_pty_exit` → `publish_stopped`（唯一发布点） | 移动端 HTTP 停会话 → 收到 `session:stopped`，`session_id` 与名字逐字对齐 | 正例 |
//! | E-001c | `actions::remove_via_host` → `publish_removed` | 移动端 HTTP 删会话 → 收到 `session:removed` | 正例 |
//! | E-002a | `task::state::set_auto_mode` → `ws_events::session_mode_payload` | 桌面命令面改会话模式 → 收到 `session:mode-changed`，`auto_approve` 逐字段对齐 | 正例 |
//! | E-002b | `task::queue` 广播收口 `broadcast_queue_changed` | 入队一个任务 → 收到 `task:queue-changed`，`action == "add"` 且 `queue_count` 与真源一致 | 正例 |
//! | E-002c | `create_task_from_dispatch` → `broadcast_task_status` | 开启 auto_execute → 调度下发 → 收到 `task:status-changed`，`task_status == "in_progress"` 且带 reason | 正例 |
//! | E-002d | `scheduled::create_job_with_broadcast` | 桌面命令面建定时任务 → 收到 `task:scheduled-changed`，`status == "pending"` / `action == "create"` / `job_id` 与命令回执逐字一致 | 正例 |
//! | E-003a | `event_ws::run_supervisor` 自愈分支（非致命断开 → HTTP reauth → 重建） | 桌面断开事件通道 → 监督任务重建出**一条新连接**（client_id 变了且已认证）→ 新事件重新可达 | 正例 |
//! | E-003b | M1/ADR 0031：认证类致命关闭（4001/4003）**不自愈** + `plugin_event.rs`「事件不重放」 | 致命关闭后不得重连风暴；断链期间的真实事件丢失；**用户重新认证**后重建通道；重建后**不重放**漏掉的事件；缺口能靠 HTTP 全量拉取补齐 | 正例 + 反例 |
//! | E-004 | `channel/plugin.rs`（宿主 `auth:"jwt"` 端点闸门，失败 close 4001） | 伪造 JWT 建事件通道 → 桌面端显性关闭且**认证类致命**；此刻桌面真实广播的事件一帧都不许落地 | 反例（fail-closed） |
//!
//! `ws_event_channel_ready` 本身**不在本文件断言**：它经 `AppHandle::emit` 发给页面
//! WebView，无头环境没有前端（`app_handle = None` 时连发射都不发生）。本文件改为
//! 钉住它的**语义等价物**——认证首帧被桌面端接受后事件即刻可达（E-001a 起全程成立）。

mod common;

use std::time::Duration;

use wasmnest_mobile_lib::auth::http::{AuthHttpClient, DeviceAuthContext};
use wasmnest_mobile_lib::connection::event_ws::run_supervisor;
use wasmnest_mobile_lib::connection::WsClientEvent;
use wasmnest_mobile_lib::router::MobileEvent;
use wasmnest_mobile_lib::state::{get_auth_manager, get_connection_manager};
use wasmnest_mobile_lib::system::constants::connection::is_auth_fatal_close_code;

use common::desktop_ctx::{self, EVENT_CHANNEL_PATH, SESSION_PLUGIN_ID};
use common::mobile_ctx;

/// 配对时申报的设备名：JWT claims 里的 `device_name`，事件 `source_device` 的来源
const DEVICE_NAME: &str = "CrossEnd Event Phone";

/// 带移动 JWT 的真实 HTTP 建会话（形状 = 桌面 `/api/sessions/start`，与退役前移动侧
/// `SessionHttpClient::start_session` 逐字一致）：E-001a 要靠它验证「事件 `source_device`
/// 来自 JWT claims 的设备名」这条跨面身份链。
async fn start_session(base: &str, token: &str, config_id: &str, what: &str) -> String {
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/sessions/start"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "configId": config_id, "cols": 100, "rows": 30 }))
        .send()
        .await
        .unwrap_or_else(|e| panic!("{what} HTTP 建会话失败：{e}"));
    let body: serde_json::Value = resp.json().await.unwrap_or_else(|e| panic!("{what} 响应非 JSON：{e}"));
    assert_eq!(body["code"], 0, "{what} 建会话业务码应为 0：{body}");
    body["data"]["sessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("{what} 响应缺 sessionId：{body}"))
        .to_string()
}

/// 带移动 JWT 的真实 HTTP 停会话
async fn stop_session(base: &str, token: &str, session_id: &str, what: &str) {
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/sessions/{session_id}/stop"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap_or_else(|e| panic!("{what} 停会话失败：{e}"));
    assert_eq!(resp.status(), 200, "{what} 停会话 HTTP 状态应为 200");
}

/// 带移动 JWT 的真实 HTTP 移除会话
async fn remove_session(base: &str, token: &str, session_id: &str, what: &str) {
    let resp = reqwest::Client::new()
        .delete(format!("{base}/api/sessions/{session_id}/remove"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap_or_else(|e| panic!("{what} 移除会话失败：{e}"));
    assert_eq!(resp.status(), 200, "{what} 移除会话 HTTP 状态应为 200");
}

/// 带移动 JWT 的真实 HTTP 读会话列表（对账腿用）
async fn list_sessions(base: &str, token: &str) -> Vec<serde_json::Value> {
    let resp = reqwest::Client::new()
        .get(format!("{base}/api/sessions"))
        .bearer_auth(token)
        .send()
        .await
        .expect("读列表失败");
    let body: serde_json::Value = resp.json().await.expect("列表响应非 JSON");
    assert_eq!(body["code"], 0, "列表业务码应为 0：{body}");
    // 桌面契约：`data: {sessions: [...]}`（旧控制器逐字段形状）；数组形状一并接受
    let data = &body["data"];
    data.as_array()
        .cloned()
        .or_else(|| data["sessions"].as_array().cloned())
        .unwrap_or_default()
}

/// 播种的会话配置名（会话记录名 = 配置名，事件 `session_name` 的来源）
const CONFIG_NAME: &str = "cross-end-events";

/// 桌面命令面驱动（= 桌面 UI 那条路：`plugin_invoke` 的下游）
async fn plugin_command(command: &str, args: serde_json::Value) -> serde_json::Value {
    desktop_ctx::plugin_command(SESSION_PLUGIN_ID, command, args).await
}

/// 桌面侧主动断开事件通道的关闭码（**非**认证类：走监督任务自愈路径）
const OUTAGE_CLOSE_CODE: u16 = 1001;

/// 认证类致命关闭码（M1/ADR 0031：链路加密失败 = 重连前需重新配对/认证）
const AUTH_FATAL_CLOSE_CODE: u16 = 4003;

/// 场景失败时也要清临时目录（panic 路径走不到收尾的显式清理；实测失败一轮会在
/// `/tmp` 留一对目录，而磁盘常年 90%+）
struct TempDirGuard;

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        desktop_ctx::cleanup_temp_dirs();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plugin_events_reach_mobile_over_real_event_channel() {
    if tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::DEBUG)
        .try_init()
        .is_err()
    {
        tracing::debug!("tracing subscriber already initialized");
    }

    desktop_ctx::init_app_context().await;
    let _temp_dirs = TempDirGuard;
    let (port, handle, server_task) = desktop_ctx::start_server().await;
    mobile_ctx::set_target(port).await;
    let base = mobile_ctx::base_url(port);
    let address = format!("127.0.0.1:{port}");

    // ==================== E-004 反例：伪造 JWT 的事件通道被闸门挡住 ====================
    // 放在真实认证之前跑：此刻还没有任何合法连接，桌面端广播的事件帧若有落地，
    // 只可能来自这条没过闸门的连接。
    let events = mobile_ctx::MobileEventRecorder::attach();
    mobile_ctx::remember_token("forged.jwt.token");
    let forged = get_connection_manager()
        .establish_event_ws(None)
        .await
        .expect("E-004 建连本身不等待认证回执（首帧发出即返回），应拿到 client");
    let mut forged_events = forged.subscribe();
    let close_code = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match forged_events.recv().await {
                Ok(WsClientEvent::ServerClosed { code, reason }) => {
                    tracing::debug!("E-004 桌面端关闭事件通道: code={code} reason={reason}");
                    return code;
                }
                Ok(other) => tracing::debug!("E-004 关链途中事件（忽略）: {other:?}"),
                Err(e) => panic!("E-004 事件通道事件流异常关闭: {e}"),
            }
        }
    })
    .await
    .expect("E-004 未过首帧认证的事件通道必须被桌面端显式关闭（不得静默挂着）");
    assert!(
        is_auth_fatal_close_code(close_code),
        "E-004 认证被拒的关闭码必须是认证类致命（4001/4003，客户端据此不自愈、要求重新配对），\
         实际 code={close_code}"
    );

    // 闸门期间桌面端真的广播一条（入队即广播，队列真源写入插件私有库）
    let ghost_session = "crossend-event-unauthed-probe";
    plugin_command(
        "session.task.queue-add",
        serde_json::json!({ "session_id": ghost_session, "prompt": "E-004 probe" }),
    )
    .await;
    mobile_ctx::assert_no_mobile_event(
        &events,
        "E-004 未过认证的连接一帧事件都不许落地",
        Duration::from_millis(800),
        |e| matches!(e, MobileEvent::SyncTaskQueueChanged { session_id, .. } if session_id == ghost_session),
    )
    .await;
    forged.disconnect().await;
    mobile_ctx::clear_identity();

    // ==================== E-001/E-002 正例：真实配对 → 事件通道就绪 ====================

    // 监督任务必须**先于**认证订阅（生产顺序：HTTP 认证成功的 AuthSuccess 事件
    // 是建连契口，晚订阅就永远等不到）
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let _supervisor = tokio::spawn(run_supervisor(get_connection_manager(), None, Some(ready_tx)));
    ready_rx
        .await
        .expect("E-001 监督任务应在启动后完成事件总线订阅（ready 握手）");

    let auth = AuthHttpClient::new();
    let ctx = DeviceAuthContext {
        device_id: "crossend-event-device",
        device_name: DEVICE_NAME,
        fingerprint: "crossend-event-fp",
        uid_hash: None,
    };
    let pairing = auth
        .request_pairing(&base, ctx.device_id, ctx.device_name, ctx.fingerprint)
        .await
        .expect("E-001 配对码签发");
    let token = auth
        .verify_pairing_code(&base, ctx, &pairing.pairing_code, &address)
        .await
        .expect("E-001 配对码换 token")
        .token;

    // 生产收尾走 AuthManager（不是测试自拼）：凭据 / 全局 token / 链路加密 pin /
    // `AuthSuccess` 广播都由 `apply_auth_success` 一处落地，监督任务据此建连
    get_auth_manager()
        .authenticate_with_token(&token)
        .await
        .expect("E-001 真实 HTTP reauth 必须成功（认证中心自签自验闭环）");

    // 无竞态就绪判据：桌面端注册表上出现「首帧认证已通过」的连接
    desktop_ctx::wait_plugin_ws_endpoint_authenticated(
        SESSION_PLUGIN_ID,
        EVENT_CHANNEL_PATH,
        "E-001 事件通道首帧认证被桌面端接受",
    )
    .await;

    // 会话下令面在移动真插件里（HTTP + 宿主代注 JWT）：与事件通道（移动 lib 的
    // 监督任务/路由）在同一进程内并行跑，正是跨端真实形态
    let config_id = desktop_ctx::seed_shell_config(CONFIG_NAME).await;

    // ==================== E-001a 会话创建事件 ====================
    let session_id = start_session(&base, &token, &config_id, "E-001a").await;
    let created = events
        .wait_for("E-001a session:created", |e| {
            matches!(e, MobileEvent::SyncSessionCreated { .. })
        })
        .await;
    match created {
        MobileEvent::SyncSessionCreated { session, source_device } => {
            assert_eq!(
                session.id, session_id,
                "E-001a 事件里的会话 id 必须与 HTTP 建出的那个一致（防张冠李戴的恒真断言）"
            );
            assert_eq!(session.status, "running", "E-001a 新建会话事件状态应为 running");
            assert_eq!(
                session.config_id.as_deref(),
                Some(config_id.as_str()),
                "E-001a 事件载荷须自带 config_id（载荷自足：消费方不必回查真源）"
            );
            assert_eq!(
                source_device, DEVICE_NAME,
                "E-001a source_device 应取 JWT claims 的 deviceName（配对时申报的设备名），\
                 说明身份链真的跨过了 WS 面"
            );
        }
        other => panic!("E-001a 应为 SyncSessionCreated，got {other:?}"),
    }

    // ==================== E-001b 会话停止事件 ====================
    stop_session(&base, &token, &session_id, "E-001b").await;
    let stopped = events
        .wait_for("E-001b session:stopped", |e| {
            matches!(e, MobileEvent::SyncSessionStopped { .. })
        })
        .await;
    match stopped {
        MobileEvent::SyncSessionStopped {
            session_id: sid,
            session_name,
        } => {
            assert_eq!(sid, session_id, "E-001b 停止事件的会话 id 必须一致");
            assert_eq!(
                session_name, CONFIG_NAME,
                "E-001b 停止事件须带退出前的名字快照（载荷自足，消费方无需回查）"
            );
        }
        other => panic!("E-001b 应为 SyncSessionStopped，got {other:?}"),
    }

    // ==================== E-001c 会话移除事件 ====================
    remove_session(&base, &token, &session_id, "E-001c").await;
    let removed = events
        .wait_for("E-001c session:removed", |e| {
            matches!(e, MobileEvent::SyncSessionRemoved { .. })
        })
        .await;
    match removed {
        MobileEvent::SyncSessionRemoved {
            session_id: sid,
            session_name,
        } => {
            assert_eq!(sid, session_id, "E-001c 移除事件的会话 id 必须一致");
            assert_eq!(session_name, CONFIG_NAME, "E-001c 移除事件须带会话名");
        }
        other => panic!("E-001c 应为 SyncSessionRemoved，got {other:?}"),
    }

    // ==================== E-002 任务域 / 模式域 / 定时域事件 ====================
    // 任务事件需要一个在跑的会话承载（调度下发要求会话仍在登记域内）
    let task_session = start_session(&base, &token, &config_id, "E-002").await;

    // ---- E-002a session:mode-changed（先关自动执行：入队不立即下发，队列数可预期） ----
    plugin_command(
        "session.task.set-auto-mode",
        serde_json::json!({ "session_id": task_session, "auto_execute": false, "auto_answer": true }),
    )
    .await;
    let mode = events
        .wait_for("E-002a session:mode-changed", |e| {
            matches!(e, MobileEvent::SyncSessionModeChanged { .. })
        })
        .await;
    match mode {
        MobileEvent::SyncSessionModeChanged {
            session_id: sid,
            auto_approve,
        } => {
            assert_eq!(sid, task_session, "E-002a 模式事件的会话 id 必须一致");
            assert!(
                auto_approve,
                "E-002a auto_approve 应为 true（桌面侧写入 auto_answer=true，wire 键名映射在此被钉住）"
            );
        }
        other => panic!("E-002a 应为 SyncSessionModeChanged，got {other:?}"),
    }

    // ---- E-002b task:queue-changed（入队一帧） ----
    plugin_command(
        "session.task.queue-add",
        serde_json::json!({ "session_id": task_session, "prompt": "cross-end-event-probe" }),
    )
    .await;
    let queued = events
        .wait_for(
            "E-002b task:queue-changed(add)",
            |e| matches!(e, MobileEvent::SyncTaskQueueChanged { action, .. } if action == "add"),
        )
        .await;
    match queued {
        MobileEvent::SyncTaskQueueChanged {
            session_id: sid,
            queue_count,
            ..
        } => {
            assert_eq!(sid, task_session, "E-002b 队列事件的会话 id 必须一致");
            assert_eq!(queue_count, 1, "E-002b 入队一帧后待执行数应为 1（数字段不是字符串）");
        }
        other => panic!("E-002b 应为 SyncTaskQueueChanged，got {other:?}"),
    }

    // ---- E-002c task:status-changed（开自动执行 → 调度下发 → in_progress） ----
    plugin_command(
        "session.task.set-auto-mode",
        serde_json::json!({ "session_id": task_session, "auto_execute": true }),
    )
    .await;
    let status = events
        .wait_for("E-002c task:status-changed(in_progress)", |e| {
            matches!(e, MobileEvent::SyncTaskStatusChanged { .. })
        })
        .await;
    match status {
        MobileEvent::SyncTaskStatusChanged {
            session_id: sid,
            task_status,
            task_reason,
            ..
        } => {
            assert_eq!(sid, task_session, "E-002c 任务状态事件的会话 id 必须一致");
            assert_eq!(
                task_status, "in_progress",
                "E-002c 开启自动执行后队列任务应被下发，状态为 in_progress"
            );
            assert!(
                task_reason.as_deref().is_some_and(|r| !r.is_empty()),
                "E-002c 下发应带 reason（载荷自足：移动端据此解释为什么任务跑起来了），got {task_reason:?}"
            );
        }
        other => panic!("E-002c 应为 SyncTaskStatusChanged，got {other:?}"),
    }

    // ---- E-002d task:scheduled-changed（定时任务建档） ----
    let scheduled = plugin_command(
        "session.task.scheduled-create",
        serde_json::json!({
            "name": "cross-end-scheduled",
            "config_id": config_id,
            // 远期触发：只验证「建档即广播」，不触发真实调度（否则会凭空起会话）
            "trigger_at": "2099-01-01 00:00:00",
            "prompts": ["cross-end-event-scheduled"],
        }),
    )
    .await;
    let scheduled_event = events
        .wait_for("E-002d task:scheduled-changed(create)", |e| {
            matches!(e, MobileEvent::SyncTaskScheduledChanged { .. })
        })
        .await;
    match scheduled_event {
        MobileEvent::SyncTaskScheduledChanged { job_id, status, action } => {
            assert_eq!(
                job_id,
                scheduled["job_id"].as_str().unwrap_or_default(),
                "E-002d 事件里的 job_id 必须与命令回执逐字一致（事件与真源同一条写入）"
            );
            assert_eq!(status, "pending", "E-002d 新建定时任务状态应为 pending");
            assert_eq!(action, "create", "E-002d 触发动作应为 create");
        }
        other => panic!("E-002d 应为 SyncTaskScheduledChanged，got {other:?}"),
    }

    // ==================== E-003a 意外断链：监督任务自愈重建 ====================
    let before_outage = desktop_ctx::wait_plugin_ws_endpoint_authenticated(
        SESSION_PLUGIN_ID,
        EVENT_CHANNEL_PATH,
        "E-003a 自愈前的连接",
    )
    .await;
    let dropped = desktop_ctx::disconnect_plugin_ws_endpoint_clients(
        SESSION_PLUGIN_ID,
        EVENT_CHANNEL_PATH,
        OUTAGE_CLOSE_CODE,
        "cross-end outage simulation",
    )
    .await;
    assert_eq!(dropped, 1, "E-003a 断链前事件通道上应恰好有一个在线连接");
    let healed = desktop_ctx::wait_new_plugin_ws_endpoint_client(
        SESSION_PLUGIN_ID,
        EVENT_CHANNEL_PATH,
        &before_outage.client_id,
        "E-003a 监督任务自愈重建事件通道",
    )
    .await;
    assert_ne!(
        healed.client_id, before_outage.client_id,
        "E-003a 自愈必须重建一条新连接（而不是沿用断链前的旧句柄）"
    );
    // 自愈后的通道必须真的能收到新事件（否则「重建」只是握手成功，帧仍在丢）
    let job_after_heal = plugin_command(
        "session.task.scheduled-create",
        serde_json::json!({
            "name": "cross-end-scheduled-after-heal",
            "config_id": config_id,
            "trigger_at": "2099-01-02 00:00:00",
            "prompts": ["cross-end-event-scheduled-after-heal"],
        }),
    )
    .await;
    events
        .wait_for("E-003a 自愈后新事件重新可达", |e| {
            matches!(e, MobileEvent::SyncTaskScheduledChanged { job_id, .. }
                if Some(job_id.as_str()) == job_after_heal["job_id"].as_str())
        })
        .await;

    // ==================== E-003b 认证类致命关闭：不自愈 + 不重放 + 对账补齐 ====================
    // 选 4003（而非普通断链）：M1/ADR 0031 定下「认证类致命关闭不自愈」，观察窗足够长、
    // 不会与毫秒级自愈抢跑，于是「断链期间的事件丢失」这条契约可以被确定性地验证。
    let before_fatal = desktop_ctx::wait_plugin_ws_endpoint_authenticated(
        SESSION_PLUGIN_ID,
        EVENT_CHANNEL_PATH,
        "E-003b 致命断链前的连接",
    )
    .await;
    let fatal_dropped = desktop_ctx::disconnect_plugin_ws_endpoint_clients(
        SESSION_PLUGIN_ID,
        EVENT_CHANNEL_PATH,
        AUTH_FATAL_CLOSE_CODE,
        "cross-end auth-fatal simulation",
    )
    .await;
    assert_eq!(fatal_dropped, 1, "E-003b 致命断链前事件通道上应恰好有一个在线连接");

    // 断链期间驱动一次真实的、会广播的状态迁移（停会话 → session:stopped）
    stop_session(&base, &token, &task_session, "E-003b 断链期间停会话").await;
    mobile_ctx::assert_no_mobile_event(
        &events,
        "E-003b 断链期间的事件不得落地",
        Duration::from_millis(800),
        |e| matches!(e, MobileEvent::SyncSessionStopped { session_id, .. } if session_id == &task_session),
    )
    .await;

    // 认证类致命关闭不得进入重连风暴（2026-09-29 实测 616 次/98 秒的放大器）
    desktop_ctx::assert_no_new_plugin_ws_client(
        SESSION_PLUGIN_ID,
        EVENT_CHANNEL_PATH,
        &before_fatal.client_id,
        "E-003b 认证类致命关闭后监督任务不得自愈重连",
        Duration::from_millis(1500),
    )
    .await;

    // 用户重新认证（生产里是重扫 QR / 重新配对）→ AuthSuccess → 监督任务重建通道
    get_auth_manager()
        .authenticate_with_token(&token)
        .await
        .expect("E-003b 重新认证（重新配对的等价路径）必须成功");
    desktop_ctx::wait_new_plugin_ws_endpoint_client(
        SESSION_PLUGIN_ID,
        EVENT_CHANNEL_PATH,
        &before_fatal.client_id,
        "E-003b 重新认证后重建事件通道",
    )
    .await;

    // 契约核心：重建后**不重放**断链期间漏掉的事件（事件不重放，缺口靠 HTTP 对账）
    mobile_ctx::assert_no_mobile_event(
        &events,
        "E-003b 重建后不得补发（重放）断链期间的事件",
        Duration::from_millis(800),
        |e| matches!(e, MobileEvent::SyncSessionStopped { session_id, .. } if session_id == &task_session),
    )
    .await;

    // 对账腿：漏掉的状态变化必须能靠 HTTP 全量拉取补齐
    // （这正是 `ws_event_channel_ready` 存在的原因——事件不重放，重连后必须对账）
    let mut reconciled = None;
    for _ in 0..100 {
        let list = list_sessions(&base, &token).await;
        if let Some(entry) = list.iter().find(|s| s["id"] == task_session) {
            let st = entry["status"].as_str().unwrap_or("").to_string();
            if st != "running" {
                reconciled = Some(st);
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        reconciled.as_deref(),
        Some("stopped"),
        "E-003b 断链期间漏掉的状态变化必须能靠 HTTP 全量拉取补齐（对账腿）"
    );

    // 重建后的通道必须真的能收到新事件
    let job_after_rebuild = plugin_command(
        "session.task.scheduled-create",
        serde_json::json!({
            "name": "cross-end-scheduled-after-rebuild",
            "config_id": config_id,
            "trigger_at": "2099-01-03 00:00:00",
            "prompts": ["cross-end-event-scheduled-after-rebuild"],
        }),
    )
    .await;
    events
        .wait_for("E-003b 重建后新事件重新可达", |e| {
            matches!(e, MobileEvent::SyncTaskScheduledChanged { job_id, .. }
                if Some(job_id.as_str()) == job_after_rebuild["job_id"].as_str())
        })
        .await;

    // ==================== 收尾 ====================
    remove_session(&base, &token, &task_session, "收尾移除会话").await;
    // 主动断开（生产同款：manual_disconnect 标记会让监督任务的自愈循环直接退出）——
    // 否则常驻事件通道会一直挂着，服务器的优雅停机会等它超时
    get_connection_manager().disconnect().await;
    mobile_ctx::clear_identity();
    desktop_ctx::stop_server(handle, server_task).await;
    desktop_ctx::cleanup_temp_dirs();
}
