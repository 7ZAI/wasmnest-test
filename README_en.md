# wasmnest-test

**The WasmNest cross-end test repo — real mobile client code talking to a real desktop server, with zero protocol mocks.**

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-stable-DEA584?logo=rust&logoColor=white)](https://www.rust-lang.org)
[![wasmtime](https://img.shields.io/badge/wasmtime-48-654FF0?logo=webassembly&logoColor=white)](https://wasmtime.dev)
[![Zero mock](https://img.shields.io/badge/interop-zero%20mock-success)](#why-this-repo-exists)

English | [简体中文](README.md)

## Why This Repo Exists

Both ends already had tests, but their peers were **protocol-level mocks**: desktop tests used generic `reqwest` / `tokio-tungstenite` to play the mobile side, while mobile tests used a fake desktop server. Both mocks were self-consistent, so when docs or one implementation quietly drifted, both suites stayed green while real connectivity broke.

This repo puts both real paths on one rig:

```text
real mobile client code / real mobile wasm plugin artifact
        ⇅  real TCP (HTTP / WebSocket)
real desktop Actix server + real auth-center wasm plugin + real bash PTY
```

Zero protocol mocks, no adb, no WebView. Each scenario is a separate test binary (process isolation), because the desktop `AppContext` is a process-wide `OnceLock` singleton.

## Two Rigs

| Rig | Build mode | Description |
| --- | --- | --- |
| **Single process** | default (`desktop` feature) | Desktop lib, mobile lib and test code share one process; covers pairing, auth, session HTTP, event channel, failure paths, lifecycle, mDNS and more |
| **Two process** | `--no-default-features --features two-proc` | The desktop side runs in a separate `desktop-harness` process; the test process links only the mobile side. The desktop lib leaves the dependency graph, so host-domain implementations from both ends (registered process-wide via `inventory`) cannot overwrite each other |

The two-process rig is authoritative for terminal streaming: the single-process `terminal_ws_flow` is `#[ignore]`d because both host-domain implementations conflict, and real coverage lives in `two_proc_terminal_ws`.

Both rigs have a **self-check**: when a scenario goes red, first prove the rig is healthy (server ports installed through the same assembly point as GUI bootstrap, mobile target address round-trips, real plugins actually handle requests, and the port is released after shutdown).

## Scenarios and Locks

### Interop Scenarios

| Test | What it pins |
| --- | --- |
| `harness_selfcheck` | Rig wiring: target address, server port assembly, real HTTP round-trip, port ownership, plugin WS endpoint registration |
| `pairing_auth_flow` | Pairing / auth HTTP loop: a wrong code does not consume the valid code, token exchange, error-code mapping |
| `jwt_rotate_reconnect` | Admission-key rotation grace period: a token signed before rotation still verifies and reconnects afterwards |
| `session_http_flow` | Session HTTP surface from real mobile plugin to real desktop plugin; the host injects the JWT |
| `terminal_ws_flow` | Terminal stream, single-process shape (**ignored by default**, see above) |
| `two_proc_terminal_ws` | Terminal stream across processes: real bash PTY, output continuity, replay on resubscribe, backpressure and lossless narrow forwarding |
| `fail_closed_flow` | No registered center / invalid credentials always fail visibly; WS never goes live and emits zero output bytes |
| `lifecycle_flow` | Desktop plugin deactivate / activate: WS reclamation, endpoint unregistration, explicit mobile failure and rebuild |
| `event_channel_flow` | Desktop plugin broadcast → host WS endpoint → real mobile WS client → event router |
| `http_proxy_flow` | The mobile side's only HTTP path: Egress L1 + JWT injection + link-encryption envelope |
| `mdns_health_flow` | mDNS discovery / advertising + `/api/health` probing (the user's "find and connect to desktop" entry) |
| `two_proc_selfcheck` | Two-process assembly guard: the test process `inventory` contains only mobile modules; harness spawn / RPC / graceful shutdown |

### Contract Locks

| Lock | What it pins |
| --- | --- |
| `wire_drift_locks` | Wire vocabulary copies held by capability crates ↔ platform SDK originals, compared byte for byte |
| `permission_vocabulary_lock` | The single permission-vocabulary source ↔ five platform facts (generated artifact / validator / host / SDK) |
| `command_vocabulary_lock` | Lifecycle command names match on both ends, preventing "command not found" after a one-sided rename |
| `mdns_browse_singleton_lock` | Only the type hub may call `daemon.browse` per service type; no bypass |
| `bedcode_app_identity_lock` | BedCode's three identity exemptions and the id-family replacement boundary |
| `file_transfer_wiring_lock` | Dual-end single artifact: core logic cannot flow back and mobile cannot rebuild an artifact source |

## Performance Benchmarks

`perf-tests/` is an independent Cargo workspace that measures the platform bridge and the bundled real apps in release mode:

| Module | What it measures |
| --- | --- |
| `bridge` | Host ↔ wasm-plugin primitives and channels (migrated from the platform repo's `wasm_bridge_bench`) |
| `apps` | Real command paths of the four bundled apps (ai-chatbox / agent-hub / BedCode / file-transfer) |

```bash
cd perf-tests

cargo run --release --bin perf-harness            # smoke (seconds)
cargo run --release --bin perf-harness -- --full  # full matrix
cargo run --release --bin perf-harness -- --list  # scenario inventory
```

Scenario matrix, budgets, headless assembly semantics and `--compare` usage live in [`perf-tests/README.md`](perf-tests/README.md). Debug builds are refused by the entry point; the apps module requires prebuilt desktop app artifacts.

## Prerequisites

| Dependency | Notes |
| --- | --- |
| Four-repo layout | Platform, capability, apps and this repo as siblings; inside the project, cross-repo paths resolve via `../../wasmnest` and `../../wasmnest-lib` |
| App artifacts | Tests load the **real bundled wasm artifacts**. Build both ends first by running `pnpm run plugins:build` once in each of `wasmnest-desktop` and `wasmnest-mobile` |
| Rust / sccache | Rust stable; `.cargo/config.toml` forces `sccache`, so cargo fails immediately when it is missing |
| Terminal environment | Terminal scenarios use a real `bash` PTY and need a Unix-like environment |
| App dependencies | Before building app artifacts, every app project in the apps repo needs `pnpm install` |
| mDNS environment | `mdns_health_flow` requires **no second WasmNest desktop instance running on the same machine** (identical instance name `WasmNest-<hostname>` → mDNS conflict and discovery-cache overwrite, which makes the scenario racily flaky on a shared dev host). See that test's header comment for the root cause and the isolation recipe |

Non-standard layouts can use environment overrides:

| Variable | Purpose |
| --- | --- |
| `WASMNEST_PLATFORM_ROOT` | Platform repo root (default `../wasmnest`) |
| `WASMNEST_LIB_ROOT` | Capability repo root (default `../wasmnest-lib`) |
| `WASMNEST_APPS_ROOT` | Apps repo root (default `../wasmnest-apps`) |

All resolvers fail visibly: a missing directory throws / panics instead of returning a possibly invalid path.

## Running

```bash
cd cross-end-tests

# All single-process scenarios (terminal_ws_flow is skipped by design)
cargo test

# One scenario
cargo test --test pairing_auth_flow
cargo test --test wire_drift_locks

# Two-process shape (build the desktop harness first, then run two-proc tests)
cargo build -p wasmnest-crossend-desktop-harness
cargo test --no-default-features --features two-proc

# One two-process scenario
cargo test --no-default-features --features two-proc --test two_proc_terminal_ws
```

**Recommended triage order**: run `harness_selfcheck` (or `two_proc_selfcheck`) first to prove the rig is trustworthy, then look at the failing scenario. Rig problems masquerading as protocol bugs are the most common misdiagnosis in this repo's history.

## Repository Layout

```text
wasmnest-test/
├─ cross-end-tests/                # The workspace itself (independent Cargo workspace)
│  ├─ Cargo.toml                   # Main package + workspace root + two build modes
│  ├─ src/lib.rs                   # Crate-level notes
│  ├─ tests/                       # Scenario tests + contract locks (one binary per scenario)
│  │  ├─ common/                   # desktop_ctx / mobile_ctx / mobile_plugin / repo_paths
│  │  ├─ two_proc_common/          # Two-process driver + reused mobile-side fixtures
│  │  └─ *.rs
│  ├─ desktop-harness/             # Desktop process for the two-process rig (stdin/stdout JSON-RPC)
│  └─ rustfmt.toml
├─ perf-tests/                     # Performance benchmarks (independent Cargo workspace)
│  ├─ src/modules/{bridge,apps}.rs # Bridge benchmarks + real-app benchmarks
│  ├─ src/support/                 # Host assembly / temp HOME / app fixture env / RSS
│  ├─ docs/adr/                    # Performance engineering decisions
│  ├─ GLOSSARY.md                  # Performance-context vocabulary
│  └─ README.md                    # Scenarios / budgets / usage
├─ .cargo/config.toml              # sccache + build resource limits
├─ AGENTS.md                       # Navigation and shared conventions
└─ LICENSE
```

## Cross-repo Boundaries

- Both hosts are referenced through sibling paths: `../../wasmnest/{wasmnest-desktop,wasmnest-mobile}/src-tauri`. Capability crates must be read through `../../wasmnest-lib/<crate>` so there is exactly one copy in the dependency graph (type identity).
- Desktop assembly must match GUI bootstrap: the port surface goes through the host composition root's `server::composition::install_server_ports()`. Without ports, the HTTP gateway silently falls back to `PassThrough` and every plugin endpoint returns 404 — it looks like a protocol bug, but the rig is simply miswired.
- The locks in this repo are part of the delivery surface: never delete one, narrow its glob or turn an assertion into a tautology. Update stale allowlists in the same commit.
- When changing cross-end wire formats, permission vocabulary or lifecycle command names, the corresponding locks in this repo, the platform repo and the capability repo must all go green together.

## Documentation and Contributing

| Need | Entry point |
| --- | --- |
| Navigation and hard constraints | [`AGENTS.md`](AGENTS.md) |
| Scenario behavior contracts | Each test file's header (source / behavior / case for every assertion) |
| Shared conventions | Platform repo `docs/public/` (git / comments / testing / versioning / issues) |
| Platform architecture and cross-end protocols | [wasmnest/docs/architecture.md](https://github.com/7ZAI/wasmnest/blob/main/docs/architecture.md) |
| Capability contract sources | [wasmnest-lib](https://github.com/7ZAI/wasmnest-lib) |
| Apps repo | [wasmnest-apps](https://github.com/7ZAI/wasmnest-apps) |

Before committing, run the scenarios and locks that match the changed surface, and report what ran, what did not, and the risk. There is no CI; verification means actually running locally. Cross-session work is tracked in [GitHub Issues](https://github.com/7ZAI/wasmnest-test/issues).

## Versioning

Early development, version line `0.0.1`. This repo is tagged independently (`test-vX.Y.Z`) and does not participate in product assembly; its version is identification only.

## License

[MIT](LICENSE) © 2026 7ZAI
