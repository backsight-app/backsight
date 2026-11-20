<#
.SYNOPSIS
    Installs a new Backsight installer on a clean Windows machine and checks it starts, updates
    over the current release and uninstalls. Used by release.yml before a release is published.
.PARAMETER Installer
    The new Backsight_<version>_x64-setup.exe.
.PARAMETER Version
    The version that installer carries.
.NOTES
    Needs GH_TOKEN to download the current release. Machines without a GPU are fine: capture is not
    checked, only that the program runs without crashing.
#>
param(
    [Parameter(Mandatory)][string]$Installer,
    [Parameter(Mandatory)][string]$Version
)
$ErrorActionPreference = 'Stop'
$Dir = Join-Path $env:LOCALAPPDATA 'Backsight'
$Exe = Join-Path $Dir 'backsight.exe'
$Data = Join-Path $env:APPDATA 'Backsight'
$Settings = Join-Path $Data 'config\settings.toml'

function Install-Backsight([string]$Path) {
    Write-Host "Installing $Path"
    $p = Start-Process $Path -ArgumentList '/S' -Wait -PassThru
    if ($p.ExitCode -ne 0) { throw "installer exited with $($p.ExitCode)" }
    if (-not (Test-Path $Exe)) { throw "no $Exe after install" }
}

# Starts the installed copy, lets it run, and fails on an early exit, a panic or a crash report.
function Test-Run([string]$Label) {
    Remove-Item (Join-Path $Data 'logs\backsight.log') -ErrorAction SilentlyContinue
    $p = Start-Process $Exe -PassThru
    Start-Sleep 20
    # Settings are first written by the window (it opens on a first start): give WebView2 time.
    for ($i = 0; $i -lt 40 -and -not (Test-Path $Settings); $i++) { Start-Sleep 1 }
    if ($p.HasExited) { throw "${Label}: Backsight exited after start (code $($p.ExitCode))" }
    $log = Get-Content (Join-Path $Data 'logs\backsight.log') -Raw -ErrorAction SilentlyContinue
    if (-not $log) { throw "${Label}: no log written" }
    if ($log -match 'panic') { Write-Host $log; throw "${Label}: panic in the log" }
    if (Get-ChildItem (Join-Path $Data 'crash-reports') -ErrorAction SilentlyContinue) { throw "${Label}: crash report written" }
    & $Exe --quit
    if (-not $p.WaitForExit(30000)) { throw "${Label}: Backsight did not quit" }
    Write-Host "${Label}: ran 20 s, quit cleanly"
}

function Uninstall-Backsight {
    # The NSIS uninstaller copies itself to TEMP and returns at once: wait for the folder to go,
    # or it could still be deleting while the next install writes.
    Start-Process (Join-Path $Dir 'uninstall.exe') -ArgumentList '/S' -Wait
    for ($i = 0; $i -lt 60 -and (Test-Path $Dir); $i++) { Start-Sleep 1 }
    if (Test-Path $Dir) { throw "uninstall left $Dir" }
}

# 1. Fresh install.
Install-Backsight $Installer
$got = (Get-Item $Exe).VersionInfo.ProductVersion
if ($got -notlike "$Version*") { throw "installed version $got, expected $Version" }
Test-Run 'fresh install'
Uninstall-Backsight
Remove-Item $Data -Recurse -Force -ErrorAction SilentlyContinue

# 2. Update over the current release (what installed copies do).
$Old = Join-Path $env:RUNNER_TEMP 'old'
gh release download --repo backsight-app/backsight --pattern '*_x64-setup.exe' --dir $Old --clobber
if ($LASTEXITCODE) { throw 'could not download the current release' }
Install-Backsight (Get-ChildItem $Old -Filter *.exe | Select-Object -First 1).FullName
Test-Run 'current release'
if (-not (Test-Path $Settings)) { throw "current release wrote no $Settings" }
# A changed setting must survive the update (the new version may rewrite the file, so not byte-equal).
$text = Get-Content $Settings -Raw
if ($text -notmatch '(?m)^sounds = true') { throw "no 'sounds = true' in $Settings" }
Set-Content $Settings ($text -replace '(?m)^sounds = true', 'sounds = false') -NoNewline
Install-Backsight $Installer
$got = (Get-Item $Exe).VersionInfo.ProductVersion
if ($got -notlike "$Version*") { throw "after update: version $got, expected $Version" }
Test-Run 'after update'
if ((Get-Content $Settings -Raw) -notmatch '(?m)^sounds = false') { throw 'a changed setting was lost in the update' }
Uninstall-Backsight
Write-Host 'Install test passed.'
