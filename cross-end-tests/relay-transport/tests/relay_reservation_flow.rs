//! 真实中继端到端：`wasmnest-relay` 服务端 ↔ `wasmnest-p2p-engine` 客户端。
//!
//! ## 补的是什么缺口（此前两侧各自成立、从未相遇）
//!
//! - `wasmnest-relay` 自带单测只测配置 / 准入 / 指标，从未被真实客户端预留；
//! - `wasmnest-p2p-engine` 的 `relay_flow` 用**裸 libp2p relay behaviour** 当服务端，
//!   没走 `wasmnest-relay` 的身份 / 配额 / 准入代码路径；
//! - `wasmnest-server-peer-net` 的隧道用例用 fake engine（`reserve_relay` 打桩）。
//!
//! ## 两个用例
//!
//! 1. `进程内中继_预留_经中继拨号_往返`：同进程起 `wasmnest-relay::server::start`
//!    （临时身份 / 临时配置 / 回环端口），零外部依赖，CI 可跑；
//! 2. `已部署中继_预留_经中继拨号_往返`：`#[ignore]` + `WASMNEST_RELAY_E2E_ADDRS`
//!    指向真实公网中继（逗号分隔的端点 multiaddr），验证部署形态。
//!    显式 `--ignored` 运行时缺环境变量直接 panic（不静默跳过）。
//!
//! ## 判据链
//!
//! 服务端接受预留 → 拨号经 `/p2p-circuit`（`Transport::WanRelay`）→ 具名流
//! 64 KiB 往返逐字一致；进程内用例额外核对中继侧指标（活跃预留 1 条 /
//! 累计 circuit ≥ 1），证明流量真的过了中继而不是退回直连。

use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::io::{AsyncReadExt, AsyncWriteExt};

use wasmnest_p2p_engine::engine::{P2pEngine, P2pNodeConfig};
use wasmnest_p2p_engine::identity_bridge::{NodeId, NodeKey};
use wasmnest_p2p_engine::ports::{
    DialFailed, P2pEventSink, P2pPorts, PeerAdmissionPort, PeerConnected, PeerDenied, PeerDisconnected,
};
use wasmnest_p2p_engine::relay::{RelayEndpoint, RelayState, Transport};
use wasmnest_p2p_engine::stream_proto::SESSION;
use wasmnest_p2p_engine::{Multiaddr, PeerId};
use wasmnest_relay::config::{IdentityConfig, ListenConfig, MetricsConfig, RelayConfig};
use wasmnest_relay::identity::RelayIdentity;
use wasmnest_relay::metrics::Metrics;
use wasmnest_relay::server::{self, RelayRuntime};

/// 单步等待上限：环回 < 1s，公网中继留足 TCP + Noise 握手与预留往返
const STEP_TIMEOUT: Duration = Duration::from_secs(30);

/// 往返载荷：跨多个 yamux 帧（默认窗口 256 KiB 以内），能发现截断 / 乱序，
/// 又不把中继带宽配额当压力测试
const PAYLOAD_BYTES: usize = 64 * 1024;

#[tokio::test(flavor = "multi_thread")]
async fn 进程内中继_预留_经中继拨号_往返() {
    let dir = tempfile::tempdir().expect("创建临时数据目录");
    let relay = start_in_process_relay(dir.path()).await;

    let endpoint = relay
        .advertise_addrs()
        .first()
        .cloned()
        .expect("中继应通告至少一条地址（回环监听时回退为监听地址）");
    assert!(
        endpoint.to_string().ends_with(&format!("/p2p/{}", relay.peer_id)),
        "通告地址应钉住中继身份：{endpoint}"
    );

    relayed_round_trip(endpoint, Some(relay.metrics.as_ref())).await;
    relay.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "需要 WASMNEST_RELAY_E2E_ADDRS（真实公网中继端点）；cargo test -p relay-transport -- --ignored 显式运行"]
async fn 已部署中继_预留_经中继拨号_往返() {
    let raw = std::env::var("WASMNEST_RELAY_E2E_ADDRS")
        .expect("显式运行已部署中继用例时必须设置 WASMNEST_RELAY_E2E_ADDRS（逗号分隔的 relay 端点 multiaddr）");
    let addrs: Vec<Multiaddr> = raw
        .split(',')
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| {
            text.parse()
                .unwrap_or_else(|e| panic!("WASMNEST_RELAY_E2E_ADDRS 里的地址非法（{text}）：{e}"))
        })
        .collect();
    let endpoint = addrs.first().cloned().expect("WASMNEST_RELAY_E2E_ADDRS 不能为空");

    relayed_round_trip(endpoint, None).await;
}

/// A 预留 → B 经 `/p2p-circuit` 拨 A → SESSION 流 64 KiB 往返。
///
/// `relay_metrics` 给出时在收发完成后、关停之前核对中继侧计数：`a.shutdown()`
/// 会释放预留，快照必须早于它。
async fn relayed_round_trip(endpoint: Multiaddr, relay_metrics: Option<&Metrics>) {
    let (a, mut streams) = P2pEngine::start_with_protocols(NodeKey::generate(), loopback_config(), ports(), &[SESSION])
        .expect("启动预留方节点");

    let reserved = a
        .reserve_relay(vec![endpoint.clone()])
        .await
        .expect("中继应接受预留（checklist：端口可达 / 准入模式 / 单 peer 配额）");
    assert_eq!(
        reserved, endpoint,
        "预留回执应等于钉住的 relay 端点（引擎不得改写 PeerId）"
    );
    assert_eq!(
        a.relay_status().state,
        RelayState::Reserved,
        "预留成功后状态应为 relay-reserved"
    );

    let b = P2pEngine::start(NodeKey::generate(), loopback_config(), ports()).expect("启动拨号方节点");
    let dial = RelayEndpoint::parse(&endpoint, None)
        .expect("端点须为 `.../tcp/<port>/p2p/<relay>`（带 /p2p-circuit 的是拨号地址，不是端点）")
        .dial_addr(a.peer_id());
    let outcome = b
        .dial_addrs(&[dial], a.node_id().clone(), STEP_TIMEOUT)
        .await
        .expect("经中继拨号应成功");
    assert_eq!(
        outcome.transport,
        Transport::WanRelay,
        "含 /p2p-circuit 的地址必须判为 wan-relay：{}",
        outcome.addr
    );

    // 非零变化字节：能发现截断 / 乱序 / 全零填充
    let payload: Vec<u8> = (0..PAYLOAD_BYTES).map(|i| (i % 251) as u8).collect();
    let expect = payload.clone();
    let mut incoming = streams
        .remove(SESSION)
        .expect("SESSION 接收器应在 start_with_protocols 登记");
    let echo = tokio::spawn(async move {
        let mut inbound = incoming.next().await.expect("中继上应收到业务流");
        let mut buf = vec![0u8; expect.len()];
        inbound.stream.read_exact(&mut buf).await.expect("读满载荷");
        assert_eq!(buf, expect, "经中继的载荷应逐字一致");
        inbound.stream.write_all(&buf).await.expect("回写载荷");
        inbound.stream.flush().await.expect("flush");
    });

    let mut stream = b.open_stream(a.node_id(), SESSION).await.expect("经中继开流");
    stream.write_all(&payload).await.expect("写载荷");
    stream.flush().await.expect("flush");
    let mut buf = vec![0u8; payload.len()];
    stream.read_exact(&mut buf).await.expect("读回载荷");
    assert_eq!(buf, payload, "往返载荷应逐字一致");
    echo.await.expect("echo 任务不应 panic");

    if let Some(metrics) = relay_metrics {
        let snapshot = metrics.snapshot();
        assert_eq!(
            snapshot.reservations_active, 1,
            "中继侧应看到 1 条活跃预留：{snapshot:?}"
        );
        assert!(
            snapshot.circuits_total >= 1,
            "中继侧应记录至少 1 条 circuit（否则流量没走中继）：{snapshot:?}"
        );
    }

    a.shutdown().await;
    b.shutdown().await;
}

/// 起一个真实中继进程（与公网部署同一份 `server::start`）。
async fn start_in_process_relay(dir: &Path) -> RelayRuntime {
    let port = free_tcp_port();
    let config = RelayConfig {
        identity: IdentityConfig {
            key_file: dir.join("identity.key"),
        },
        listen: ListenConfig {
            addrs: vec![format!("/ip4/127.0.0.1/tcp/{port}")],
        },
        // 指标端点显式绑 0 端口：默认 127.0.0.1:9090 会与本机中继 / 并行用例撞车
        metrics: MetricsConfig {
            listen: "127.0.0.1:0".into(),
            expose_public: false,
        },
        ..Default::default()
    };
    let identity = RelayIdentity::generate();
    server::start(config, &dir.join("config.toml"), identity)
        .await
        .expect("中继启动失败（身份 / 配置 / 端口三者之一，见 RelayError）")
}

/// 取一个当前空闲的回环端口：中继配置需要具体端口（通告地址必须可拨）。
///
/// 与 `tests/peer_harness.rs` 的占位 listener 同思路——绑定后立即释放，
/// 竞态窗口极小；端口被抢占时用例会以「中继启动失败」显性红。
fn free_tcp_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("绑定临时端口");
    listener.local_addr().expect("读取临时端口地址").port()
}

fn loopback_config() -> P2pNodeConfig {
    P2pNodeConfig {
        listen_addr: IpAddr::from([127, 0, 0, 1]),
        ..Default::default()
    }
}

fn ports() -> P2pPorts {
    P2pPorts {
        admission: Arc::new(AdmitAll),
        events: Arc::new(SilentSink),
    }
}

/// 首连即放行：本用例只验传输面，信任确认语义由隧道 / peer 用例覆盖
struct AdmitAll;

#[async_trait]
impl PeerAdmissionPort for AdmitAll {
    async fn confirm_inbound(&self, _node_id: &NodeId, _peer_id: &PeerId) -> bool {
        true
    }
}

/// 静默事件出口：用例只读 `relay_status()` 与中继侧指标，不消费连接事件
struct SilentSink;

impl P2pEventSink for SilentSink {
    fn listening_changed(&self, _addrs: Vec<SocketAddr>) {}

    fn peer_connected(&self, _ev: PeerConnected) {}

    fn peer_disconnected(&self, _ev: PeerDisconnected) {}

    fn peer_denied(&self, _ev: PeerDenied) {}

    fn dial_failed(&self, _ev: DialFailed) {}
}
