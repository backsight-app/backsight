# Hardware check (docs/architecture/testing.md, tier 2): run on a PC with a GPU before merging
# capture or encode changes and before releases. Prints the machine, probes the encoders, runs
# the ignored hardware tests in release (including colour and encoder quality) and writes
# target/hardware-report.md to attach to the PR.
# Usage: ./scripts/hardware-check.ps1
# Needs an unlocked desktop and an audio output: the tests play a beep and press Ctrl+Alt+Shift+F24.
param(
    [string]$ReplayOffCsv,
    [string]$ReplayOnCsv,
    [switch]$QualifyM1,
    [switch]$ReportOnly
)
$ErrorActionPreference = "Stop"
. (Join-Path $PSScriptRoot 'hardware-metrics.ps1')
if ([bool]$ReplayOffCsv -ne [bool]$ReplayOnCsv) { throw 'Provide both replay-off and replay-on CSV paths.' }
if (-not $ReportOnly -and (Get-Process backsight -ErrorAction SilentlyContinue)) {
    throw 'Quit Backsight from its tray menu before running the hardware tests.'
}
# Resolve before Set-Location, so relative input paths mean the caller's current directory.
if ($ReplayOffCsv) {
    $ReplayOffCsv = (Resolve-Path -LiteralPath $ReplayOffCsv).Path
    $ReplayOnCsv = (Resolve-Path -LiteralPath $ReplayOnCsv).Path
}

$Root = Split-Path -Parent $PSScriptRoot
Set-Location $Root
$Target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $Root "target" }
New-Item -ItemType Directory -Force $Target | Out-Null
$Report = Join-Path $Target "hardware-report.md"

$os = Get-CimInstance Win32_OperatingSystem
$gpus = Get-CimInstance Win32_VideoController | ForEach-Object { "- $($_.Name), driver $($_.DriverVersion)" }
$header = @(
    "# Backsight hardware check",
    "",
    "Commit $(git rev-parse --short HEAD) on $(Get-Date -Format 'yyyy-MM-dd HH:mm')",
    "",
    "- $($os.Caption), build $($os.BuildNumber)"
) + $gpus + ""
$header | ForEach-Object { Write-Host $_ }
$lines = @()

# Runs cargo through cmd so stderr is plain text in Windows PowerShell too; drops build progress.
function Invoke-Cargo([string]$Arguments) {
    $out = cmd /c "cargo $Arguments 2>&1"
    $ok = $LASTEXITCODE -eq 0
    $kept = $out | Where-Object { $_ -notmatch '^\s*(Compiling|Finished|Running|Blocking|Downloaded|Downloading|Updating|Locking)\b' }
    return @{ Ok = $ok; Output = $kept }
}

$results = @()
if (-not $ReportOnly) {
Write-Host "Probing encoders (builds in release first, which takes a few minutes)..."
$probe = Invoke-Cargo "test --release -p backsight-encode --lib probe::tests::cpu_encoders_work_and_failures_explain_themselves -- --exact --nocapture"
$lines += "## Encoders", "", '```'
$lines += $probe.Output | Where-Object { $_ -match '^\S+\s{2,}' }
$lines += '```', ""
$results += , @("encoder probe", $probe.Ok)

$tests = @(
    @("capture_to_clip", "WGC capture, GPU conversion and NVENC zero copy into an MP4"),
    @("replay_session", "Instant Replay end to end: a saved clip with desktop audio"),
    @("loopback", "WASAPI desktop audio, continuous and on the QPC clock"),
    @("raw_input", "Hotkeys through Raw Input"),
    @("colour", "Colour bars and range ramps through the GPU converter and NVENC read back as BT.709 limited range; wrong-matrix and wrong-range controls are caught (synthetic pattern)"),
    @("quality", "x264 and NVENC H.264/HEVC defaults are checked against PSNR, SSIM and available VMAF floors; good and bad controls (including an x264 wrong-matrix control) bracket the floors (synthetic reference)", "backsight-encode")
)
$lines += "## Hardware tests (release)", ""
foreach ($t in $tests) {
    Write-Host "Running $($t[0])..."
    $OldQualification = $env:BACKSIGHT_QUALIFY_M1
    try {
        if ($t[0] -eq 'replay_session' -and $QualifyM1) { $env:BACKSIGHT_QUALIFY_M1 = '1' }
        else { $env:BACKSIGHT_QUALIFY_M1 = $null }
        $package = if ($t.Count -gt 2) { $t[2] } else { "backsight-engine" }
        $r = Invoke-Cargo "test --release -p $package --test $($t[0]) -- --ignored --nocapture --test-threads=1 --skip a_long_recording"
    } finally { $env:BACKSIGHT_QUALIFY_M1 = $OldQualification }
    $results += , @($t[0], $r.Ok)
    $mark = if ($r.Ok) { "pass" } else { "FAIL" }
    $lines += "### $($t[0]): $mark", "", $t[1], "", '```'
    $lines += $r.Output
    $lines += '```', ""
}
}

$lines += '## M1 measurements', ''
if ($ReplayOffCsv) { $lines += Compare-PresentMon $ReplayOffCsv $ReplayOnCsv }
else { $lines += '- PENDING: game FPS cost (provide -ReplayOffCsv and -ReplayOnCsv).' }
if ($ReportOnly -or -not $QualifyM1) {
    $lines += '- PENDING: 1080p60 two-minute save and memory measurements (run -QualifyM1).'
} else {
    $lines += '- Two-minute save and pre-save memory observations: BACKSIGHT_M1_METRICS in replay_session output above.',
        '- Test-process private working set includes the harness; subtracting buffer bytes is only an estimate, not engine-RAM qualification.',
        '- Full M1 qualification remains pending the game benchmark and manual checks below.'
}
$lines += ''

$lines += '## M2 measurements', '',
    '- Colour and encoder quality: BACKSIGHT_COLOUR and BACKSIGHT_QUALITY lines above (synthetic receipts: generated pictures through the real converter and encoders).',
    '- PENDING: game FPS, RAM and save time with recording on (real-device receipts): run ./scripts/recording-cost.ps1 as testing.md describes.',
    ''

$lines += "## By hand", "",
    "- [ ] Desktop audio with other Windows mix formats (Settings > System > Sound > output device > Format): 44.1 kHz, 96 kHz, and 7.1 if the device has it. For each, run ``cargo test --release -p backsight-engine --test loopback -- --ignored --nocapture`` and note whether it passes.",
    "- [ ] A clip saved with the default settings (crash-safe MP4) plays in sync in the Windows Photos app, a browser and Discord.",
    ""

$summary = $results | ForEach-Object { if ($_[1]) { "- pass: $($_[0])" } else { "- FAIL: $($_[0])" } }
$header + "## Summary" + "" + $summary + "" + $lines | Set-Content -Encoding utf8 $Report
Write-Host ""
$summary | ForEach-Object { Write-Host $_ }
Write-Host "Report: $Report"
if ($results | Where-Object { -not $_[1] }) { exit 1 }
