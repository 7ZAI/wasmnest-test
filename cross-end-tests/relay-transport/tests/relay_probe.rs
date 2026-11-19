//! T19 真实中继上的**只读连通性探测**（`wasmnest-p2p-engine::probe`）。
//!
//! ## 为什么放这里
//!
//! `wasmnest-p2p-engine` 的边界锁禁止 dev-dependencies 出现任何 `wasmnest*` 内部
//! crate（通用引擎不能为测试长出内部依赖边），所以「引擎探测 ↔ 真实 `wasmnest-relay`
//! 服务端」的相遇只能落在本测试工程（与 `relay_reservation_flow` 同一夹具）。
//!
//! ## 判据链
//!
//! 1. 进程内起真实中继（同一份 `server::start`）→ 探测成功：报告地址 / PeerId /
//!    指纹（与中继身份独立复算值逐字相等）/ 握手耗时 / RTT；中继侧
//!    `connections_total` 至少 1（证明真连上了，不是本地假阳性）。
//! 2. 把地址尾部的 `/p2p/<relay>` 换成另一个合法 PeerId → `IdentityMismatch`
//!    （安全事件：同 IP 不同身份必须显性拒绝，且不当作「不可达」）。
//! 3. 关闭端口 → `Unreachable`，且逐候选点名地址。
//!
//! 只读语义（不建 reservation / 不登记规则）由 `probe` 模块结构保证：它没有
//! `P2pPorts` 可写、没有管理器句柄，本用例额外以中继指标确认「只有一条连接、
//! 零预留」。

use std::net::TcpListener;
use std::path::Path;
use std::time::Duration;

use wasmnest_p2p_engine::identity_bridge::NodeKey;
use wasmnest_p2p_engine::probe::{probe_relay, ProbeError};
use wasmnest_p2p_engine::Multiaddr;
use wasmnest_relay::config::{IdentityConfig, ListenConfig, MetricsConfig, RelayConfig};
use wasmnest_relay::identity::RelayIdentity;
use wasmnest_relay::server::{self, RelayRuntime};

/// 单次探测预算：环回握手 + 首个 ping 远小于此；留足 CI 抖动余量
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

#[tokio::test(flavor = "multi_thread")]
async fn 进程内中继_探测成功_身份指纹与_rtt_齐全() {
    let dir = tempfile::tempdir().expect("临时数据目录");
    let identity = RelayIdentity::generate();
    let expected_fingerprint = identity.fingerprint();
    let relay = start_in_process_relay(dir.path(), identity).await;
    let addr = relay
        .advertise_addrs()
        .first()
        .cloned()
        .expect("中继应通告至少一条可拨地址");

    let report = probe_relay(&[addr.clone()], PROBE_TIMEOUT)
        .await
        .expect("真实中继上的探测必须成功（端口可达 + 身份一致）");

    assert_eq!(report.addr, addr, "报告应指回实际探测成功的地址");
    assert_eq!(report.peer_id, relay.peer_id, "握手实测 PeerId 必须是中继身份");
    assert_eq!(
        report.peer_id_sha256, expected_fingerprint,
        "指纹口径必须与 wasmnest-relay 的 peer_id_sha256 一致（可独立复算）"
    );
    assert!(
        report.handshake < PROBE_TIMEOUT,
        "握手耗时应在预算内：{:?}",
        report.handshake
    );
    assert!(
        report.rtt.is_some(),
        "中继支持 ping，首个往返必须能取到 RTT"
    );

    let snapshot = relay.metrics.snapshot();
    assert!(
        snapshot.connections_total >= 1,
        "中继侧应记录到探测连接（否则不是真连）：{snapshot:?}"
    );
    assert_eq!(
        snapshot.reservations_active, 0,
        "探测不得建立 reservation（只读判据）：{snapshot:?}"
    );

    relay.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn 多地址_首项失败次项成功_返回次项() {
    let dir = tempfile::tempdir().expect("临时数据目录");
    let relay = start_in_process_relay(dir.path(), RelayIdentity::generate()).await;
    let good = relay
        .advertise_addrs()
        .first()
        .cloned()
        .expect("中继应通告至少一条可拨地址");
    // 同一 PeerId、另一个未监听端口：按序回退时不得把失败当结论
    let dead: Multiaddr = format!(
        "/ip4/127.0.0.1/tcp/{}/p2p/{}",
        free_tcp_port(),
        relay.peer_id
    )
    .parse()
    .expect("端点形状");

    let report = probe_relay(&[dead, good.clone()], PROBE_TIMEOUT)
        .await
        .expect("首项失败后应回退到次项并成功");
    assert_eq!(report.addr, good, "报告应指回真正成功的候选");
    assert_eq!(report.peer_id, relay.peer_id);

    relay.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn 地址钉住身份不符_报身份不符而不是不可达() {
    let dir = tempfile::tempdir().expect("临时数据目录");
    let relay = start_in_process_relay(dir.path(), RelayIdentity::generate()).await;
    let addr = relay
        .advertise_addrs()
        .first()
        .cloned()
        .expect("中继应通告至少一条可拨地址");

    // 同 IP / 同端口，只把钉住的 PeerId 换掉：不能退化成「拨号失败」
    let impostor = NodeKey::generate().peer_id();
    let tampered: Multiaddr = format!(
        "{}/p2p/{impostor}",
        addr.to_string()
            .rsplit_once("/p2p/")
            .expect("通告地址必须带 /p2p/<relay>")
            .0
    )
    .parse()
    .expect("篡改后的地址仍应是合法 multiaddr");

    let err = probe_relay(&[tampered], PROBE_TIMEOUT)
        .await
        .expect_err("身份不符必须失败");
    assert!(
        matches!(err, ProbeError::IdentityMismatch(_)),
        "同地址不同身份应归为身份不符（安全事件），实得：{err:?}"
    );

    relay.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn 端口未监听_报不可达且点名地址() {
    // 取一个刚释放的回环端口（无监听）
    let port = {
        let listener = TcpListener::bind("127.0.0.1:0").expect("取空闲端口");
        listener.local_addr().expect("读端口").port()
    };
    let peer = NodeKey::generate().peer_id();
    let addr: Multiaddr = format!("/ip4/127.0.0.1/tcp/{port}/p2p/{peer}")
        .parse()
        .expect("端点形状");

    let err = probe_relay(&[addr], Duration::from_secs(3))
        .await
        .expect_err("没有监听时必须失败");
    assert!(
        matches!(err, ProbeError::Unreachable(_)),
        "关闭端口应归为不可达：{err:?}"
    );
    assert!(
        err.to_string().contains(&port.to_string()),
        "失败明细应点名候选地址与端口：{err}"
    );
}

/// 起一个真实中继（与公网部署同一份 `server::start`），回环端口 + 临时身份
async fn start_in_process_relay(dir: &Path, identity: RelayIdentity) -> RelayRuntime {
    let port = free_tcp_port();
    let config = RelayConfig {
        identity: IdentityConfig {
            key_file: dir.join("identity.key"),
        },
        listen: ListenConfig {
            addrs: vec![format!("/ip4/127.0.0.1/tcp/{port}")],
        },
        // 指标端点绑 0 端口：默认 9090 会与本机中继 / 并行用例撞车
        metrics: MetricsConfig {
            listen: "127.0.0.1:0".into(),
            expose_public: false,
        },
        ..Default::default()
    };
    server::start(config, &dir.join("config.toml"), identity)
        .await
        .expect("中继启动失败（身份 / 配置 / 端口三者之一，见 RelayError）")
}

/// 绑定后立即释放：竞态窗口极小，被抢占时用例会以「中继启动失败」显性红
fn free_tcp_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("绑定临时端口");
    listener.local_addr().expect("读取临时端口地址").port()
}
