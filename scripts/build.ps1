# Builds the Backsight installer from source on a clean PC.
# Run from PowerShell: ./scripts/build.ps1
# It checks what you need, fetches FFmpeg, then builds the app and its installer.
# The installer ends up in target\release\bundle\nsis\ (or in CARGO_TARGET_DIR if you set it).
$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot

function Fail($what, $fix) {
    Write-Host ""
    Write-Host "Can't continue: $what" -ForegroundColor Red
    Write-Host "What to do: $fix"
    exit 1
}

function Have($name) { [bool](Get-Command $name -ErrorAction SilentlyContinue) }

# Rust, with the Windows (MSVC) toolchain
if (-not (Have "rustc") -or -not (Have "cargo")) {
    Fail "Rust is not installed." "Install it from https://rustup.rs, then open a new PowerShell window and run this script again."
}
if ((rustc -vV | Out-String) -notmatch "host: x86_64-pc-windows-msvc") {
    Fail "Rust is not set up for Windows (MSVC)." "Run: rustup default stable-x86_64-pc-windows-msvc"
}

# Visual Studio Build Tools (the C++ compiler)
$vswhere = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\Installer\vswhere.exe"
$hasVs = (Have "cl.exe") -or ((Test-Path $vswhere) -and
    (& $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath))
if (-not $hasVs) {
    Fail "The Visual Studio C++ build tools are missing." "Install 'Visual Studio Build Tools 2022' from https://visualstudio.microsoft.com/downloads/ and tick 'Desktop development with C++' (with a Windows 11 SDK)."
}

# Node.js 22 or newer
if (-not (Have "node")) {
    Fail "Node.js is not installed." "Install Node.js 22 or newer from https://nodejs.org, then open a new PowerShell window."
}
$nodeMajor = [int](((node --version).TrimStart("v")).Split(".")[0])
if ($nodeMajor -lt 22) {
    Fail "Node.js $nodeMajor is too old." "Install Node.js 22 or newer from https://nodejs.org."
}

# libclang (needed to build the FFmpeg bindings)
$libclang = $env:LIBCLANG_PATH -and (Test-Path (Join-Path $env:LIBCLANG_PATH "libclang.dll"))
if (-not $libclang) {
    $libclang = (Test-Path "$env:ProgramFiles\LLVM\bin\libclang.dll")
}
if (-not $libclang) {
    Fail "libclang was not found." "Install LLVM ('winget install LLVM.LLVM', needs admin), or without admin run 'python -m pip install --user libclang' and set LIBCLANG_PATH to the folder holding libclang.dll (see docs/DEVELOPMENT.md)."
}

# CMake (builds whisper.cpp for captions). Visual Studio's C++ tools include one that isn't on PATH.
if (-not (Have "cmake")) {
    $vsPath = if (Test-Path $vswhere) { & $vswhere -latest -products * -property installationPath }
    $vsCmake = if ($vsPath) { Join-Path $vsPath "Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin" }
    if ($vsCmake -and (Test-Path (Join-Path $vsCmake "cmake.exe"))) {
        $env:PATH = "$vsCmake;$env:PATH"
    } else {
        Fail "CMake was not found." "In the Visual Studio Installer, modify Build Tools 2022 and tick 'C++ CMake tools for Windows', or install CMake from https://cmake.org/download/ and open a new PowerShell window."
    }
}

Write-Host "All tools found. Fetching FFmpeg..." -ForegroundColor Cyan
& (Join-Path $PSScriptRoot "fetch-ffmpeg.ps1")
if ($LASTEXITCODE) { Fail "Fetching FFmpeg failed." "Read the message above. fetch-ffmpeg.ps1 needs the GitHub CLI (gh) signed in; see docs/DEVELOPMENT.md." }

Push-Location (Join-Path $Root "app")
try {
    Write-Host "Installing the window's packages..." -ForegroundColor Cyan
    npm ci
    if ($LASTEXITCODE) { Fail "'npm ci' failed." "Check your internet connection and run the script again." }

    Write-Host "Building Backsight and its installer (this takes a while)..." -ForegroundColor Cyan
    npm run tauri build
    if ($LASTEXITCODE) { Fail "The build failed." "Read the error above. If it says LNK1104 or a path is too long, move the folder somewhere shorter or set CARGO_TARGET_DIR to a short folder such as C:\cargo-target." }
} finally { Pop-Location }

$target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $Root "target" }
Write-Host ""
Write-Host "Done. The installer is in $target\release\bundle\nsis\" -ForegroundColor Green
