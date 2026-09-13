# scripts/package.ps1 — build release artifacts, generate dist/ with hashes.
# Usage: powershell -ExecutionPolicy Bypass -File scripts\package.ps1 [-SkipBuild]
param([switch]$SkipBuild)
$ErrorActionPreference = 'Stop'

$Root    = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$Rel     = Join-Path $Root 'target\release'
$DistDir = Join-Path $Root 'dist\radin-win-x64'
$DistZip = Join-Path $Root 'dist\radin-win-x64.zip'

if (-not $SkipBuild) {
    Push-Location $Root
    cargo build --release --locked -p radin-cli -p radin-server -p radin-ffi
    if ($LASTEXITCODE -ne 0) { throw 'cargo build --release failed' }
    Pop-Location
}

$Required = @(
    (Join-Path $Rel 'radin-cli.exe'),
    (Join-Path $Rel 'radin-server.exe'),
    (Join-Path $Rel 'radin_ffi.dll'),
    (Join-Path $Rel 'radin_ffi.dll.lib')
)
foreach ($f in $Required) {
    if (-not (Test-Path $f)) {
        throw "Missing required artifact: $f`n(radin-server.exe requires Phase 2 bin target; radin_ffi.dll requires Phase 4 crate-type cdylib.)"
    }
}

New-Item -ItemType Directory -Force -Path $DistDir | Out-Null

# --- C header: prefer cbindgen, else embedded fallback (kept in sync manually) ---
$cb = Get-Command cbindgen -ErrorAction SilentlyContinue
if ($cb) {
    cbindgen --config (Join-Path $Root 'crates\radin-ffi\cbindgen.toml') `
             --crate radin-ffi --output (Join-Path $DistDir 'radin.h')
    if ($LASTEXITCODE -ne 0) { throw 'cbindgen failed' }
} else {
    Write-Host 'cbindgen not found — writing fallback radin.h (see MISSION dossier §A.1)'
    $header = @'
/* radin.h — FALLBACK header (cbindgen unavailable at package time).
   Prefer the cbindgen-generated header; this one is hand-maintained and must
   stay in sync with crates/radin-ffi/src/lib.rs exports. */
#include <stdint.h>
#include <stdlib.h>

typedef struct RadinEngine RadinEngine;

#define RADIN_ERR_OK 0
#define RADIN_ERR_NULL -1
#define RADIN_ERR_PARSE -2
#define RADIN_ERR_BAD_TRANSPORT -3
#define RADIN_ERR_PANIC -4

const char *radin_version(void);
RadinEngine *radin_engine_new(const char *config_json, size_t config_len);
void radin_engine_free(RadinEngine *handle);
size_t radin_engine_last_error(const RadinEngine *handle, char *buf, size_t cap);
int32_t radin_engine_set_edges(RadinEngine *handle, const char *edges_json, size_t edges_len, uint64_t now_ms);
int32_t radin_engine_set_network(RadinEngine *handle, int32_t network, uint64_t now_ms);
int32_t radin_engine_set_vpn(RadinEngine *handle, int32_t vpn, uint64_t now_ms);
int32_t radin_engine_observe(RadinEngine *handle, const char *edge_id, size_t edge_id_len, double latency_ms, double jitter_ms, int32_t lost, uint64_t now_ms);
int32_t radin_engine_report_transport_failure(RadinEngine *handle, int32_t transport, uint64_t now_ms);
int32_t radin_engine_report_transport_success(RadinEngine *handle, int32_t transport, uint64_t now_ms);
int32_t radin_engine_diagnostic_json(RadinEngine *handle, uint64_t now_ms, char **out);
int32_t radin_engine_current_route_json(RadinEngine *handle, char **out);
int32_t radin_engine_fail_safe(RadinEngine *handle);
int32_t radin_engine_may_forward_direct(RadinEngine *handle);
void radin_free_string(char *ptr);
'@
    Set-Content -Path (Join-Path $DistDir 'radin.h') -Value $header -Encoding ascii
}

# --- Copy artifacts ---
Copy-Item (Join-Path $Rel 'radin-cli.exe')        $DistDir
Copy-Item (Join-Path $Rel 'radin-server.exe')     $DistDir
Copy-Item (Join-Path $Rel 'radin_ffi.dll')        $DistDir
Copy-Item (Join-Path $Rel 'radin_ffi.dll.lib')    $DistDir

# --- README with real hashes + honesty stamp ---
$files = Get-ChildItem $DistDir -File
$sums  = foreach ($f in $files) { "$((Get-FileHash $f.FullName -Algorithm SHA256).Hash)  $($f.Name)" }
$readme = @"
RADIN Network Engine v0.1.0 — Windows x64 package (BUILT FROM SOURCE)
====================================================================
Contents
--------
  radin-cli.exe      Scripted fail-over DEMO (SYNTHETIC harness measurements).
  radin-server.exe   Reference edge control API (axum). Read-only control
                     plane; holds no user traffic.
  radin_ffi.dll      C ABI surface for Flutter/Dart (dart:ffi) consumers.
  radin_ffi.dll.lib  MSVC import library (for C consumers linking this DLL).
  radin.h            C header (cbindgen or hand-maintained fallback).

Honesty (docs/HONESTY.md)
-------------------------
  * radin-cli prints SYNTHETIC harness data only; no production telemetry.
  * radin-server is a reference control API with an in-memory idempotency
    ledger; it never sees payloads, credentials or user data.
  * These binaries are NOT code-signed; Windows SmartScreen may warn. Verify
    the SHA-256 hashes below against the CI build or local build from source
    (cargo build --release --locked -p radin-cli -p radin-server -p radin-ffi).

SHA-256 (SHA256SUMS.txt also included)
--------------------------------------
$($sums -join "`n")

Run
---
  .\radin-cli.exe --scenario full --report plain
  .\radin-server.exe --bind 127.0.0.1 --port 8787
  (then: curl.exe http://127.0.0.1:8787/health)
"@
Set-Content -Path (Join-Path $DistDir 'README.txt') -Value $readme -Encoding ascii
$sums | Set-Content -Path (Join-Path $DistDir 'SHA256SUMS.txt') -Encoding ascii

# --- Zip + hash the zip ---
if (Test-Path $DistZip) { Remove-Item $DistZip -Force }
Compress-Archive -Path $DistDir -DestinationPath $DistZip -Force
$zipHash = (Get-FileHash $DistZip -Algorithm SHA256).Hash
"zip  $DistZip" | Out-File -FilePath (Join-Path $Root 'dist\SHA256SUMS.txt') -Encoding ascii
Add-Content -Path (Join-Path $Root 'dist\SHA256SUMS.txt') -Value $sums

Write-Host "Packaged: $DistZip"
Write-Host "SHA256:   $zipHash"