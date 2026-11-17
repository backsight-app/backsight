# What Instant Replay costs at each setting, in one table: runs the engine's cost test
# (crates/backsight-engine/tests/gpu_cost.rs, `run_spec`) once per row with settings built from a
# line of words, and samples Windows' GPU counters per engine, nvidia-smi (clock, power) and CPU
# while it runs. It never touches the installed Backsight or its settings file.
#
# Scenes: 'still' (whatever is on screen; leave it alone), 'moving' (the test's own animated
# window, one picture per refresh) and 'game' (the same window drawn flat out: its pictures per
# second, with a row's settings and with 'off', are what a game would feel).
#
# Build first: cargo test -p backsight-engine --test gpu_cost --no-run
# Usage: ./scripts/replay-cost-matrix.ps1 [-Scenes still,game] [-Seconds 15] [-Specs 'off','fps=60 bitrate=20000']
#        [-SceneMonitor 1] [-Load 40] [-OutFile table.txt]   (monitor numbers: the test `list_monitors`)
# Yardstick: turn another recorder on (NVIDIA App's Instant Replay at matching settings) and run
#   ./scripts/replay-cost-matrix.ps1 -Specs off -Yardstick nvcontainer,'NVIDIA Overlay' [-SceneMonitor 1]
# once with it on and once with it off: the Enc%/3D% columns then show its processes, and
# GameFps what the stand-in game loses to it.
# A spec is `key=value` words: encoder, fps, resolution, hdr, monitor, audio, and any encoder option
# (bitrate, preset, bframes, rc, tune, ...). 'off' runs no session (the baseline).
param(
    [string]$Exe,
    [ValidateRange(5, 600)][int]$Seconds = 15,
    [string[]]$Scenes = @('still', 'game'),
    [string[]]$Specs,
    [int]$Load = 40,
    [int]$SceneMonitor = -1,
    [int]$WarmUp = 60,
    [string]$OutFile,
    [string[]]$Yardstick = @(),
    [string[]]$BusyGames = @()
)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'replay-gpu-cost.ps1')

if (-not $Exe) {
    $Target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $PSScriptRoot '..\target' }
    $Found = Get-ChildItem (Join-Path $Target 'debug') -Recurse -Filter 'gpu_cost-*.exe' -ErrorAction SilentlyContinue |
        Sort-Object LastWriteTime | Select-Object -Last 1
    if (-not $Found) { throw 'Build the cost test first: cargo test -p backsight-engine --test gpu_cost --no-run' }
    $Exe = $Found.FullName
}
# The test loads FFmpeg's DLLs from the PATH, as `cargo test` arranges by itself.
$env:PATH = (Join-Path $PSScriptRoot '..\third_party\ffmpeg\bin') + ';' + $env:PATH
if (-not $Specs) {
    $Usual = 'encoder=nvenc-hevc resolution=1080p'
    $Specs = @(
        'off'
        "$Usual fps=60 bitrate=20000"
        "$Usual fps=30 bitrate=5000"
        "$Usual fps=60 bitrate=5000"
        "$Usual fps=60 bitrate=50000 max_bitrate=75000"
        "$Usual fps=30 bitrate=20000"
        "$Usual fps=60 bitrate=20000 bframes=0"
        "$Usual fps=60 bitrate=20000 bframes=2"
        "$Usual fps=60 bitrate=20000 rc=cbr"
        "$Usual fps=60 rc=cq"
        "$Usual fps=60 bitrate=20000 preset=p4"
        "$Usual fps=60 bitrate=20000 preset=p7"
        "$Usual fps=60 bitrate=20000 tune=ll"
        "$Usual fps=60 bitrate=20000 tune=ull"
        "$Usual fps=60 bitrate=20000 lookahead=true"
        "$Usual fps=60 bitrate=20000 spatial_aq=true temporal_aq=true"
        "$Usual fps=60 bitrate=20000 multipass=qres"
        'encoder=nvenc-hevc resolution=source fps=60 bitrate=20000'
        'encoder=nvenc-hevc resolution=720p fps=60 bitrate=20000'
        'encoder=nvenc-h264 resolution=1080p fps=60 bitrate=20000'
        'off'
    )
}
$Work = Join-Path ([IO.Path]::GetTempPath()) 'backsight-cost-matrix'
New-Item -ItemType Directory -Force $Work | Out-Null
$Cards = Get-GpuCardNames
$Smi = Get-Command nvidia-smi -ErrorAction SilentlyContinue

function Start-CostTest([string]$Test, [hashtable]$Environment, [string]$Log) {
    foreach ($Name in $Environment.Keys) { Set-Item "env:$Name" $Environment[$Name] }
    try {
        Start-Process -FilePath $Exe -ArgumentList $Test, '--exact', '--ignored', '--nocapture' `
            -RedirectStandardOutput $Log -RedirectStandardError "$Log.err" -WindowStyle Hidden -PassThru
    } finally {
        foreach ($Name in $Environment.Keys) { Remove-Item "env:$Name" }
    }
}

# Sum of one engine's percentages for a group, over every card (or the cards matching $Card).
function Get-Engine([hashtable]$Totals, [int]$Count, [string]$Group, [string]$Engine, [string]$Card = '') {
    $Sum = 0.0
    foreach ($Key in $Totals.Keys) {
        $G, $C, $E = $Key -split '\|', 3
        if ($G -eq $Group -and $E -eq $Engine -and $C -match $Card) { $Sum += $Totals[$Key] }
    }
    [math]::Round($Sum / [math]::Max($Count, 1), 1)
}

# Everything a group uses outside the four named engines (compute shaders show up as Compute_N).
function Get-Other([hashtable]$Totals, [int]$Count, [string]$Group) {
    $Sum = 0.0
    foreach ($Key in $Totals.Keys) {
        $G, $C, $E = $Key -split '\|', 3
        if ($G -eq $Group -and $E -notin '3D', 'Copy', 'VideoEncode', 'VideoDecode') { $Sum += $Totals[$Key] }
    }
    [math]::Round($Sum / [math]::Max($Count, 1), 1)
}

$Rows = foreach ($Scene in $Scenes) {
    $SceneProcess = $null
    $SceneLog = Join-Path $Work "scene-$Scene.log"
    if ($Scene -ne 'still') {
        $Vsync = if ($Scene -eq 'moving') { '1' } else { '0' }
        $Life = [string](($Seconds + 40) * $Specs.Count + 30)
        $SceneEnv = @{ BACKSIGHT_SCENE_SECONDS = $Life; BACKSIGHT_SCENE_LOAD = "$Load"; BACKSIGHT_SCENE_VSYNC = $Vsync }
        if ($SceneMonitor -ge 0) { $SceneEnv.BACKSIGHT_SCENE_MONITOR = "$SceneMonitor" }
        $SceneProcess = Start-CostTest 'moving_scene' $SceneEnv $SceneLog
        Start-Sleep -Seconds $WarmUp # the card takes a while to reach its full clock, then its steady heat
    }
    try {
        foreach ($Spec in $Specs) {
            if (@(Get-Process | Where-Object { $BusyGames -contains $_.Name }).Count) { throw 'A game is running: not measuring.' }
            $Session = $null
            $Log = Join-Path $Work 'session.log'
            $Stop = Join-Path $Work 'session.stop'
            if ($Spec -ne 'off') {
                Remove-Item $Log -ErrorAction SilentlyContinue
                Remove-Item $Stop -ErrorAction SilentlyContinue
                $Session = Start-CostTest 'run_spec' @{ BACKSIGHT_COST_SPEC = $Spec; BACKSIGHT_EXP_SECONDS = "$($Seconds * 4 + 60)"; BACKSIGHT_COST_STOP = $Stop } $Log
                $Wait = [Diagnostics.Stopwatch]::StartNew()
                while (-not (Select-String -LiteralPath $Log -Pattern 'BACKSIGHT_COST_READY' -Quiet -ErrorAction SilentlyContinue)) {
                    if ($Session.HasExited -or $Wait.Elapsed.TotalSeconds -gt 90) {
                        throw "The session for '$Spec' didn't start: $(Get-Content "$Log.err" -Raw -ErrorAction SilentlyContinue)"
                    }
                    Start-Sleep -Milliseconds 200
                }
            }
            Start-Sleep -Seconds 3 # let the encoder and the card's clock settle
            $Groups = @{}
            foreach ($P in @(Get-Process -Name dwm -ErrorAction SilentlyContinue)) { $Groups[$P.Id] = 'Windows compositor' }
            if ($Session) { $Groups[$Session.Id] = 'Backsight' }
            # Another recorder as the yardstick: its processes fill the same columns.
            $Watched = @(Get-Process | Where-Object { $Yardstick -contains $_.Name })
            foreach ($P in $Watched) { $Groups[$P.Id] = 'Backsight' }
            if ($SceneProcess) { $Groups[$SceneProcess.Id] = 'Scene' }
            $Totals = @{}
            $SmiTotals = @(0.0, 0.0, 0.0, 0.0)
            $Count = 0
            $CpuStart = if ($Session) { Get-CpuSeconds @($Session.Id) } else { 0.0 }
            $From = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
            $Watch = [Diagnostics.Stopwatch]::StartNew()
            Get-Counter '\GPU Engine(*)\Utilization Percentage' -SampleInterval 1 -MaxSamples $Seconds -ErrorAction SilentlyContinue |
                ForEach-Object {
                    Add-GpuSample $Totals $_.CounterSamples $Groups $Cards
                    $Count++
                    if ($Smi) {
                        $Line = & $Smi.Source '--query-gpu=utilization.gpu,utilization.encoder,clocks.gr,power.draw' '--format=csv,noheader,nounits' 2>$null |
                            Select-Object -First 1
                        $Fields = @("$Line" -split ',')
                        for ($i = 0; $i -lt 4 -and $Fields.Count -eq 4; $i++) {
                            $Number = 0.0
                            if ([double]::TryParse($Fields[$i].Trim(), [Globalization.NumberStyles]::Float, [Globalization.CultureInfo]::InvariantCulture, [ref]$Number)) {
                                $SmiTotals[$i] += $Number
                            }
                        }
                    }
                }
            $Elapsed = $Watch.Elapsed.TotalSeconds
            $To = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
            $Cpu = if ($Session) { [math]::Round(100 * ((Get-CpuSeconds @($Session.Id)) - $CpuStart) / $Elapsed, 1) } else { 0.0 }
            $Result = ''
            if ($Session) {
                Set-Content -LiteralPath $Stop -Value 'stop'
                if (-not $Session.WaitForExit(60000)) { throw "The session for '$Spec' didn't end." }
                $Result = "$(Select-String -LiteralPath $Log -Pattern '^COST_RESULT (.*)$' | ForEach-Object { $_.Matches[0].Groups[1].Value })"
                if (-not $Result) { throw "The session for '$Spec' failed: $(Get-Content "$Log.err" -Raw -ErrorAction SilentlyContinue)" }
            }
            $Fps = ''
            if ($SceneProcess) {
                if ($SceneProcess.HasExited) { throw 'The scene window closed early.' }
                $Seen = @(Get-Content $SceneLog | ForEach-Object {
                        if ($_ -match '^SCENE (\d+) ([\d.]+)$' -and [long]$Matches[1] -ge $From -and [long]$Matches[1] -le $To) { [double]$Matches[2] }
                    })
                if ($Seen.Count) { $Fps = [math]::Round(($Seen | Measure-Object -Average).Average, 1) }
            }
            $N = [math]::Max($Count, 1)
            $Row = [pscustomobject][ordered]@{
                Scene     = $Scene
                Spec      = $Spec
                'Enc%'    = Get-Engine $Totals $Count 'Backsight' 'VideoEncode'
                '3D%'     = Get-Engine $Totals $Count 'Backsight' '3D'
                'Copy%'   = Get-Engine $Totals $Count 'Backsight' 'Copy'
                'Dec%'    = Get-Engine $Totals $Count 'Backsight' 'VideoDecode'
                'Oth%'    = Get-Other $Totals $Count 'Backsight'
                'Dwm%'    = (Get-Engine $Totals $Count 'Windows compositor' '3D') + (Get-Engine $Totals $Count 'Windows compositor' 'Copy')
                'Game3D%' = Get-Engine $Totals $Count 'Scene' '3D'
                'Nv3D%'   = Get-Engine $Totals $Count 'Everything' '3D' 'NVIDIA'
                'CPU%'    = $Cpu
                SmiBusy   = [math]::Round($SmiTotals[0] / $N)
                SmiEnc    = [math]::Round($SmiTotals[1] / $N)
                MHz       = [math]::Round($SmiTotals[2] / $N)
                Watts     = [math]::Round($SmiTotals[3] / $N, 1)
                GameFps   = $Fps
                Out       = $Result
            }
            Write-Host ($Row | Format-List | Out-String).Trim() -ForegroundColor DarkGray
            Write-Host ''
            $Row
            Start-Sleep -Seconds 2
        }
    } finally {
        if ($SceneProcess -and -not $SceneProcess.HasExited) { Stop-Process -Id $SceneProcess.Id -Force }
    }
}
$Table = $Rows | Format-Table -Property * -AutoSize | Out-String -Width 500
Write-Host $Table
if ($OutFile) { $Table | Set-Content -LiteralPath $OutFile -Encoding utf8 }
