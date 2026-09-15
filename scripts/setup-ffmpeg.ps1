# Prepares the machine-local build inputs this project links against, and
# records them in `.cargo/config.toml`.
#
# Two things have to exist before the workspace can build:
#
#   * an FFmpeg 9.0 **shared** development kit — `include/`, `lib/*.lib` and
#     `bin/*.dll`. The player links the import libraries and copies the DLLs next
#     to the executable, so a static build is not usable.
#   * `libclang`, which `ffmpeg-sys-next` loads through bindgen.
#
# Neither path is a property of the project, so the result goes into the
# repo-local `.cargo/config.toml` — which is ignored by git. That keeps local
# paths out of everyone else's `git pull`, and makes this script the only thing
# that ever has to know where the SDK lives.
#
# Re-running it is safe and cheap: an already-unpacked kit and an already-found
# libclang are reused, and the config is only rewritten when it really changed.
#
# Usage:  .\scripts\setup-ffmpeg.ps1 [-FfmpegDir <existing kit>]
#                                   [-LibclangPath <dir or libclang.dll>]
#                                   [-Destination C:\ffmpeg-dev] [-Version 9.0]
#                                   [-NoDownload] [-Force]

[CmdletBinding()]
param(
    [string] $Destination,
    [string] $Version,
    [string] $FfmpegDir,
    [string] $LibclangPath,
    [switch] $NoDownload,
    [switch] $Force
)

$ErrorActionPreference = 'Stop'

. "$PSScriptRoot\ffmpeg-env.ps1"

if ($Destination) { $MvpFfmpegRoot = $Destination }
if ($Version) { $MvpFfmpegVersion = $Version }

# ---------------------------------------------------------------------------
# FFmpeg
# ---------------------------------------------------------------------------
# A named kit, or anything already unpacked, beats downloading: the archive is
# over a gigabyte and a contributor usually already has one.
$root = $null
if ($FfmpegDir) {
    if (-not (Test-MvpFfmpegKit $FfmpegDir)) {
        throw "指定的 FFmpeg 目录不是可用的 shared 开发包（需要 include/、lib/*.lib、bin/）：$FfmpegDir"
    }
    $root = (Get-Item -Path $FfmpegDir).FullName
} else {
    $existing = @(Get-Item -Path (Join-Path $MvpFfmpegRoot "ffmpeg-n$MvpFfmpegVersion-*") -ErrorAction SilentlyContinue) |
        Where-Object { $_.PSIsContainer -and (Test-MvpFfmpegKit $_.FullName) } |
        Sort-Object -Property Name -Descending
    if ($existing) {
        $root = $existing[0].FullName
        Write-Host "复用已解压的 FFmpeg：$root" -ForegroundColor DarkGray
    }
}

if (-not $root) {
    if ($NoDownload) {
        throw "本机没找到 FFmpeg $MvpFfmpegVersion 开发包，请先运行 .\scripts\setup-ffmpeg.ps1（或用 -FfmpegDir 指定已有目录）。"
    }

    New-Item -ItemType Directory -Force -Path $MvpFfmpegRoot | Out-Null

    # The **n9.0 release branch** is used rather than `master`: master's headers
    # already contain codec enum values that `ffmpeg-next 9.0.0` does not know
    # about, which makes it fail to compile.
    $archive = "ffmpeg-n$MvpFfmpegVersion-latest-win64-gpl-shared-$MvpFfmpegVersion.zip"
    $url = "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/$archive"
    $zip = Join-Path $MvpFfmpegRoot $archive

    if (-not (Test-Path $zip)) {
        Write-Host "下载 FFmpeg $MvpFfmpegVersion 开发包（约 1 GB）…" -ForegroundColor Cyan
        Write-Host "  $url"
        & curl.exe -L --retry 3 --retry-delay 2 -o $zip $url
        if ($LASTEXITCODE -ne 0) { throw "下载失败" }
    } else {
        Write-Host "已存在 $zip，跳过下载" -ForegroundColor DarkGray
    }

    Write-Host '解压…' -ForegroundColor Cyan
    Expand-Archive -Path $zip -DestinationPath $MvpFfmpegRoot -Force

    $unpacked = @(Get-Item -Path (Join-Path $MvpFfmpegRoot "ffmpeg-n$MvpFfmpegVersion-*") -ErrorAction SilentlyContinue) |
        Where-Object { $_.PSIsContainer } |
        Sort-Object -Property Name -Descending |
        Select-Object -First 1
    if (-not $unpacked) { throw "解压后找不到 FFmpeg 目录" }
    # A build without headers or import libraries cannot be linked.
    if (-not (Test-MvpFfmpegKit $unpacked.FullName)) {
        throw "这个 FFmpeg 包缺少 include/、lib/*.lib 或 bin/*.dll，需要 shared 构建"
    }
    $root = $unpacked.FullName
}

# ---------------------------------------------------------------------------
# libclang
# ---------------------------------------------------------------------------
# Required by ffmpeg-sys-next's bindgen step. The search lives in the shared
# helper so that the ordering is stated once — see `ffmpeg-env.ps1`.
$libclang = Find-MvpLibclang -Hint $LibclangPath

if ($libclang) {
    $libclangEntry = 'LIBCLANG_PATH = {{ value = "{0}", force = false }}' -f ($libclang -replace '\\', '/')
} else {
    # Writing an empty value here would be worse than writing nothing: it would
    # *set* the variable to "" and defeat clang-sys's own search for LLVM.
    $libclangEntry = @"
# LIBCLANG_PATH 未设置：本机没有找到 libclang.dll。下面两条任选其一，然后重跑本脚本：
#   * 安装 LLVM（https://releases.llvm.org/）到 C:\Program Files\LLVM —— clang-sys
#     会自己去那里找，不需要任何配置；
#   * 或者 pip install libclang，它自带的 libclang.dll 会被本脚本探测到。
"@
}

# ---------------------------------------------------------------------------
# .cargo/config.toml
# ---------------------------------------------------------------------------
$configPath = $MvpConfigPath
New-Item -ItemType Directory -Force -Path (Split-Path -Parent $configPath) | Out-Null

# Forward slashes, exactly as `dev.ps1` normalises them: `ffmpeg-sys-next`
# declares `cargo:rerun-if-env-changed=FFMPEG_DIR`, so a value that differs only
# in its separators would look like a configuration change and re-run bindgen
# (a multi-minute rebuild) every time the two entry points are mixed.
$config = @"
# ---------------------------------------------------------------------------
# 本机生成的文件 —— 由 scripts/setup-ffmpeg.ps1 写出，已被 .gitignore 忽略。
#
# 这里记录的是「这台机器上 SDK 装在哪」的事实，不是项目配置：换机器、换解压
# 位置，重跑一次脚本即可，既不用手改路径，也不会被 git pull 覆盖。
#
# FFmpeg 是直接链接的（没有 side-car 进程），用的是 shared 开发包：
#   <root>/include  -> libav*/**/*.h
#   <root>/lib      -> avcodec.lib、avformat.lib …（MSVC 导入库）
#   <root>/bin      -> avcodec-*.dll …（打包时复制到 exe 旁边）
#
# 每一项都用 force = false，因此已经导出的环境变量优先于这里写的值。
# ---------------------------------------------------------------------------

[env]
# FFmpeg 9.0 发行分支 —— 与 ffmpeg-next 9.0.0 的绑定一致。
# （master 分支的枚举里含有这个 crate 还不认识的新编解码器。）
FFMPEG_DIR = { value = "$($root -replace '\\', '/')", force = false }
$libclangEntry
# ffmpeg-sys-next 的 hwcontext 垫片早于 FFmpeg 9 的 CUDA 句柄类型。把新增的两个
# 类型用不透明宏顶掉，bindgen 才能跑完；播放器本身不碰 CUDA。
BINDGEN_EXTRA_CLANG_ARGS = { value = "-DCUarray=void* -DCUDA_ARRAY3D_DESCRIPTOR=void*", force = false }
"@

$existing = if (Test-Path $configPath) { (Get-Content -Path $configPath -Raw).Trim() } else { $null }
if (-not $Force -and $existing -eq $config.Trim()) {
    Write-Host "配置已是最新：$configPath" -ForegroundColor DarkGray
} else {
    Set-Content -Path $configPath -Value $config -Encoding UTF8
    Write-Host "已写入 $configPath" -ForegroundColor Green
}

Write-Host ''
Write-Host '完成。' -ForegroundColor Green
Write-Host ("  FFmpeg  : {0}" -f $root)
Write-Host ("  libclang: {0}" -f $(if ($libclang) { $libclang } else { '未找到 —— 见配置文件里的说明' }))
Write-Host ("  配置    : {0}" -f $configPath)
Write-Host ''
Write-Host '接下来可以：' -ForegroundColor DarkGray
Write-Host '  cargo run        # 直接运行（FFmpeg 的 DLL 由 build.rs 复制到 target/）' -ForegroundColor DarkGray
Write-Host '  .\scripts\dev.ps1 test --workspace' -ForegroundColor DarkGray
