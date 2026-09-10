# Runs any cargo command with the FFmpeg runtime on PATH.
#
# The player links FFmpeg's shared libraries, so anything that *runs* a binary
# (`cargo run`, `cargo test`) needs `<ffmpeg>/bin` on PATH — the import libraries
# are found at link time through FFMPEG_DIR, but the DLLs are loaded at start-up.
# The packaged player ships those DLLs next to the executable and does not need
# this script; it exists purely to make `cargo test` work from a dev checkout.
#
# Usage:  .\scripts\dev.ps1 test -p mvp-core
#         .\scripts\dev.ps1 run -p mvp-player --release

[CmdletBinding()]
param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]] $CargoArgs
)

$ErrorActionPreference = 'Continue'
# Cargo writes progress and warnings to stderr; PowerShell 7.4+ would otherwise
# turn those into terminating errors and swallow the real exit code.
if (Get-Variable -Name PSNativeCommandUseErrorActionPreference -ErrorAction SilentlyContinue) {
    $PSNativeCommandUseErrorActionPreference = $false
}

$root = Split-Path -Parent $PSScriptRoot
$ffmpegDir = $env:FFMPEG_DIR
if (-not $ffmpegDir) {
    $ffmpegDir = 'C:\ffmpeg-dev\ffmpeg-n9.0-latest-win64-gpl-shared-9.0'
}

$binDir = Join-Path $ffmpegDir 'bin'
if (-not (Test-Path $binDir)) {
    Write-Warning "FFmpeg bin directory not found at '$binDir'; binaries may fail to start."
} else {
    $env:PATH = "$binDir;$env:PATH"
}

$env:FFMPEG_DIR = $ffmpegDir
Push-Location $root
try {
    & cargo @CargoArgs
    exit $LASTEXITCODE
} finally {
    Pop-Location
}
