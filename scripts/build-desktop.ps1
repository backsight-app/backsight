# Builds a self-contained application folder and a desktop shortcut, without an installer.
# A running Backsight isn't touched: the build waits in dist-desktop/next and Home offers an
# Update button (ADR 0009). Otherwise it is installed right away.
# -Installer also builds the NSIS installer (target/release/bundle/nsis).
param([switch]$Installer)
$ErrorActionPreference = 'Stop'
$ProjectRoot = Split-Path -Parent $PSScriptRoot
# Builds this checkout; FFmpeg and the desktop app folder live in the main checkout, also when
# run from a linked git worktree.
$MainRoot = Split-Path -Parent (git -C $ProjectRoot rev-parse --path-format=absolute --git-common-dir)
$Output = Join-Path $MainRoot 'dist-desktop/Backsight'
$Next = Join-Path $MainRoot 'dist-desktop/next'
# shortcut: two builds at once would mix files in dist-desktop/next; add a lock if they do.

if (-not $env:LIBCLANG_PATH -and (Get-Command python -ErrorAction SilentlyContinue)) {
    $ClangPath = python -c "import pathlib, clang; print(pathlib.Path(clang.__file__).parent / 'native')" 2>$null
    if ($LASTEXITCODE -eq 0 -and (Test-Path -LiteralPath (Join-Path $ClangPath 'libclang.dll'))) {
        $env:LIBCLANG_PATH = $ClangPath
    }
}

# CMake builds whisper.cpp (captions); Visual Studio's C++ tools carry one that isn't on PATH.
if (-not (Get-Command cmake -ErrorAction SilentlyContinue)) {
    $Vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    $VsPath = if (Test-Path $Vswhere) { & $Vswhere -latest -products * -property installationPath }
    if ($VsPath) { $env:PATH = (Join-Path $VsPath 'Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin') + ";$env:PATH" }
}

# Keep the builder's Windows user name out of the program: rustc embeds source paths (crate
# registry, this checkout) in panic messages. The last matching prefix wins, so the profile goes first.
$CargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE '.cargo' }
$PreviousFlags = $env:RUSTFLAGS
$env:RUSTFLAGS = "$PreviousFlags --remap-path-prefix=$env:USERPROFILE=user --remap-path-prefix=$CargoHome=cargo --remap-path-prefix=$ProjectRoot=backsight".Trim()
$TargetRoot = if ($env:CARGO_TARGET_DIR) {
    # Windows PowerShell 5.1 has no two-argument GetFullPath.
    if ([IO.Path]::IsPathRooted($env:CARGO_TARGET_DIR)) { $env:CARGO_TARGET_DIR } else { [IO.Path]::GetFullPath((Join-Path $ProjectRoot $env:CARGO_TARGET_DIR)) }
} else { Join-Path $ProjectRoot 'target' }
$Executable = Join-Path $TargetRoot 'release/backsight.exe'
# whisper.cpp's asserts name their source files (__FILE__), which cargo copies under the target
# folder; trim that folder off. Same flags as .cargo/config.toml plus the trim (env wins over it).
$Trim = "/d1trimfile:$([IO.Path]::GetFullPath($TargetRoot).TrimEnd('\'))\"
$env:CMAKE_C_FLAGS_RELEASE = "-nologo -MD -O2 -Ob2 -DNDEBUG $Trim"
$env:CMAKE_CXX_FLAGS_RELEASE = "-utf-8 -nologo -MD -O2 -Ob2 -DNDEBUG $Trim"
$SymbolsName = "$(Get-Date -Format 'yyyy-MM-dd-HHmm')-$(git -C $ProjectRoot log -1 --format=%h)"

# Checks the program just built and keeps its PDB, before the next build replaces both.
function Test-AndKeepSymbols([string]$Suffix) {
    if (-not (Test-Path -LiteralPath $Executable)) { throw "Missing application: $Executable" }
    $Text = [Text.Encoding]::ASCII.GetString([IO.File]::ReadAllBytes($Executable))
    foreach ($Private in $env:USERPROFILE, $ProjectRoot) {
        if ($Text.IndexOf($Private, [StringComparison]::OrdinalIgnoreCase) -ge 0) {
            throw "The built program still contains '$Private'. Don't share it; check the --remap-path-prefix flags."
        }
    }
    # Each build's PDB is kept (never shared): it turns a freeze report's `backsight.exe+0x…` into code.
    $Pdb = Join-Path $TargetRoot 'release/backsight.pdb'
    if (Test-Path -LiteralPath $Pdb) {
        $Symbols = Join-Path $MainRoot "dist-desktop/symbols/$SymbolsName$Suffix"
        New-Item -ItemType Directory -Force -Path $Symbols | Out-Null
        Copy-Item -LiteralPath $Pdb -Destination $Symbols
    }
}

Push-Location (Join-Path $ProjectRoot 'app')
try {
    # The installer is the shareable build, without the local updater (ADR 0009). The desktop copy
    # then gets its own build with it, which only recompiles the app crate.
    if ($Installer) {
        # The installer ships THIRD-PARTY-NOTICES.txt: refuse to share one with stale credits.
        & (Join-Path $PSScriptRoot 'third-party-notices.ps1') -Check
        npm run tauri build
        if ($LASTEXITCODE -ne 0) { throw 'Backsight could not be built. See the build output above.' }
        Test-AndKeepSymbols '-installer'
    }
    npm run tauri build -- --no-bundle --features local-update
    if ($LASTEXITCODE -ne 0) { throw 'Backsight could not be built. See the build output above.' }
    Test-AndKeepSymbols ''
} finally {
    Pop-Location
    $env:RUSTFLAGS = $PreviousFlags
}
$Libraries = @(Get-ChildItem (Join-Path $MainRoot 'third_party/ffmpeg/bin') -Filter '*.dll')
if ($Libraries.Count -eq 0) { throw 'FFmpeg is missing. Run scripts/fetch-ffmpeg.ps1 first.' }

if (Test-Path -LiteralPath $Next) { Remove-Item -LiteralPath $Next -Recurse -Force }
New-Item -ItemType Directory -Force -Path $Next | Out-Null
Copy-Item -LiteralPath $Executable -Destination $Next
foreach ($Library in $Libraries) { Copy-Item -LiteralPath $Library.FullName -Destination $Next }
Copy-Item -LiteralPath (Join-Path $ProjectRoot 'app/src-tauri/icons/desktop/icon.ico') -Destination (Join-Path $Next 'Backsight.ico')
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'apply-update.ps1') -Destination $Next
# General's About section opens the notices beside the program, as in an installed copy.
Copy-Item -LiteralPath (Join-Path $ProjectRoot 'THIRD-PARTY-NOTICES.txt') -Destination $Next
Copy-Item -LiteralPath (Join-Path $ProjectRoot 'LICENSE') -Destination (Join-Path $Next 'LICENSE.txt')
# One line that Home shows: when it was built and from which commit.
$Commit = git -C $ProjectRoot log -1 --format='%h %s'
$Dirty = if (git -C $ProjectRoot status --porcelain --untracked-files=no) { ' (plus uncommitted changes)' } else { '' }
# WriteAllText: UTF-8 without the BOM that Set-Content adds in Windows PowerShell.
[IO.File]::WriteAllText((Join-Path $Next 'build.txt'), "$(Get-Date -Format 'yyyy-MM-dd HH:mm'), $Commit$Dirty")

$Running = Get-Process backsight -ErrorAction SilentlyContinue |
    Where-Object { $_.Path -eq (Join-Path $Output 'backsight.exe') }
if ($Running) {
    Write-Output 'Backsight is running: the new build waits for its Update button on Home.'
} else {
    & (Join-Path $Next 'apply-update.ps1') -NoStart
    if (Test-Path -LiteralPath $Next) { throw "The new build couldn't be installed. See $env:TEMP\backsight-update.log." }
}

$Desktop = [Environment]::GetFolderPath('DesktopDirectory')
if (-not $Desktop) { throw 'Windows did not provide a desktop folder.' }
$ShortcutPath = Join-Path $Desktop 'Backsight.lnk'
# Preserve an unrelated shortcut if it already uses this name.
if (Test-Path -LiteralPath $ShortcutPath) {
    $Existing = (New-Object -ComObject WScript.Shell).CreateShortcut($ShortcutPath)
    if ($Existing.TargetPath -ne (Join-Path $Output 'backsight.exe')) {
        $ShortcutPath = Join-Path $Desktop 'Backsight (local).lnk'
        if (Test-Path -LiteralPath $ShortcutPath) {
            $Existing = (New-Object -ComObject WScript.Shell).CreateShortcut($ShortcutPath)
            if ($Existing.TargetPath -ne (Join-Path $Output 'backsight.exe')) {
                throw 'Both desktop shortcut names are already taken.'
            }
        }
    }
}
$Shortcut = (New-Object -ComObject WScript.Shell).CreateShortcut($ShortcutPath)
$Shortcut.TargetPath = Join-Path $Output 'backsight.exe'
$Shortcut.WorkingDirectory = $Output
$Shortcut.IconLocation = "$(Join-Path $Output 'Backsight.ico'),0"
$Shortcut.Description = 'Backsight — save your last great moment.'
$Shortcut.Save()
if (-not (Test-Path -LiteralPath $ShortcutPath)) { throw 'The desktop shortcut was not created.' }
Write-Output "Application: $Output"
Write-Output "Desktop shortcut: $ShortcutPath"
