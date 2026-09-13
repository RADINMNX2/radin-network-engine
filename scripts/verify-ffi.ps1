# scripts/verify-ffi.ps1 — real DLL load + symbol smoke test (no C toolchain).
# Usage: powershell -ExecutionPolicy Bypass -File scripts\verify-ffi.ps1
$ErrorActionPreference = 'Stop'
$Dll = Join-Path (Resolve-Path (Join-Path $PSScriptRoot '..')).Path 'dist\radin-win-x64\radin_ffi.dll'
if (-not (Test-Path $Dll)) { throw "DLL not found: $Dll (run scripts\package.ps1 first)" }

Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
public static class RadinFfiProbe {
    [DllImport("$($Dll.Replace('\','\\'))", CallingConvention = CallingConvention.Cdecl)]
    public static extern IntPtr radin_version();
}
"@
$ptr = [RadinFfiProbe]::radin_version()
$ver = [System.Runtime.InteropServices.Marshal]::PtrToStringAnsi($ptr)
if ($ver -notlike 'radin/*') { throw "radin_version returned unexpected value: '$ver'" }
Write-Host "OK: radin_ffi.dll loaded; radin_version() = $ver"