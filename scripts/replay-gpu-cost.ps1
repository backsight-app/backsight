# What Instant Replay costs while it just sits there: samples Windows' own GPU counters (the ones
# Task Manager shows) per engine type (3D, Copy, VideoEncode, ...), per graphics card, for
# Backsight, for its window (WebView2), for the Windows compositor (dwm.exe does the screen-capture
# copies, so capture cost shows up there, not under Backsight) and for everything together, plus
# Backsight's CPU use. Starts nothing and changes nothing: run it while the installed Backsight is
# in the state to measure, and keep the screen the same for every run (same windows, same game
# scene or an untouched desktop).
#
# Usage (Windows PowerShell 5.1 or PowerShell 7, no admin needed):
#   ./scripts/replay-gpu-cost.ps1 -Label off      # Backsight running, Instant Replay off
#   ./scripts/replay-gpu-cost.ps1 -Label before   # Instant Replay on, the build to compare against
#   ./scripts/replay-gpu-cost.ps1 -Label after    # Instant Replay on, the new build
#   ./scripts/replay-gpu-cost.ps1 -Compare off,before,after
# Options: -Seconds 30 (one sample a second), -StartDelay 5 (time to let go of the mouse),
#          -OutDir <folder> (default %TEMP%\backsight-gpu-cost; one <label>.json per run).
#
# Reading the table: each number is the average percentage of that engine over the run, summed
# over the group's processes. Task Manager's single "GPU" figure for a process is its busiest
# engine, so a process that only encodes video still shows a large number there. A sleepy
# graphics card runs at a low clock, which makes small work look like a big percentage: the
# nvidia-smi line (clock, power) tells the two apart.
# shortcut: counter names are the English ones, add PdhLookupPerfNameByIndex if a PC whose
# Windows is in another language needs measuring.
param(
    [string]$Label,
    [ValidateRange(5, 3600)][int]$Seconds = 30,
    [ValidateRange(0, 600)][int]$StartDelay = 5,
    [string]$Process = 'backsight',
    [string]$OutDir = (Join-Path ([IO.Path]::GetTempPath()) 'backsight-gpu-cost'),
    [string[]]$Compare
)
$ErrorActionPreference = 'Stop'

# 'pid_1234_luid_0x00000000_0x0000D2B4_phys_0_eng_3_engtype_VideoEncode' -> its parts, or nothing.
function ConvertFrom-GpuEngineInstance([string]$Name) {
    if ($Name -match '^pid_(\d+)_luid_(0x[0-9a-f]+)_(0x[0-9a-f]+)_phys_\d+_eng_\d+_engtype_(.+)$') {
        $Luid = ([Convert]::ToUInt64($Matches[2], 16) -shl 32) -bor [Convert]::ToUInt64($Matches[3], 16)
        [pscustomobject]@{ ProcessId = [int]$Matches[1]; Luid = $Luid; Engine = $Matches[4] }
    }
}

# Adds one sample set to $Totals ("group|card|engine" -> summed percentages). $Groups maps a
# process id to its group name; every process also counts towards 'Everything'.
function Add-GpuSample([hashtable]$Totals, $CounterSamples, [hashtable]$Groups, [hashtable]$Cards) {
    foreach ($Sample in $CounterSamples) {
        $Part = ConvertFrom-GpuEngineInstance $Sample.InstanceName
        if (-not $Part -or $Sample.CookedValue -le 0) { continue }
        $Card = $Cards[$Part.Luid]
        if (-not $Card) { $Card = 'card 0x{0:X}' -f $Part.Luid }
        foreach ($Group in @($Groups[$Part.ProcessId], 'Everything')) {
            if (-not $Group) { continue }
            $Key = "$Group|$Card|$($Part.Engine)"
            $Totals[$Key] = [double]$Totals[$Key] + $Sample.CookedValue
        }
    }
}

# Totals over $Count samples -> rows of average percentages, largest first within each group.
function ConvertTo-GpuRows([hashtable]$Totals, [int]$Count) {
    $Order = @{ 'Backsight' = 0; 'Backsight window' = 1; 'Windows compositor' = 2; 'Everything' = 3 }
    $Rows = foreach ($Key in $Totals.Keys) {
        $Group, $Card, $Engine = $Key -split '\|', 3
        [pscustomobject]@{ Group = $Group; Card = $Card; Engine = $Engine; Percent = [math]::Round($Totals[$Key] / $Count, 2) }
    }
    @($Rows | Sort-Object @{ Expression = { $Order[$_.Group] } }, @{ Expression = 'Percent'; Descending = $true })
}

# Several runs side by side: one line per group, card and engine that reached 0.05% in any run.
function Format-GpuComparison($Runs) {
    $Keys = [ordered]@{}
    foreach ($Run in $Runs) {
        foreach ($Row in $Run.Rows) { $Keys["$($Row.Group)|$($Row.Card)|$($Row.Engine)"] = $true }
    }
    $Lines = foreach ($Key in $Keys.Keys) {
        $Group, $Card, $Engine = $Key -split '\|', 3
        $Line = [ordered]@{ Who = $Group; Card = $Card; Engine = $Engine }
        $Seen = $false
        foreach ($Run in $Runs) {
            $Row = @($Run.Rows | Where-Object { $_.Group -eq $Group -and $_.Card -eq $Card -and $_.Engine -eq $Engine })
            $Value = 0.0
            if ($Row.Count) { $Value = [double]$Row[0].Percent }
            if ($Value -ge 0.05) { $Seen = $true }
            $Line["$($Run.Label) %"] = $Value.ToString('F1', [Globalization.CultureInfo]::InvariantCulture)
        }
        if ($Seen) { [pscustomobject]$Line }
    }
    $Cpu = [ordered]@{ Who = 'Backsight'; Card = '-'; Engine = 'CPU (100 = one core)' }
    foreach ($Run in $Runs) {
        $Cpu["$($Run.Label) %"] = ([double]$Run.CpuPercent).ToString('F1', [Globalization.CultureInfo]::InvariantCulture)
    }
    @($Lines) + [pscustomobject]$Cpu
}

# Graphics cards by the id the counters use. Windows keeps one key per card here.
function Get-GpuCardNames {
    $Cards = @{}
    $Keys = Get-ChildItem 'HKLM:\SOFTWARE\Microsoft\DirectX' -ErrorAction SilentlyContinue
    foreach ($Key in $Keys) {
        $Luid = $Key.GetValue('AdapterLuid')
        $Name = $Key.GetValue('Description')
        if ($null -ne $Luid -and $Name) { $Cards[[uint64]$Luid] = [string]$Name }
    }
    $Cards
}

function Get-CpuSeconds([int[]]$Ids) {
    $Total = 0.0
    foreach ($P in @(Get-Process -Id $Ids -ErrorAction SilentlyContinue)) { $Total += $P.TotalProcessorTime.TotalSeconds }
    $Total
}

# Dot-sourced by replay-gpu-cost.check.ps1 for the functions above only.
if ($MyInvocation.InvocationName -eq '.') { return }

if ($Compare) {
    $Runs = foreach ($Item in ($Compare -split ',')) {
        $Path = $Item
        if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { $Path = Join-Path $OutDir "$Item.json" }
        if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { throw "No saved run '$Item' (looked for $Path)." }
        Get-Content -LiteralPath $Path -Raw | ConvertFrom-Json
    }
    Format-GpuComparison @($Runs) | Format-Table -AutoSize
    foreach ($Run in $Runs) {
        if ($Run.Nvidia) { Write-Host "$($Run.Label): NVIDIA card $($Run.Nvidia)" }
        if (-not $Run.BacksightRunning) { Write-Host "$($Run.Label): Backsight wasn't running" }
    }
    return
}

if (-not $Label -or $Label -notmatch '^[\w-]+$') { throw 'Give the run a name: -Label off, -Label before or -Label after (letters, digits, - and _).' }
$Backsight = @(Get-Process -Name $Process -ErrorAction SilentlyContinue)
$Groups = @{}
foreach ($P in @(Get-Process -Name dwm -ErrorAction SilentlyContinue)) { $Groups[$P.Id] = 'Windows compositor' }
foreach ($P in $Backsight) {
    $Groups[$P.Id] = 'Backsight'
    # The window is WebView2: child processes of Backsight, gone while the window is closed.
    $Children = @(Get-CimInstance Win32_Process -Filter "ParentProcessId = $($P.Id)" -ErrorAction SilentlyContinue)
    while ($Children.Count) {
        $Next = @()
        foreach ($Child in $Children) {
            if ($Groups.ContainsKey([int]$Child.ProcessId)) { continue }
            $Groups[[int]$Child.ProcessId] = 'Backsight window'
            $Next += @(Get-CimInstance Win32_Process -Filter "ParentProcessId = $($Child.ProcessId)" -ErrorAction SilentlyContinue)
        }
        $Children = $Next
    }
}
if (-not $Backsight.Count) { Write-Warning "$Process isn't running: measuring only the compositor and the whole PC." }
$Cards = Get-GpuCardNames
$Smi = Get-Command nvidia-smi -ErrorAction SilentlyContinue

if ($StartDelay) {
    Write-Host "Measuring for $Seconds s in $StartDelay s. Leave the mouse and keyboard alone."
    Start-Sleep -Seconds $StartDelay
}
$Totals = @{}
$SmiTotals = @(0.0, 0.0, 0.0, 0.0)
$SmiCount = 0
$CpuIds = [int[]]@($Backsight | ForEach-Object { $_.Id })
$CpuStart = Get-CpuSeconds $CpuIds
$Watch = [Diagnostics.Stopwatch]::StartNew()
$Count = 0
# One sample a second; each is Windows' own average over that second.
Get-Counter '\GPU Engine(*)\Utilization Percentage' -SampleInterval 1 -MaxSamples $Seconds -ErrorAction SilentlyContinue |
    ForEach-Object {
        Add-GpuSample $Totals $_.CounterSamples $Groups $Cards
        $Count++
        if ($Smi) {
            # Whole-card figures: busy %, encoder %, clock in MHz, power in W ([N/A] on some laptops).
            $Line = & $Smi.Source '--query-gpu=utilization.gpu,utilization.encoder,clocks.gr,power.draw' '--format=csv,noheader,nounits' 2>$null |
                Select-Object -First 1
            $Fields = @("$Line" -split ',')
            if ($Fields.Count -eq 4) {
                for ($i = 0; $i -lt 4; $i++) {
                    $Number = 0.0
                    if ([double]::TryParse($Fields[$i].Trim(), [Globalization.NumberStyles]::Float, [Globalization.CultureInfo]::InvariantCulture, [ref]$Number)) {
                        $SmiTotals[$i] += $Number
                    }
                }
                $SmiCount++
            }
        }
    }
$Elapsed = $Watch.Elapsed.TotalSeconds
if (-not $Count) { throw "Windows gave no GPU counters ('\GPU Engine(*)\Utilization Percentage'). They need Windows 10 1709 or later and an English counter set." }
$CpuPercent = 0.0
if ($CpuIds.Count) { $CpuPercent = [math]::Round(100 * ((Get-CpuSeconds $CpuIds) - $CpuStart) / $Elapsed, 1) }
$Nvidia = $null
if ($SmiCount) {
    $Nvidia = 'busy {0:F0}%, encoder {1:F0}%, clock {2:F0} MHz, power {3:F1} W' -f `
        ($SmiTotals[0] / $SmiCount), ($SmiTotals[1] / $SmiCount), ($SmiTotals[2] / $SmiCount), ($SmiTotals[3] / $SmiCount)
}
$Run = [pscustomobject]@{
    Label = $Label
    When = (Get-Date).ToString('s')
    Seconds = [math]::Round($Elapsed, 1)
    Samples = $Count
    BacksightRunning = [bool]$Backsight.Count
    CpuPercent = $CpuPercent
    Nvidia = $Nvidia
    Rows = ConvertTo-GpuRows $Totals $Count
}
New-Item -ItemType Directory -Force $OutDir | Out-Null
$File = Join-Path $OutDir "$Label.json"
$Run | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $File -Encoding utf8
Format-GpuComparison @($Run) | Format-Table -AutoSize
if ($Nvidia) { Write-Host "NVIDIA card as a whole: $Nvidia" }
Write-Host "Saved $File. Compare runs with: ./scripts/replay-gpu-cost.ps1 -Compare off,before,after"
