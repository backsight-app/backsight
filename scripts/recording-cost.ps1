# One run of the recording regression (docs/architecture/testing.md): repeats the Instant Replay
# game FPS measurement with a recording running, plus RAM and replay save time while recording. Run from an elevated PowerShell 7 (PresentMon needs it)
# while the game shows the same repeatable scene each time. Backsight itself must not be running.
#
# Usage: ./scripts/recording-cost.ps1 -Kind plain|base|replay|record -Run 1 [-Game TombRaider.exe]
#        [-Seconds 60] [-StartDelay 20] [-Settings <settings.toml>] [-PresentMon PresentMon.exe] [-KeepMedia] [-DryRun]
#   plain   nothing captures (compare with base to measure the cost of WGC alone)
#   base    WGC capture only: the baseline the 5% budget is measured over
#   replay  Instant Replay alone
#   record  Instant Replay plus a running recording
# Writes frames.csv, gpu.csv, and harness.log/.err in <Target>/m2-<kind>-<run>/.
# Compare two runs with: ./scripts/hardware-check.ps1 -ReportOnly -ReplayOffCsv <base.csv> -ReplayOnCsv <record.csv>
param(
    [Parameter(Mandatory)][ValidateSet('plain', 'base', 'replay', 'record')][string]$Kind,
    [Parameter(Mandatory)][ValidateRange(1, 2147483647)][int]$Run,
    [string]$Game = 'TombRaider.exe',
    # PresentMon needs time beyond 30 seconds to provide 30 seconds of frame intervals.
    [ValidateRange(35, 3600)][int]$Seconds = 60,
    # Time to switch to the game and start the scene before PresentMon starts.
    [ValidateRange(0, 600)][int]$StartDelay = 20,
    [string]$Settings,
    [string]$PresentMon = 'PresentMon.exe',
    [switch]$KeepMedia,
    [switch]$DryRun
)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'hardware-metrics.ps1')
$Root = Split-Path -Parent $PSScriptRoot
# The test programs are started directly, not by Cargo, so they need FFmpeg's DLLs on PATH.
$env:PATH = "$Root\third_party\ffmpeg\bin;$env:PATH"
$Target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $Root 'target' }
# Relative to the repo, as cargo reads it there; works in Windows PowerShell 5.1 too.
if (-not [IO.Path]::IsPathRooted($Target)) { $Target = Join-Path $Root $Target }
$Target = [IO.Path]::GetFullPath($Target)
$RunDir = Join-Path $Target "m2-$Kind-$Run"
$FramesCsv = Join-Path $RunDir 'frames.csv'
$GpuCsv = Join-Path $RunDir 'gpu.csv'
$Log = Join-Path $RunDir 'harness.log'
$Err = Join-Path $RunDir 'harness.err'
$Media = Join-Path $RunDir 'media'
$Test = @{ base = 'gpu_cost'; replay = 'gpu_cost'; record = 'recording_cost' }[$Kind]
$HarnessTimeout = $StartDelay + $Seconds + 30
$PresentMonArgs = @('--v1_metrics', '--timed', "$Seconds", '--terminate_after_timed', '--no_console_stats', '--output_file', $FramesCsv)
$SmiArgs = @('--query-gpu=timestamp,utilization.gpu,clocks.gr,power.draw,temperature.gpu', '--format=csv', '-lms', '1000', '-f', "`"$GpuCsv`"")

if ($Settings) {
    if (-not (Test-Path -LiteralPath $Settings -PathType Leaf)) { throw "Settings file not found: $Settings" }
    $Settings = (Resolve-Path -LiteralPath $Settings).Path
}
if ($DryRun) {
    if ($Test) {
        $Source = if ($Settings) { $Settings } else { 'catalog defaults' }
        Write-Host "harness: $Test ($Kind, $Source) > $Log"
    }
    Write-Host "after ${StartDelay}s: nvidia-smi $SmiArgs"
    Write-Host "after ${StartDelay}s: $PresentMon --process_id <PID of $Game> $PresentMonArgs"
    return
}

if (Get-Process backsight -ErrorAction SilentlyContinue) {
    throw 'Quit Backsight from its tray menu first, so two captures do not compete.'
}
$PresentMonExe = (Get-Command $PresentMon -CommandType Application).Source
$SmiExe = (Get-Command nvidia-smi -CommandType Application).Source
# Claim the name atomically. Directory creation alone can succeed on an existing directory.
$Claim = Join-Path $Target ('.m2-' + [guid]::NewGuid())
[IO.Directory]::CreateDirectory($Claim) | Out-Null
try {
    [IO.Directory]::Move($Claim, $RunDir)
} catch {
    if (Test-Path -LiteralPath $RunDir) { throw "Output already exists: $RunDir. Choose a new run number." }
    throw
} finally {
    if (Test-Path -LiteralPath $Claim) { [IO.Directory]::Delete($Claim) }
}
$Harness = $null
$Smi = $null
$OwnMedia = $false

function Wait-HarnessMarker([string]$Marker, [int]$Timeout) {
    $Deadline = [datetime]::UtcNow.AddSeconds($Timeout)
    while (-not (Select-String -LiteralPath $Log -SimpleMatch -Pattern $Marker -Quiet)) {
        if ($Harness.HasExited -or [datetime]::UtcNow -ge $Deadline) {
            throw "The harness did not reach $Marker. See $Log and $Err."
        }
        Start-Sleep -Milliseconds 100
    }
}

try {
    if ($Test) {
        # Resolve the compiled executable before timing, then own just the test process.
        Push-Location $Root
        # Cargo writes progress to stderr; under 'Stop' a redirected stderr line (PowerShell 5.1) would end the run.
        $ErrorActionPreference = 'Continue'
        try {
            $Artifacts = cargo test --locked --target-dir $Target --release -p backsight-engine --test $Test --no-run --message-format=json
            if ($LASTEXITCODE -ne 0) { throw "Building the $Test harness failed." }
        } finally {
            $ErrorActionPreference = 'Stop'
            Pop-Location
        }
        $Exe = $Artifacts | ConvertFrom-Json | Where-Object {
            $_.reason -eq 'compiler-artifact' -and $_.target.name -eq $Test -and $_.executable
        } | Select-Object -ExpandProperty executable
        if (@($Exe).Count -ne 1 -or -not $Exe) { throw "Could not locate the $Test executable." }

        # Settings::load_from uses the catalog defaults for a missing file.
        if (-not $Settings) { $Settings = Join-Path $RunDir 'defaults.toml' }
        if ($Kind -eq 'record') {
            [IO.Directory]::CreateDirectory($Media) | Out-Null
            $OwnMedia = $true
        }
        $Vars = @{
            BACKSIGHT_EXP = $Settings
            BACKSIGHT_EXP_SECONDS = "$HarnessTimeout"
            BACKSIGHT_EXP_MODE = $(if ($Kind -eq 'base') { 'capture' } else { $null })
            BACKSIGHT_COST_RUN = $RunDir
        }
        $Previous = @{}
        try {
            foreach ($Key in $Vars.Keys) {
                $Previous[$Key] = [Environment]::GetEnvironmentVariable($Key)
                [Environment]::SetEnvironmentVariable($Key, $Vars[$Key])
            }
            $Harness = Start-Process $Exe -ArgumentList '--ignored', '--nocapture' -WindowStyle Hidden -PassThru -WorkingDirectory $Root -RedirectStandardOutput $Log -RedirectStandardError $Err
            $null = $Harness.Handle
        } finally {
            foreach ($Key in $Previous.Keys) { [Environment]::SetEnvironmentVariable($Key, $Previous[$Key]) }
        }
        Wait-HarnessMarker 'BACKSIGHT_COST_READY' 30
    }
    Write-Host "Switch to the game and start the scene: measuring in $StartDelay s for $Seconds s."
    Start-Sleep -Seconds $StartDelay
    $GameProcesses = @(Get-Process -ErrorAction Stop | Where-Object {
        $_.ProcessName -eq [IO.Path]::GetFileNameWithoutExtension($Game)
    })
    if ($GameProcesses.Count -ne 1) { throw "Expected one running instance of $Game." }
    $GameId = $GameProcesses[0].Id
    $Smi = Start-Process $SmiExe -ArgumentList $SmiArgs -WindowStyle Hidden -PassThru
    if ($Harness) {
        # Publish the PID only once the whole file can be read.
        [IO.File]::WriteAllText((Join-Path $RunDir 'game.pid.tmp'), "$GameId")
        [IO.File]::Move((Join-Path $RunDir 'game.pid.tmp'), (Join-Path $RunDir 'game.pid'))
        Wait-HarnessMarker 'BACKSIGHT_COST_MEASURING' 10
    }
    & $PresentMonExe --process_id "$GameId" @PresentMonArgs
    if ($LASTEXITCODE -ne 0) { throw "PresentMon failed ($LASTEXITCODE). Run from an elevated PowerShell, with PresentMon 2.x (the FrameView SDK copy is too old for these options)." }
    if ($Smi.HasExited) { throw "nvidia-smi exited during the measurement ($($Smi.ExitCode))." }
    if ($Harness) {
        if ($Harness.HasExited) { throw "The harness exited during measurement. See $Log and $Err." }
        [IO.File]::WriteAllText((Join-Path $RunDir 'presentmon.done'), '')
    }
    Stop-Process -InputObject $Smi -ErrorAction SilentlyContinue
    Read-PresentMon $FramesCsv -ExpectedProcessId $GameId -ExpectedApplication ([IO.Path]::GetFileName($Game)) | Out-Null
    if ($Harness) {
        if (-not $Harness.WaitForExit(120000)) { throw "The harness did not finish. See $Log and $Err." }
        if ($Harness.ExitCode -ne 0) { throw "The harness failed. See $Log and $Err." }
        Get-Content -LiteralPath $Log | Select-String 'BACKSIGHT_M2_RECORDING|^EXP|test result' | ForEach-Object { Write-Host $_ }
    }
} finally {
    if ($Smi -and -not $Smi.HasExited) { Stop-Process -InputObject $Smi -ErrorAction SilentlyContinue }
    if ($Harness -and -not $Harness.HasExited) {
        Stop-Process -InputObject $Harness -ErrorAction SilentlyContinue
        if (-not $Harness.WaitForExit(5000)) { throw "The harness is still running. Check its output in $RunDir." }
    }
    if ($OwnMedia) {
        if ($KeepMedia) {
            Write-Host "Saved benchmark media in $Media"
        } else {
            if ((Get-Item -LiteralPath $Media).Attributes -band [IO.FileAttributes]::ReparsePoint) {
                throw "Cannot clean up a redirected media folder: $Media"
            }
            # The harness only writes flat files here, including interrupted .part files.
            foreach ($File in [IO.Directory]::EnumerateFiles($Media)) { [IO.File]::Delete($File) }
            [IO.Directory]::Delete($Media)
        }
    }
}
Write-Host "Wrote benchmark results in $RunDir"
