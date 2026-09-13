# Spec → Implementation Traceability

Best-effort map from the 41 specification items to code, tests, and docs.
Spec-referencing identifiers appear verbatim in the codebase (e.g. `spec 41`
in `radin-core/src/telemetry.rs` and `metrics.rs`).

| Spec | Deliverable | Where                                      | Verification |
|------|-------------|--------------------------------------------|--------------|
| 1-3  | Workspace + Honesty contract            | `Cargo.toml`, `docs/HONESTY.md`            | `MetricsSource` gates            |
| 4-5  | Detect + symptom classification         | `radin-core/src/detect.rs`                 | unit tests                     |
| 6-7  | Measure + edge registry                 | `radin-core/src/edges.rs`                  | unit tests                     |
| 8    | Health grades + tracker                 | `radin-core/src/health.rs`                 | `upgrades_require_sustain…`    |
| 9-10 | Scoring (stable beats fast)             | `radin-core/src/scoring.rs`                | `stable_fast_edges_win…`       |
| 11-13| Fallback chain incl. Direct terminal    | `radin-core/src/fallback.rs`               | `fallback_ends_at_direct`      |
| 14   | Circuit breakers per transport          | `radin-core/src/circuit.rs`                | `opens_after…` / `half_open…`  |
| 15-17| Hysteresis rules                        | `radin-core/src/hysteresis.rs`             | `no_switch_on_single_loss…`    |
| 18-19| Engine loop + policy gates              | `radin-core/src/engine.rs`                 | `failover_scenarios.rs`        |
| 20   | Data-plane prohibition                  | `docs/ARCHITECTURE.md`, `android/` scaffold| None (a transport concern, no sockets in core) |
| 21   | Control/data plane separation           | crate split core vs transport/server       | architecture doc               |
| 22   | Keepalive, probe cadence                | `radin-core/src/{keepalive,probe}.rs`      | unit tests                     |
| 23-24| Health recovery (sustained)             | `health.rs` + scenario 5                    | `recovery_after_outage…`       |
| 25-27| Retry policy, reconnects                | `radin-core/src/retry.rs`                  | unit tests                     |
| 28   | State machine / transitions             | `radin-core/src/state.rs`                  | unit tests                     |
| 29   | Session management                      | `radin-core/src/session.rs`                | unit tests                     |
| 30   | Diagnostic report (plaintext)           | `telemetry.rs::render_plaintext`, `radin-cli` | CLI run + FFI test            |
| 31-33| Config sync, migrations, delta patches  | `radin-core/src/{sync,migration,delta}.rs` | unit tests                     |
| 34   | Compression (zstd, brotli)              | `radin-core/src/compression.rs`            | feature-gated tests            |
| 35   | Security (edge signatures, dev bootstrap) | `radin-core/src/security.rs`              | security unit tests            |
| 36   | Control plane + idempotency             | `radin-core/src/control.rs`, `radin-server`| `in_memory_store_dedupes…`     |
| 37   | Stateless server                        | `radin-server` (axum routes)               | `cargo build`                  |
| 38   | Chaos test matrix                       | `radin-chaos` + `tests/test_matrix.rs`     | 6 matrix tests                 |
| 39   | Demo/bench                              | `radin-cli/src/main.rs`                    | `cargo run -p radin-cli`       |
| 40   | Flutter↔Rust bridge                     | `radin-ffi` + `app/lib/ffi/radin_ffi.dart` | FFI round-trip tests (**Dart side is a build-required scaffold**) |
| 41   | Honesty audit / no fabrication          | `docs/HONESTY.md`                          | grep procedure in §5           |

## Explicit build-required scaffold items

These exist as source in the tree but **cannot be compiled in this workspace**
(no Flutter SDK, Android SDK/NDK, or protoc):

- `android/` — `RadinVpnService.kt` (VpnService + TUN data-plane loop).
- `app/` — Flutter UI (`main.dart`, `pubspec.yaml`, `ffi/radin_ffi.dart`).
- `proto/` — `radin_control.proto` (needs `protoc` for generated bindings).

Each file carries an inline `BUILD-REQUIRED SCAFFOLD` notice so nobody mistakes
an unbuilt scaffold for a verified artifact (honesty applies to process too).