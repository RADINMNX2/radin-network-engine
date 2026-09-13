# radin-network-engine

RADIN Network Engine — measurement-honest network-resiliency core (Rust):
pure decision engine, transport adapters, TEST-ONLY chaos harness, CLI demo,
reference edge-control server, FFI surface, and mobile scaffolds.

> **Honesty first.** Every measurement this project prints is either REAL
> telemetry (production path, `MeasuredSource::Real`) or EXPLICITLY stamped
> SYNTHETIC harness data (radin-chaos). Nothing is faked or relabelled. See
> [`docs/HONESTY.md`](docs/HONESTY.md).

## Workspace layout

| Crate | Role |
|---|---|
| `radin-core` | Pure decision engine: scoring, health grading, fallback chain, hysteresis, circuit breaker, retry, keepalive, mobility, telemetry. No I/O. |
| `radin-transport` | Transport adapters (QUIC, UDP, TCP/TLS, h2, KCP, WebSocket) with framing + capability model. |
| `radin-chaos` | **TEST-ONLY** synthetic network pipeline (loss/jitter/reset matrix) used by demos and tests. `synthetic_never_exports`. |
| `radin-server` | Reference edge-control API (axum). Read-only control plane; idempotent `POST /v1/benchmark` + `POST /v1/telemetry`. |
| `radin-cli` | Scripted fail-over DEMO driving a real `RouteEngine` through the chaos harness; prints SYNTHETIC data only. |
| `radin-ffi` | C ABI (`cdylib` + `rlib`) for Flutter/Dart (`dart:ffi`) consumers; `cbindgen.toml` ready. |

Mobile scaffolds (`android/`, `app/`, `proto/`) are tracked but **not built in
this repository's CI** — they require external toolchains (Flutter, Android
SDK, protoc) that the reference build deliberately does not depend on.

## Build from source

Requires Rust 1.85+ (see `rust-toolchain.toml`).

```powershell
cargo fmt --all
cargo clippy --workspace --all-features --all-targets --locked -- -D warnings
cargo test --workspace --all-features --locked        # 144 tests
cargo build --release --locked -p radin-cli -p radin-server -p radin-ffi
```

## Package (Windows x64)

```powershell
powershell -ExecutionPolicy Bypass -File scripts\package.ps1
```

Creates `dist\radin-win-x64\` (binaries, `radin.h`, README, SHA256SUMS) plus
`dist\radin-win-x64.zip`. Artifacts are NOT code-signed — verify SHA-256 hashes
or build from source.

## Use the packaged binaries

```powershell
cd dist\radin-win-x64

# CLI demo (SYNTHETIC harness data — labelled as such)
.\radin-cli.exe --scenario smoke --report plain
.\radin-cli.exe --scenario full  --report plain
.\radin-cli.exe --scenario smoke --report json   # report.source == "Synthetic"

# Reference server
.\radin-server.exe --bind 127.0.0.1 --port 8787
```

### radin-cli flags

| Flag | Meaning |
|---|---|
| `--scenario <full\|smoke>` | full = 4-phase A-D timeline (default); smoke = short CI-friendly pass |
| `--report <plain\|json>` | plain = human-readable (default); json = machine-readable on stdout, markers on stderr |
| `--ms <ms>` | tick interval in ms between probe rounds (default 200) |
| `--rounds <n>` | override probe rounds in every phase |
| `--version`, `--help` | version / usage |

Exit codes: `0` ok, `2` usage error, `101` runtime panic.

### radin-server endpoints

| Endpoint | Description |
|---|---|
| `GET /health` | Liveness + minimal load |
| `GET /v1/edges` | Signed/config edge list (reference edges seeded) |
| `GET /v1/config` | Optimization config defaults |
| `POST /v1/benchmark` | Benchmark metadata ingestion (`Idempotency-Key`) |
| `POST /v1/telemetry` | Telemetry ingestion (`Idempotency-Key`) |

Idempotency: a repeated `Idempotency-Key` returns `{"accepted":0}`.

## Documentation

- [`docs/RUNBOOK.md`](docs/RUNBOOK.md) — exact commands for build, package, CLI,
  server, FFI probe, with real captured outputs and PS 5.1 traps
- [`docs/HONESTY.md`](docs/HONESTY.md) — synthetic/real measurement policy
- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — architecture
- [`docs/CI.md`](docs/CI.md) — continuous integration
- [`docs/SPEC_TRACEABILITY.md`](docs/SPEC_TRACEABILITY.md) — spec mapping
- [`docs/TEST_MATRIX.md`](docs/TEST_MATRIX.md) — chaos test matrix

## License

Apache-2.0.