# Machine-local paths shared by every script in this folder.
#
# The FFmpeg development kit and `libclang` are facts about *this machine*, not
# about the project: they live wherever the SDK happens to have been unpacked or
# installed. Committing them means the next `git pull` on a second machine
# overwrites the paths with someone else's, so `.cargo/config.toml` is generated
# locally by `setup-ffmpeg.ps1` and ignored by git instead.
#
# This file is dot-sourced by `setup-ffmpeg.ps1`, `dev.ps1` and `package.ps1` so
# that all three agree on where those things are. It only *reads*: it never
# downloads, writes or installs anything.
#
# Usage:  . "$PSScriptRoot\ffmpeg-env.ps1"

# The release branch `setup-ffmpeg.ps1` fetches, and the directory it unpacks
# into. Both are overridable from the command line.
$MvpFfmpegVersion = '9.0'
$MvpFfmpegRoot = 'C:\ffmpeg-dev'

$MvpRepoRoot = Split-Path -Parent $PSScriptRoot
$MvpConfigPath = Join-Path $MvpRepoRoot '.cargo\config.toml'

# Where a plain `setup-ffmpeg.ps1` run puts the kit.
function Get-MvpDefaultFfmpegDir {
    Join-Path $MvpFfmpegRoot "ffmpeg-n$MvpFfmpegVersion-latest-win64-gpl-shared-$MvpFfmpegVersion"
}

# A kit is only usable when it has headers to bind, import libraries to link
# and DLLs to run against. BtbN publishes static and shared archives under
# near-identical names; picking the static one fails much later, at link time,
# as a wall of unresolved symbols.
function Test-MvpFfmpegKit {
    param([string] $Path)

    if (-not $Path -or -not (Test-Path -Path $Path -PathType Container)) { return $false }
    foreach ($needed in @('include', 'lib', 'bin')) {
        if (-not (Test-Path -Path (Join-Path $Path $needed) -PathType Container)) { return $false }
    }
    $libs = Get-ChildItem -Path (Join-Path $Path 'lib') -Filter *.lib -ErrorAction SilentlyContinue
    return $libs.Count -ge 5
}

# Reads one `KEY = { value = "…" }` entry back out of the generated cargo
# config, so the scripts reuse what setup already decided instead of repeating
# its search.
function Get-MvpConfigValue {
    param([string] $Key)

    if (-not (Test-Path -Path $MvpConfigPath)) { return $null }
    # `{{` is how `-f` spells a literal brace, so this matches the generated
    # `KEY = { value = "…", force = false }` form.
    $pattern = '^\s*{0}\s*=\s*{{\s*value\s*=\s*"([^"]*)"' -f $Key
    $match = Select-String -Path $MvpConfigPath -Pattern $pattern -ErrorAction SilentlyContinue |
        Select-Object -First 1
    if (-not $match) { return $null }
    return $match.Matches[0].Groups[1].Value
}

# The FFmpeg kit to build against: what the environment says, then what
# `setup-ffmpeg.ps1` last wrote, then the default download location. Returns
# `$null` when the kit still has to be fetched.
function Get-MvpFfmpegDir {
    if ($env:FFMPEG_DIR -and (Test-MvpFfmpegKit $env:FFMPEG_DIR)) { return $env:FFMPEG_DIR }

    $configured = Get-MvpConfigValue 'FFMPEG_DIR'
    if ($configured -and (Test-MvpFfmpegKit $configured)) { return $configured }

    $default = Get-MvpDefaultFfmpegDir
    if (Test-MvpFfmpegKit $default) { return $default }

    return $null
}

# Asks one interpreter where `pip install libclang` put the DLL.
#
# A missing module is the *expected* answer on a machine that never installed
# it, so the traceback is dropped and the error preference is relaxed for the
# call: with `$ErrorActionPreference = 'Stop'` PowerShell would otherwise turn
# the interpreter's stderr into a terminating error and abort the whole setup.
function Get-MvpPipLibclangDir {
    param([string] $Interpreter, [string[]] $Extra)

    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'SilentlyContinue'
    try {
        $probe = & $Interpreter @Extra -c "import clang, os; print(os.path.join(os.path.dirname(clang.__file__), 'native'))" 2>$null
    } finally {
        $ErrorActionPreference = $previous
    }

    $first = @($probe) | Where-Object { $_ } | Select-Object -First 1
    if (-not $first) { return $null }
    return $first.Trim()
}

# Every directory that might hold `libclang.dll`, in the order they should be
# tried.
#
# `clang-sys` — the crate bindgen loads the DLL through — searches
# `LIBCLANG_PATH`, then `llvm-config`, then a fixed list that covers a
# system-wide LLVM install but *not* the copy that ships inside pip's `clang`
# package. That copy is by far the cheapest for a contributor to obtain, so it
# is looked for here too rather than left to chance.
function Find-MvpLibclangDirs {
    $dirs = New-Object System.Collections.Generic.List[string]

    if ($env:LIBCLANG_PATH) { $dirs.Add($env:LIBCLANG_PATH) }
    $configured = Get-MvpConfigValue 'LIBCLANG_PATH'
    if ($configured) { $dirs.Add($configured) }

    # `pip install libclang`: ask each interpreter where it actually put the
    # DLL rather than guessing at a site-packages layout, so virtualenvs, the
    # Microsoft Store build and custom install locations are all covered.
    foreach ($name in @('python', 'python3', 'py')) {
        $interpreter = Get-Command -Name $name -ErrorAction SilentlyContinue
        if (-not $interpreter) { continue }
        $extra = if ($name -eq 'py') { @('-3') } else { @() }
        $native = Get-MvpPipLibclangDir -Interpreter $interpreter.Source -Extra $extra
        if ($native) { $dirs.Add($native) }
    }

    # Installer layouts that put the DLL somewhere the interpreter probe cannot
    # see: LLVM itself, Scoop, and the LLVM that ships inside Visual Studio.
    $patterns = @(
        (Join-Path $env:LOCALAPPDATA 'Programs\Python\Python3*\Lib\site-packages\clang\native'),
        (Join-Path $env:APPDATA 'Python\Python3*\site-packages\clang\native'),
        (Join-Path $env:LOCALAPPDATA 'Packages\PythonSoftwareFoundation.Python.3*\LocalCache\local-packages\Python3*\site-packages\clang\native'),
        'C:\Program Files\LLVM\bin',
        'C:\Program Files (x86)\LLVM\bin',
        (Join-Path $env:USERPROFILE 'scoop\apps\llvm\current\bin'),
        'C:\Program Files\Microsoft Visual Studio\*\*\VC\Tools\Llvm\*\bin',
        'C:\Program Files (x86)\Microsoft Visual Studio\*\*\VC\Tools\Llvm\*\bin'
    )
    foreach ($pattern in $patterns) {
        # `Get-Item` returns the directory itself for a literal path and every
        # match for a wildcard pattern, which is what both cases want.
        foreach ($found in @(Get-Item -Path $pattern -ErrorAction SilentlyContinue)) {
            if ($found.PSIsContainer) { $dirs.Add($found.FullName) }
        }
    }

    return $dirs
}

# The first candidate that really contains `libclang.dll`, or `$null`.
#
# `LIBCLANG_PATH` is also allowed to name the DLL itself — clang-sys accepts
# both forms — so a file is turned into its directory here.
function Find-MvpLibclang {
    param([string] $Hint)

    $candidates = New-Object System.Collections.Generic.List[string]
    if ($Hint) { $candidates.Add($Hint) }
    foreach ($dir in (Find-MvpLibclangDirs)) { $candidates.Add($dir) }

    foreach ($candidate in $candidates) {
        if (-not $candidate) { continue }
        $item = Get-Item -Path $candidate -ErrorAction SilentlyContinue
        if (-not $item) { continue }
        if (-not $item.PSIsContainer) {
            $item = $item.Directory
            if (-not $item) { continue }
        }
        if (Test-Path -Path (Join-Path $item.FullName 'libclang.dll') -PathType Leaf) {
            return $item.FullName
        }
    }

    return $null
}
