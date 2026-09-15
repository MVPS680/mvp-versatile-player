# Regenerates the media fixtures used by the end-to-end playback tests.
#
# The files are deliberately tiny (a few hundred kilobytes in total) and are
# checked in, so `cargo test` works on a fresh clone without needing an FFmpeg
# command line tool. Re-run this script only when a fixture needs to change.
#
# Usage:  .\scripts\make-testdata.ps1 [-Ffmpeg <path\to\ffmpeg.exe>]

[CmdletBinding()]
param(
    [string] $Ffmpeg = 'D:\ffmpeg-2025-11-12-git-6cdd2cbe32-full_build\bin\ffmpeg.exe'
)

$ErrorActionPreference = 'Stop'

if (-not (Test-Path $Ffmpeg)) {
    $found = Get-Command ffmpeg.exe -ErrorAction SilentlyContinue
    if ($found) { $Ffmpeg = $found.Source } else { throw "找不到 ffmpeg.exe: $Ffmpeg" }
}

$root = Split-Path -Parent $PSScriptRoot
$out = Join-Path $root 'testdata'
New-Item -ItemType Directory -Force -Path $out | Out-Null

function Invoke-Ffmpeg {
    param([string[]] $Arguments)
    & $Ffmpeg -y -loglevel error @Arguments
    if ($LASTEXITCODE -ne 0) { throw "ffmpeg failed: $($Arguments -join ' ')" }
}

Write-Host '生成 tiny.mp4 (3s, 320x240, H.264 + AAC)…' -ForegroundColor Cyan
Invoke-Ffmpeg @(
    '-f', 'lavfi', '-i', 'testsrc2=size=320x240:rate=25:duration=3',
    '-f', 'lavfi', '-i', 'sine=frequency=440:duration=3',
    '-c:v', 'libx264', '-preset', 'ultrafast', '-pix_fmt', 'yuv420p',
    '-c:a', 'aac', '-b:a', '64k', '-shortest', (Join-Path $out 'tiny.mp4')
)

Write-Host '生成 tiny.mp3 (2s)…' -ForegroundColor Cyan
Invoke-Ffmpeg @(
    '-f', 'lavfi', '-i', 'sine=frequency=440:duration=2',
    '-c:a', 'libmp3lame', '-b:a', '64k', (Join-Path $out 'tiny.mp3')
)

Write-Host '生成 tiny.png (320x240)…' -ForegroundColor Cyan
Invoke-Ffmpeg @(
    '-f', 'lavfi', '-i', 'testsrc2=size=320x240:rate=1:duration=1',
    '-frames:v', '1', (Join-Path $out 'tiny.png')
)

Write-Host '生成 tiny.gif (循环动画)…' -ForegroundColor Cyan
Invoke-Ffmpeg @(
    '-f', 'lavfi', '-i', 'testsrc2=size=64x64:rate=10:duration=1',
    '-loop', '0', (Join-Path $out 'tiny.gif')
)

Write-Host '生成 with_subs.mp4 (带内嵌字幕)…' -ForegroundColor Cyan
Invoke-Ffmpeg @(
    '-f', 'lavfi', '-i', 'testsrc2=size=320x240:rate=25:duration=2',
    '-f', 'lavfi', '-i', 'sine=frequency=880:duration=2',
    '-c:v', 'libx264', '-preset', 'ultrafast', '-pix_fmt', 'yuv420p',
    '-c:a', 'aac', '-shortest', (Join-Path $out 'with_subs.mp4')
)

Write-Host '生成 offset.ts (时间轴从 4200s 开始的 MPEG-TS)…' -ForegroundColor Cyan
# A container whose timeline does not start at zero, like a Blu-ray transport
# stream. Seeking has to translate playback time into container time, *and* it
# has to survive a demuxer whose seek reads packets: mpegts seeks by binary
# search, and every read calls the interrupt callback back into the seek path.
Invoke-Ffmpeg @(
    '-f', 'lavfi', '-i', 'testsrc2=size=320x240:rate=25:duration=4',
    '-c:v', 'libx264', '-preset', 'ultrafast', '-pix_fmt', 'yuv420p', '-g', '25',
    '-output_ts_offset', '4200', '-f', 'mpegts', (Join-Path $out 'offset.ts')
)

Get-ChildItem $out | Select-Object Name, @{n = 'KB'; e = { [math]::Round($_.Length / 1KB, 1) } } |
    Format-Table -AutoSize
Write-Host ("测试素材已写入 {0}" -f $out) -ForegroundColor Green
