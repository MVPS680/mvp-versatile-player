# Runs any cargo command with the FFmpeg runtime on PATH.
#
# The player links FFmpeg's shared libraries, so anything that *runs* a binary
# (`cargo run`, `cargo test`) needs `<ffmpeg>/bin` on PATH — the import libraries
# are found at link time through FFMPEG_DIR, but the DLLs are loaded at start-up.
# The packaged player ships those DLLs next to the executable and does not need
# this script; it exists purely to make `cargo test` work from a dev checkout.
#
# A fresh clone has no `.cargo/config.toml` (it is generated per machine and
# ignored by git — see `ffmpeg-env.ps1`), so this script prepares it on the way
# past. `-NoDownload` there keeps a stray `dev.ps1 test` from silently pulling a
# gigabyte of SDK: with nothing on the disk it stops and says what to run.
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

. "$PSScriptRoot\ffmpeg-env.ps1"
$root = $MvpRepoRoot

if (-not (Test-Path $MvpConfigPath)) {
    Write-Host '尚未准备本机的 FFmpeg / libclang 配置，先运行一次 setup-ffmpeg.ps1…' -ForegroundColor Cyan
    try {
        & (Join-Path $PSScriptRoot 'setup-ffmpeg.ps1') -NoDownload
    } catch {
        Write-Host $_.Exception.Message -ForegroundColor Red
        Write-Host '请先运行： .\scripts\setup-ffmpeg.ps1' -ForegroundColor Yellow
        exit 1
    }
}

# The kit itself: whatever the helper can find, which is also what cargo will
# link against through `.cargo/config.toml`.
$ffmpegDir = Get-MvpFfmpegDir
if (-not $ffmpegDir) {
    Write-Warning "找不到 FFmpeg 开发包；请先运行 .\scripts\setup-ffmpeg.ps1"
} else {
    $env:PATH = "$(Join-Path $ffmpegDir 'bin');$env:PATH"
}

# Forward slashes, exactly as `scripts/setup-ffmpeg.ps1` writes it into
# `.cargo/config.toml`. `ffmpeg-sys-next` declares
# `cargo:rerun-if-env-changed=FFMPEG_DIR`, so a value that differs only in its
# separators would make every switch between `cargo run` and this script look
# like a configuration change and re-run bindgen (a multi-minute rebuild).
#
# Only when there is a kit to point at: exporting an empty value would *set*
# the variable (`force = false` never overrides a set variable) and hide the
# one cargo wrote, turning a clear "kit missing" error into a confusing one.
if ($ffmpegDir) {
    $env:FFMPEG_DIR = $ffmpegDir -replace '\\', '/'
}
Push-Location $root
try {
    & cargo @CargoArgs
    exit $LASTEXITCODE
} finally {
    Pop-Location
}
