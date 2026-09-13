# RADIN Network Engine — Architecture

A network-resiliency optimizer for the RADIN Android VPN: it measures every
candidate path, keeps a health model, switches routes only when sustained
evidence demands, and fails safe to direct traffic when every tunnel is down —
without ever sacrificing user data, credentials, or honest numbers.

## Planes (spec 21 — control/data separation)

- **Control plane** (`radin-core` + `radin-server`): routing *decisions*,
  config, edge discovery/health, telemetry, session state, optimization policy.
  Pure logic over measured inputs. No socket code in the core.
- **Data plane** (`radin-transport` + `android/` scaffold): opaque IP
  forwarding over the selected transport. Game traffic is never decrypted,
  modified, or interpreted; the data plane only reports what it actually
  observed on the wire and follows the engine's current route.

```
  control plane                         data plane
  ┌────────────────────┐                ┌─────────────────────┐
  │ RouteEngine        │  selected      │ VpnService / TUN    │
  │  detect→health     │  route ──────► │  (Kotlin, build-    │
  │  →score→hysteresis │                │   required scaffold)│
  │  →fallback→circuit │  measurements  │  radin-transport    │
  │  →telemetry        │ ◄───────────── │  (QUIC/UDP/TCP/… )  │
  └────────┬───────────┘                └─────────────────────┘
           │
           └── radin-ffi (dart:ffi / Flutter), radin-cli,
               radin-server (control-plane API)
```

## Workspace crates

| Crate            | Role                                                              |
|------------------|-------------------------------------------------------------------|
| `radin-core`     | Pure decision engine + all models (the "brain"). No sockets.       |
| `radin-transport`| Feature-gated transport adapters (quic, h2, ws, udp/tcp/kcp…).     |
| `radin-chaos`    | **TEST-ONLY** packet-fate pipeline for fault injection.            |
| `radin-server`   | Stateless control-plane HTTP API (axum).                          |
| `radin-cli`      | Local demo/bench runner (scripted fail-over timeline).            |
| `radin-ffi`      | C ABI surface for Flutter/Dart (cbindgen + dart:ffi).              |

## Core modules (`crates/radin-core/src`)

- `detect` — symptom detection (timeout, QUIC handshake failure, degraded…).
- `metrics` — the honesty gate: `ensure_real` rejects synthetic measurements
  from every production sink.
- `edges` — edge registry (id, region, address, supported transports,
  priority, expiry, signature).
- `scoring` — `score_all` ranks candidate routes; synthetic configs rejected.
- `health` — `NetworkHealthGrade` with manual ordering (Excellent ≻ … ≻
  Critical) and `HealthTracker` (sustained upgrades, immediate degradations).
- `hysteresis` — the switch rules: no switching on a single lost packet;
  `metrics_are_degraded`, `candidate_is_falling`, `current_route_degraded`.
- `fallback` — chain-of-transports engine; walks current→…; `Direct` is the
  terminal tier at the end of the chain.
- `circuit` — per-route breakers; `record_failure(now)`, `CircuitState::Open`
  expires and re-probes (half-open).
- `engine` — `RouteEngine`: owns the loop `on_edge_observation`,
  `report_transport_failure/success`, `set_edges`, `set_network_type`,
  `set_vpn_state`, `diagnostic_report`, `fail_safe_action`,
  `may_forward_direct`.
- `telemetry` — `TelemetryBuffer` (capacity-bounded samples+events) and
  `DiagnosticBuilder` + `DiagnosticReport::render_plaintext`.
- `compression` / `delta` / `migration` / `sync` — config sync helpers.
- `keepalive` / `probe` / `retry` / `security` / `session` / `state` /
  `control` / `persistence` — supporting subsystems.

## Engine pipeline (detect → measure → adapt → recover → fail over)

1. Probes/observations land in `on_edge_observation` with live edge stats.
2. `RouteEngine` scores current vs. candidates; hysteresis decides
   *(no switch on 1 lost packet; sustained degradation switches; small-delta
   keeps current)*.
3. Health degrades immediately, upgrades only after a sustained window.
4. Transport failures march the fallback chain; breakers open and count
   `circuit_trips`.
5. With every tunnel saturated, `fail_safe_action()` → `Direct`; a
   StrictVpnOnly data plane honors the `may_forward_direct()` gate (no leak).
6. `diagnostic_report(now)` renders the truth as plaintext/JSON — real
   measurements only.

## Test layout

- `radin-core` unit tests per module (110), feature tests (sqlite, zstd).
- `crates/radin-core/tests/failover_scenarios.rs` — 9 engine-level integration
  scenarios (fast-path stability, sustained switch, clean Direct failover,
  StrictVpnOnly leak guard, recovery, breaker quarantine, spikes, honest
  telemetry, restrictive network).
- `crates/radin-chaos/tests/test_matrix.rs` — spec-38 parameter sweep over the
  chaos pipeline.
- `radin-transport` adapter tests incl. a real QUIC Dev handshake.
- `radin-ffi` ABI round-trip tests (JSON config in → report JSON out).

See `docs/TEST_MATRIX.md`, `docs/HONESTY.md`, and `docs/SPEC_TRACEABILITY.md`.