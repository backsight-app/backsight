# Self-check for replay-gpu-cost.ps1's counter parsing and tables, on made-up samples: touches no
# GPU and no counters. Run: ./scripts/replay-gpu-cost.check.ps1
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'replay-gpu-cost.ps1')
function Check([bool]$Ok, [string]$What) { if (-not $Ok) { throw "replay-gpu-cost check failed: $What" } }
function Sample([string]$Name, [double]$Value) { [pscustomobject]@{ InstanceName = $Name; CookedValue = $Value } }

$Part = ConvertFrom-GpuEngineInstance 'pid_1234_luid_0x00000001_0x0000d2b4_phys_0_eng_3_engtype_VideoEncode'
Check ($Part.ProcessId -eq 1234 -and $Part.Engine -eq 'VideoEncode' -and $Part.Luid -eq 0x10000D2B4) 'instance name parts'
Check ($null -eq (ConvertFrom-GpuEngineInstance '_total')) 'other instance names are skipped'

$Nvidia = [uint64]0xD2B4
$Cards = @{ $Nvidia = 'NVIDIA' }
$Groups = @{ 10 = 'Backsight'; 20 = 'Windows compositor' }
$Totals = @{}
# Two seconds: Backsight encodes on two engines of one type, dwm copies, a game renders.
foreach ($Second in 1..2) {
    Add-GpuSample $Totals @(
        (Sample 'pid_10_luid_0x00000000_0x0000d2b4_phys_0_eng_3_engtype_VideoEncode' 10),
        (Sample 'pid_10_luid_0x00000000_0x0000d2b4_phys_0_eng_4_engtype_VideoEncode' 2),
        (Sample 'pid_10_luid_0x00000000_0x0000d2b4_phys_0_eng_0_engtype_3D' 0),
        (Sample 'pid_20_luid_0x00000000_0x0000d2b4_phys_0_eng_1_engtype_Copy' (4 * $Second)),
        (Sample 'pid_30_luid_0x00000000_0x0000aaaa_phys_0_eng_0_engtype_3D' 50),
        (Sample '_total' 99)
    ) $Groups $Cards
}
$Rows = ConvertTo-GpuRows $Totals 2
$Find = { param($Group, $Engine) @($Rows | Where-Object { $_.Group -eq $Group -and $_.Engine -eq $Engine }) }
Check ((& $Find 'Backsight' 'VideoEncode')[0].Percent -eq 12) 'engines of one type add up, averaged over the samples'
Check ((& $Find 'Backsight' '3D').Count -eq 0) 'idle engines are left out'
Check ((& $Find 'Windows compositor' 'Copy')[0].Percent -eq 6) 'average of 4 and 8'
Check ((& $Find 'Everything' '3D')[0].Card -eq 'card 0xAAAA') 'an unnamed card shows its id'
Check ($Rows[0].Group -eq 'Backsight' -and $Rows[-1].Group -eq 'Everything') 'groups in reading order'

$Before = [pscustomobject]@{ Label = 'before'; CpuPercent = 7.5; Rows = $Rows }
$After = [pscustomobject]@{ Label = 'after'; CpuPercent = 3; Rows = @($Rows | Where-Object { $_.Engine -ne 'Copy' }) }
# Through JSON, as saved runs come back.
$Table = Format-GpuComparison @(($Before, $After) | ForEach-Object { $_ | ConvertTo-Json -Depth 4 | ConvertFrom-Json })
$Copy = @($Table | Where-Object { $_.Who -eq 'Windows compositor' -and $_.Engine -eq 'Copy' })[0]
Check ($Copy.'before %' -eq '6.0' -and $Copy.'after %' -eq '0.0') 'a row missing from one run reads as 0'
Check ($Table[-1].Engine -like 'CPU*' -and $Table[-1].'before %' -eq '7.5') 'CPU line last'
Write-Host 'replay-gpu-cost check passed.'
