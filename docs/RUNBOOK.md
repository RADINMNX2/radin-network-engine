# RUNBOOK — RADIN Network Engine v0.1.0

Windows x64, PowerShell 5.1. Every command below was executed against the v0.1.0
build from source (`main` @ release). Outputs shown are the REAL captured
responses (recorded 2026-09-14).

---

## 1. Build from source

```powershell
cargo fmt --all                                          # format
cargo fmt --check                                        # verify clean
cargo clippy --workspace --all-features --all-targets --locked -- -D warnings
cargo test --workspace --all-features --locked           # 144 tests, all green
```

Release artifacts (CLI demo, reference server, FFI cdylib):

```powershell
powershell -ExecutionPolicy Bypass -File scripts\package.ps1
```

`package.ps1` runs `cargo build --release --locked -p radin-cli -p radin-server
-p radin-ffi`, then writes `dist\radin-win-x64\` (binaries + `radin.h` + README +
SHA256SUMS) and zips it to `dist\radin-win-x64.zip`. If `cbindgen` is not on
`PATH`, a hand-maintained fallback `radin.h` is written instead (keep it in sync
with `crates/radin-ffi/src/lib.rs` exports manually).

## 2. Packaged artifacts

| File | Purpose | Size (bytes) |
|---|---|---|
| `radin-cli.exe` | Scripted fail-over DEMO. Prints **SYNTHETIC** harness measurements only (spec 39/41). | 360,960 |
| `radin-server.exe` | Reference edge control API (axum). Read-only control plane; in-memory idempotency ledger; never holds user traffic. | 2,136,576 |
| `radin_ffi.dll` | C ABI surface for Flutter/Dart (`dart:ffi`) consumers. | 349,696 |
| `radin_ffi.dll.lib` | MSVC import library for C consumers linking the DLL. | 5,260 |
| `radin.h` | C header (cbindgen or hand-maintained fallback). | 1,629 |

`dist\radin-win-x64\README.txt` and `SHA256SUMS.txt` are generated at package
time with real SHA-256 hashes.

## 3. radin-cli

```powershell
cd dist\radin-win-x64
.\radin-cli.exe --scenario smoke --report plain     # CI-friendly pass (4 phases)
.\radin-cli.exe --scenario full  --report plain     # full 4-phase timeline
.\radin-cli.exe --scenario smoke --report json      # JSON report on stdout
.\radin-cli.exe --help
.\radin-cli.exe --version                            # radin-cli 0.1.0
```

Real captured excerpt (`--scenario smoke --report plain`, exit 0):

```
╭──────────────────────────────────────────────────────────╮
│ RADIN NETWORK ENGINE — scripted fail-over demonstration  │
│  All measurements are SYNTHETIC (radin-chaos harness).    │
│  No production telemetry is involved. (spec 39/41)        │
╰──────────────────────────────────────────────────────────╯
  route=Edge-SG-01  grade=GOOD    health_worse-than-excellent=true rtt=12.0ms loss=0.00%
  route=Edge-TYO-01 grade=DEGRADED ...
  fail_safe_action=Direct may_forward_direct=true fallback_current=Direct
╭────────────────────── DIAGNOSTIC REPORT ───────────────────╮
RADIN Network Diagnostic Report
...
Selected Edge:          Edge-SG-01-quic
...
╰─────────────────────────────────────────────────────────────╯
```

JSON mode validation captured:

- Phase markers go to **stderr**; stdout is purely machine-readable JSON.
- Wrapper is `{"harness":"radin-chaos","measurements":"SYNTHETIC","report":{...}}`.
- The engine stamps `report.source = "Real"` internally (it assumes production
  feeders); radin-cli KNOWS the run is harness-fed and overrides it:
  `report.source == "Synthetic"`. JSON consumers see the truth.

Exit codes: `0` ok, `2` usage error, `101` runtime panic.

## 4. radin-server (reference API)

```powershell
cd dist\radin-win-x64
.\radin-server.exe --bind 127.0.0.1 --port 8787     # default port is 8787
```

If 8787 is busy, override: `--port 8799`.

Real captured responses (`curl.exe -s`, Windows):

```
GET /health   -> {"status":"ok","region":"reference","version":"0.1.0","load":0.0,"timestamp":...}
GET /v1/edges -> {"edges":[{"address":"edge-asia-1.radin.example:443",...},
                            ... 4 reference edges ...],
                  "issued_at":..., "protocol_version":1}
GET /v1/config -> {"score_weights":{"latency":0.30,"jitter":0.20,"loss":0.30,
                                    "stability":0.15,"handshake":0.05},
                   "fallback_order":["quic","udp","tcp_tls","http2","websocket_tls","direct"],
                   "fail_safe":{"allow_direct_fallback":true}}
```

`POST /v1/benchmark` and `POST /v1/telemetry` are idempotent via
`Idempotency-Key`. Contract captured live:

```powershell
# Valid sample body (all four BenchmarkSample fields are required):
#   transport, rtt_ms, jitter_ms, loss_ratio
# PowerShell 5.1 STRIPS the double quotes from '{"...":...}' native-arg bodies,
# so pass the JSON via a file to keep the quotes intact:
$tmp = Join-Path $env:TEMP 'bench.json'
Set-Content -Path $tmp -Value '{"version":"0.1.0","region":"test","samples":[{"transport":"quic","rtt_ms":12.0,"jitter_ms":1.5,"loss_ratio":0.0}]}' -Encoding ascii
curl.exe -s -X POST -H "Idempotency-Key: k2" -H "Content-Type: application/json" --data-binary "@$tmp" http://127.0.0.1:8787/v1/benchmark
# -> {"accepted":1}
curl.exe -s -X POST -H "Idempotency-Key: k2" -H "Content-Type: application/json" --data-binary "@$tmp" http://127.0.0.1:8787/v1/benchmark
# -> {"accepted":0}   (duplicate key — idempotency ledger reply)
```

Recorded duplicate behavior: a repeated `Idempotency-Key` returns
`{"accepted":0}` regardless of whether the first attempt succeeded or failed
(parse-error outcomes are also recorded in the ledger, so a retried bad body
with the same key is answered as a duplicate).

## 5. FFI smoke test (no C toolchain required)

```powershell
powershell -ExecutionPolicy Bypass -File scripts\verify-ffi.ps1
# real output:
# OK: radin_ffi.dll loaded; radin_version() = radin/0.1.0
```

The script loads `dist\radin-win-x64\radin_ffi.dll` with P/Invoke
(`CallingConvention.Cdecl`), calls `radin_version()`, and asserts the answer
starts with `radin/`.

## 6. Honesty notes

- **SYNTHETIC stamps.** radin-cli's measurements come exclusively from the
  TEST-ONLY `radin-chaos` harness. The banner, the JSON wrapper
  (`"measurements":"SYNTHETIC"`, `report.source="Synthetic"`), and this runbook
  all say so. See `docs/HONESTY.md`.
- **Unsigned binaries.** The v0.1.0 artifacts are NOT code-signed. Windows
  SmartScreen may warn on first run. Do not disable security — verify hashes
  instead (below) or build from source.
- **SHA-256 verification.** `dist\radin-win-x64\SHA256SUMS.txt` and
  `dist\SHA256SUMS.txt` contain real hashes. Recompute with
  `Get-FileHash .\radin-cli.exe -Algorithm SHA256`. The zip hash is printed by
  `package.ps1`.
- **Always `curl.exe`.** On Windows, bare `curl` is an alias for
  `Invoke-WebRequest`; its syntax and default output differ. Use `curl.exe`.
- **UTF-16 redirection trap (PS 5.1).** `> file` / `Out-File` write UTF-16 by
  default, which breaks tools expecting UTF-8. Use `Out-File -Encoding ascii`
  or `[System.Text.UTF8Encoding]::new($false)` for machine-readable output, or
  capture to a variable and pipe to `ConvertFrom-Json`.
- **Native-arg quoting trap (PS 5.1).** Passing `'{"a":1}'` to a native exe
  strips the inner double quotes. For JSON bodies to `curl.exe`, write the body
  to a file and use `--data-binary "@file"`.
- **Port override.** `--port 8799` if 8787 is taken.
- **No production telemetry passes through this reference build.** radin-server
  never ingests payloads, credentials, or user traffic; radin-cli never emits
  it.

## 7. CI

The `check` + `msrv` GitHub Actions jobs run fmt, clippy (`-D warnings`,
`--all-targets`), and the full test suite on push. See `docs/CI.md`.