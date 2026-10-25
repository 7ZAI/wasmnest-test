//! 场景 1：配对 / 认证闭环（HTTP 面，真实往返）
//!
//! 覆盖的盲区：**探测 / 配对面的双向契约失真**。桌面端既有测试用 reqwest
//! 造请求、移动端既有测试用假桌面服务器造应答——两套 mock 各自自洽。本文件
//! 里请求字节由**移动端真实客户端**生成、应答字节由**桌面真实插件**生成。
//!
//! ## 行为契约（unit-test-discipline G1：每条有来源）
//!
//! | 契约 | 来源 | 行为 | 场景 |
//! |---|---|---|---|
//! | C-001 | `AuthHttpClient::request_pairing` + 插件 `handle_pairing` | 移动端发 camelCase body → 桌面回 `{code:0,data:{pairingCode,expiresIn}}` → 移动端反序列化出非空码 | 正例 |
//! | C-002 | 插件 `pair_code_verify` 不等值不清码 | 错配对码 → 业务码 1005 → 移动端 `AppError::Auth`；**且有效码未被消耗**（随后仍能换到 token） | 反例 + 副作用 |
//! | C-003 | 插件 `handle_verify` | 有效码 → 签发 JWT → 移动端 `AuthTokenResponseData.token` 非空、`expiresIn > 0` | 正例 |
//! | C-004 | 插件 `handle_reauth`（ADR 0033 中心自签自验） | 真实 token 回炉 → 验签通过 → 换发；跨秒后 `iat` 变化 → token 必须真的换新 | 正例 |
//! | C-005 | 同上 | 篡改 token → 桌面拒绝 → 移动端 `AppError::Auth`（fail-closed，无降级） | 反例 |
//! | C-006 | 插件 `handle_qr_connect` + `qr_verify` 一次性 | 桌面生成 QR（`qr-code-generate` 互调，即桌面 UI 的同一入口）→ 移动端 `qr_connect` 换到 token | 正例 |
//! | C-007 | 同上一次性语义 | 同一 QR token 二次使用 → 拒绝（1006） | 反例 |
//! | C-008 | 插件 `handle_biometric_challenge` | 未绑定生物凭证 → 1008 → 移动端 `AppError::Auth`（端点存在且失败显性） | 反例 |
//!
//! QR 与生物凭证的**正向**路径不在本文件：QR 的「桌面扫码确认」与生物凭证的
//! 移动端私钥（Android Keystore）都在无头进程外（见 issue 03 记录）。

mod common;

use wasmnest_mobile_lib::auth::http::{AuthHttpClient, DeviceAuthContext};
use wasmnest_mobile_lib::system::error::AppError;

use common::desktop_ctx;
use common::mobile_ctx;

/// 断言业务码出现在错误消息里（移动端把桌面业务码透传进 `AppError::Auth`）
fn assert_business_code(err: &AppError, code: u32, what: &str) {
    match err {
        AppError::Auth(msg) => assert!(
            msg.contains(&code.to_string()),
            "{what}：错误消息应携带桌面业务码 {code}，got: {msg}"
        ),
        other => panic!("{what}：应映射为 AppError::Auth（桌面业务拒绝），got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pairing_auth_round_trip_against_real_desktop() {
    if tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::DEBUG)
        .try_init()
        .is_err()
    {
        tracing::debug!("tracing subscriber already initialized");
    }

    desktop_ctx::init_app_context().await;
    let (port, handle, server_task) = desktop_ctx::start_server().await;
    mobile_ctx::set_target(port).await;

    let auth = AuthHttpClient::new();
    let base = mobile_ctx::base_url(port);
    let ctx = DeviceAuthContext {
        device_id: "crossend-device-1",
        device_name: "CrossEnd Phone",
        fingerprint: "crossend-fp-1",
        uid_hash: Some("crossend-uid-1"),
    };
    let address = format!("127.0.0.1:{port}");

    // ==================== C-001 发起配对（移动端请求字节 → 桌面真实插件） ====================
    let pairing = auth
        .request_pairing(&base, ctx.device_id, ctx.device_name, ctx.fingerprint)
        .await
        .expect("C-001 移动端必须能从桌面真实插件拿到配对码");
    assert!(!pairing.pairing_code.is_empty(), "C-001 配对码不得为空");
    assert!(pairing.expires_in > 0, "C-001 有效期须为正，got {}", pairing.expires_in);

    // ==================== C-002 错配对码 → 1005，且不消耗有效码 ====================
    let wrong = auth
        .verify_pairing_code(&base, ctx, "000000-WRONG-CODE", &address)
        .await
        .expect_err("C-002 错码必须被桌面拒绝");
    assert_business_code(&wrong, 1005, "C-002 错配对码");

    // ==================== C-003 有效码换 JWT ====================
    let verified = auth
        .verify_pairing_code(&base, ctx, &pairing.pairing_code, &address)
        .await
        .expect("C-003 C-002 的错误尝试不得消耗有效配对码，有效码仍应换得到 token");
    assert!(!verified.token.is_empty(), "C-003 token 不得为空");
    assert!(verified.expires_in > 0, "C-003 token 有效期须为正");

    // ==================== C-004 token 回炉换发（桌面确实认这个 token） ====================
    let reauthed = auth
        .reauth(&base, ctx.device_id, ctx.fingerprint, ctx.uid_hash, &verified.token)
        .await
        .expect("C-004 桌面必须能验签自己刚签的 token（中心自签自验闭环）");
    assert!(!reauthed.token.is_empty(), "C-004 换发 token 不得为空");
    // 同一秒内 reauth 会得到**逐字节相同**的 token：JWT `iat` 是秒级粒度，
    // claims 不变 → 签名不变。这不是「没换」，下面这条才是真判据。
    auth.reauth(&base, ctx.device_id, ctx.fingerprint, ctx.uid_hash, &reauthed.token)
        .await
        .unwrap_or_else(|e| panic!("C-004 换发后的 token 必须同样可复验，got {e}"));
    // 跨过秒边界再换发：claims 变了 → token 必须真的不同。
    // 这条杀掉「reauth 只是把入参 token 原样吐回来」的变异（那样上面那条恒过）。
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let after_tick = auth
        .reauth(&base, ctx.device_id, ctx.fingerprint, ctx.uid_hash, &reauthed.token)
        .await
        .expect("C-004 跨秒重认证");
    assert_ne!(
        after_tick.token, reauthed.token,
        "C-004 跨秒重认证必须换出新 token（claims 的 iat 变了）；相同说明 reauth 是原样回显入参"
    );

    // ==================== C-005 篡改 token → 拒绝（fail-closed） ====================
    let mut tampered = reauthed.token.clone();
    tampered.push('x'); // 结构破坏：签名/结构校验必拒
    let rejected = auth
        .reauth(&base, ctx.device_id, ctx.fingerprint, ctx.uid_hash, &tampered)
        .await
        .expect_err("C-005 篡改 token 必须被拒绝（不得降级放行）");
    assert_business_code(&rejected, 1001, "C-005 篡改 token");

    // ==================== C-006 QR 正向：桌面生成 → 移动端换 token ====================
    let qr = desktop_ctx::plugin_api_call(
        "com.wasmnest.bedcode.qr-code-generate",
        // params = 入参值本身（api 声明为 `ttl: u64`），不是 {ttl:300}
        serde_json::json!(300u64),
    )
    .await;
    let qr_token = qr["token"].as_str().expect("C-006 桌面应生成 QR token").to_string();
    assert!(!qr_token.is_empty(), "C-006 QR token 不得为空");
    let qr_auth = auth
        .qr_connect(&base, ctx, &qr_token, &address)
        .await
        .unwrap_or_else(|e| panic!("C-006 移动端 qr_connect 必须成功，got {e}"));
    assert!(!qr_auth.token.is_empty(), "C-006 QR 认证必须签发 token");
    // 换到的 QR token 同样可复验（与配对码签发的 token 同域）
    auth.reauth(&base, ctx.device_id, ctx.fingerprint, ctx.uid_hash, &qr_auth.token)
        .await
        .unwrap_or_else(|e| panic!("C-006 QR 签发的 token 必须可复验，got {e}"));

    // ==================== C-007 QR 一次性：二次使用被拒 ====================
    let reused = auth
        .qr_connect(&base, ctx, &qr_token, &address)
        .await
        .expect_err("C-007 已消费的 QR token 不得二次使用");
    assert_business_code(&reused, 1006, "C-007 QR 二次使用");

    // ==================== C-008 未绑定生物凭证的挑战 → 1008 ====================
    let unpaired_fp = "crossend-fp-never-bound";
    let challenge = auth
        .biometric_challenge(&base, ctx.device_id, unpaired_fp)
        .await
        .expect_err("C-008 未绑定生物凭证的设备不得拿到挑战值");
    assert_business_code(&challenge, 1008, "C-008 未绑定生物凭证");

    // ==================== 收尾 ====================
    mobile_ctx::clear_identity();
    desktop_ctx::stop_server(handle, server_task).await;
    desktop_ctx::cleanup_temp_dirs();
}
