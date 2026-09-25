# Assembles a self-contained distribution folder and the update zip.
#
# The player links FFmpeg's shared libraries, so a runnable release is
# "executable + its seven DLLs" — no installer, no registry setup, no runtime
# to bootstrap. This script builds the release binary, copies it next to those
# DLLs, drops in the documentation, and — together with the external updater —
# packs the whole folder into the flat zip the update service hands out.
#
# The zip is what gets uploaded to the MVP Update Manager, and the
# version / file_size / sha256 it prints at the end are exactly what the
# "create version" form asks for. The zip is flat (every file at its root):
# the updater reads the archive's own central directory as the file list.
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

. "$PSScriptRoot\ffmpeg-env.ps1"

$root = $MvpRepoRoot
$exeName = 'mvp-versatile-player.exe'
$releaseDir = Join-Path $root 'target\release'

if (-not $SkipBuild) {
    Write-Host '构建发布版本…' -ForegroundColor Cyan
    & (Join-Path $PSScriptRoot 'dev.ps1') build --release -p mvp-player
    if ($LASTEXITCODE -ne 0) {
        throw "release build failed (exit $LASTEXITCODE)"
    }
    # The external updater, built as its own binary. `--features apply` is what
    # actually selects the `[[bin]]`; without it only the library is built.
    Write-Host '构建外置更新器…' -ForegroundColor Cyan
    & (Join-Path $PSScriptRoot 'dev.ps1') build --release -p mvp-updater --features apply
    if ($LASTEXITCODE -ne 0) {
        throw "updater build failed (exit $LASTEXITCODE)"
    }
}

$exe = Join-Path $releaseDir $exeName
if (-not (Test-Path $exe)) {
    throw "找不到 $exe，请先运行构建。"
}

if (Test-Path $Output) { Remove-Item $Output -Recurse -Force }
New-Item -ItemType Directory -Force -Path $Output | Out-Null

Copy-Item $exe (Join-Path $Output $exeName)

# The external updater rides along in the same folder: the player launches
# `mvp-updater.exe` from beside itself to apply an update.
$updaterExe = Join-Path $releaseDir 'mvp-updater.exe'
if (-not (Test-Path $updaterExe)) {
    throw "找不到 $updaterExe，请先运行构建（不带 -SkipBuild）。"
}
Copy-Item $updaterExe (Join-Path $Output 'mvp-updater.exe')

# The FFmpeg shared libraries the executable loads at start-up. The same
# helper the other scripts use, so a kit that was moved or re-downloaded is
# picked up without editing anything.
$ffmpegDir = Get-MvpFfmpegDir
if (-not $ffmpegDir) {
    throw "找不到 FFmpeg 开发包，请先运行 .\scripts\setup-ffmpeg.ps1"
}
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
# ---- update zip ------------------------------------------------------------
#
# One flat zip of the whole folder is the release artifact: the update service
# stores it and hands out its URL, and the updater unpacks it straight into the
# install directory. `includeBaseDirectory = $false` keeps every file at the
# zip root, which is what the updater's file list assumes.
$version = $null
if ((Get-Content (Join-Path $root 'Cargo.toml') -Raw) -match '(?m)^version\s*=\s*"([^"]+)"') {
    $version = $Matches[1]
}
if (-not $version) {
    throw '无法从 Cargo.toml 读取版本号。'
}

$distRoot = Split-Path -Parent $Output
$zipPath = Join-Path $distRoot ("MVP-Versatile-Player{0}.zip" -f $version)
if (Test-Path $zipPath) { Remove-Item $zipPath -Force }

Add-Type -AssemblyName System.IO.Compression.FileSystem
[System.IO.Compression.ZipFile]::CreateFromDirectory(
    $Output,
    $zipPath,
    [System.IO.Compression.CompressionLevel]::Optimal,
    $false
)

$zipItem = Get-Item $zipPath
$sha256 = (Get-FileHash $zipPath -Algorithm SHA256).Hash.ToLower()

Write-Host ''
Write-Host ('更新包已生成: {0}' -f $zipPath) -ForegroundColor Green
Write-Host '将下面三行填写到更新管理后台的「创建版本」表单：' -ForegroundColor Cyan
Write-Host ("  version   : {0}" -f $version)
Write-Host ("  file_size : {0}" -f $zipItem.Length)
Write-Host ("  sha256    : {0}" -f $sha256)
Write-Host ''
Write-Host '将这个目录整体拷贝到目标机器即可运行，无需安装。' -ForegroundColor DarkGray
Write-Host '首次使用请在「工具 → 文件关联」中点击「注册文件关联」。' -ForegroundColor DarkGray
