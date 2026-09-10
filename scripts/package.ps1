# Assembles a self-contained distribution folder.
#
# The player links FFmpeg's shared libraries, so a runnable release is
# "executable + its seven DLLs" — no installer, no registry setup, no runtime
# to bootstrap. This script builds the release binary, copies it next to those
# DLLs, and drops in the documentation.
#
# Usage:  .\scripts\package.ps1 [-SkipBuild] [-Output <dir>]

[CmdletBinding()]
param(
    [switch] $SkipBuild,
    [string] $Output = (Join-Path (Split-Path -Parent $PSScriptRoot) 'dist\MVP-Versatile-Player')
)

$ErrorActionPreference = 'Continue'
if (Get-Variable -Name PSNativeCommandUseErrorActionPreference -ErrorAction SilentlyContinue) {
    $PSNativeCommandUseErrorActionPreference = $false
}

$root = Split-Path -Parent $PSScriptRoot
$exeName = 'mvp-versatile-player.exe'
$releaseDir = Join-Path $root 'target\release'

if (-not $SkipBuild) {
    Write-Host '构建发布版本…' -ForegroundColor Cyan
    & (Join-Path $PSScriptRoot 'dev.ps1') build --release -p mvp-player
    if ($LASTEXITCODE -ne 0) {
        throw "release build failed (exit $LASTEXITCODE)"
    }
}

$exe = Join-Path $releaseDir $exeName
if (-not (Test-Path $exe)) {
    throw "找不到 $exe，请先运行构建。"
}

if (Test-Path $Output) { Remove-Item $Output -Recurse -Force }
New-Item -ItemType Directory -Force -Path $Output | Out-Null

Copy-Item $exe (Join-Path $Output $exeName)

# The FFmpeg shared libraries the executable loads at start-up.
$ffmpegDir = $env:FFMPEG_DIR
if (-not $ffmpegDir) { $ffmpegDir = 'C:\ffmpeg-dev\ffmpeg-n9.0-latest-win64-gpl-shared-9.0' }
$dlls = Get-ChildItem (Join-Path $ffmpegDir 'bin') -Filter *.dll
foreach ($dll in $dlls) {
    Copy-Item $dll.FullName (Join-Path $Output $dll.Name)
}

# Documentation and licensing.
foreach ($file in @('README.md', 'LICENSE')) {
    $path = Join-Path $root $file
    if (Test-Path $path) { Copy-Item $path (Join-Path $Output $file) }
}
# FFmpeg's own licence text is required when redistributing the binaries.
$ffmpegLicense = Join-Path $ffmpegDir 'LICENSE.txt'
if (Test-Path $ffmpegLicense) {
    Copy-Item $ffmpegLicense (Join-Path $Output 'LICENSE-FFmpeg.txt')
}

$total = (Get-ChildItem $Output -Recurse -File | Measure-Object -Property Length -Sum).Sum
$exeSize = (Get-Item (Join-Path $Output $exeName)).Length

Write-Host ''
Write-Host ("打包完成: {0}" -f $Output) -ForegroundColor Green
Write-Host ("  可执行文件 : {0:N2} MB" -f ($exeSize / 1MB))
Write-Host ("  FFmpeg DLL : {0} 个" -f $dlls.Count)
Write-Host ("  合计       : {0:N1} MB" -f ($total / 1MB))
Write-Host ''
Write-Host '将这个目录整体拷贝到目标机器即可运行，无需安装。' -ForegroundColor DarkGray
Write-Host '首次使用请在「工具 → 文件关联」中点击「注册文件关联」。' -ForegroundColor DarkGray
