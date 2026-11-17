# Compares criterion results saved as two baselines in the same target dir and fails on a big
# slowdown. Used by .github/workflows/bench.yml: base and head are benched on the same runner,
# twice each, and the faster of the two runs per side is compared (a noisy neighbour only ever
# makes a run slower, so the minimum is the least noisy number).
#   ./scripts/bench-compare.ps1 -Base base1,base2 -Head head1,head2 [-Threshold 30]
# Benchmarks listed in $Informational are shown but never fail the check: they involve threads
# and disk-free queues whose timing depends on how busy the shared runner is.
param(
    [Parameter(Mandatory)] [string[]] $Base,
    [Parameter(Mandatory)] [string[]] $Head,
    [double] $Threshold = 30,
    [string] $CriterionDir = "target/criterion"
)

$ErrorActionPreference = "Stop"
$Informational = @("recording_send_drain_256_packets")

# Median time in nanoseconds of every benchmark saved under `baseline`, by benchmark name.
function Get-Medians([string] $baseline) {
    $result = @{}
    Get-ChildItem $CriterionDir -Recurse -Filter estimates.json |
        Where-Object { $_.Directory.Name -eq $baseline } |
        ForEach-Object {
            # <criterion>/<group>/<bench>/<baseline>/estimates.json; the group/bench folder names
            # are the benchmark id with "/" turned into "_", so the name shows as e.g. pacer_144hz_source_to_60fps_1s.
            $id = $_.Directory.Parent.FullName.Substring((Resolve-Path $CriterionDir).Path.Length + 1) -replace '\\', '/'
            $result[$id] = (Get-Content $_.FullName -Raw | ConvertFrom-Json).median.point_estimate
        }
    $result
}

function Get-Best([string[]] $baselines) {
    $best = @{}
    foreach ($b in $baselines) {
        foreach ($kv in (Get-Medians $b).GetEnumerator()) {
            if (-not $best.ContainsKey($kv.Key) -or $kv.Value -lt $best[$kv.Key]) { $best[$kv.Key] = $kv.Value }
        }
    }
    $best
}

$before = Get-Best $Base
$after = Get-Best $Head
if ($before.Count -eq 0 -or $after.Count -eq 0) { throw "No criterion results found under $CriterionDir for these baselines." }

$lines = @("| Benchmark | Base | This change | Change | |", "|---|---|---|---|---|")
$failed = @()
foreach ($name in ($after.Keys | Sort-Object)) {
    if (-not $before.ContainsKey($name)) { $lines += "| ``$name`` | (new) | $([math]::Round($after[$name])) ns | | |"; continue }
    $pct = ($after[$name] / $before[$name] - 1) * 100
    $note = ""
    if ($pct -gt $Threshold) {
        if ($Informational -contains $name) { $note = "slower (not checked)" } else { $note = "TOO SLOW"; $failed += $name }
    }
    $lines += "| ``$name`` | $([math]::Round($before[$name])) ns | $([math]::Round($after[$name])) ns | $([math]::Round($pct, 1))% | $note |"
}

$text = "### Performance check`n`nSame runner, best of two runs per side. Fails above +$Threshold% on the checked benchmarks.`n`n" + ($lines -join "`n")
Write-Host $text
if ($env:GITHUB_STEP_SUMMARY) { Add-Content -Path $env:GITHUB_STEP_SUMMARY -Value $text -Encoding utf8 }
if ($failed.Count -gt 0) { throw "Slower than the base commit by more than $Threshold%: $($failed -join ', ')" }
