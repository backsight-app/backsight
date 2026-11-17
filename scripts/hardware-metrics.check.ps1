$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'hardware-metrics.ps1')
$Temp = Join-Path ([IO.Path]::GetTempPath()) ('backsight-metrics-' + [guid]::NewGuid())
New-Item -ItemType Directory $Temp | Out-Null
$Off = Join-Path $Temp 'off.csv'
$On = Join-Path $Temp 'on.csv'
function Capture([string]$Path, [double[]]$Intervals) {
    $Rows = foreach ($Ms in $Intervals) {
        [pscustomobject]@{ Application = 'game.exe'; ProcessID = 123; SwapChainAddress = '0x1';
            MsBetweenPresents = $Ms.ToString('R', [Globalization.CultureInfo]::InvariantCulture) }
    }
    $Rows | Export-Csv -LiteralPath $Path -NoTypeInformation
}
function Reject([scriptblock]$Action) {
    $Failed = $false
    try { & $Action | Out-Null } catch { $Failed = $true }
    if (-not $Failed) { throw 'Invalid capture was accepted.' }
}
try {
    Capture $Off (@(10.0) * 2000 + @(30.0) * 1000)
    Capture $On (@(20.0) * 2500)
    $Parsed = Read-PresentMon $Off
    if ([math]::Abs($Parsed.Fps - 60) -gt 0.00001) { throw 'FPS must use total frames / total time.' }
    $Report = Compare-PresentMon $Off $On
    if (($Report -join "`n") -notmatch '16[.,]67%') { throw 'Wrong replay FPS cost.' }
    $Culture = [Threading.Thread]::CurrentThread.CurrentCulture
    try {
        [Threading.Thread]::CurrentThread.CurrentCulture = [Globalization.CultureInfo]::GetCultureInfo('fr-FR')
        if ((Read-PresentMon $Off).Fps -ne 60) { throw 'CSV numbers must ignore the machine locale.' }
    } finally { [Threading.Thread]::CurrentThread.CurrentCulture = $Culture }
    Reject { Compare-PresentMon $Off $Off }
    Capture $On @(20.0, 20.0)
    Reject { Read-PresentMon $On }
    Capture $On (@(20.0) * 2000 + @([double]::NaN))
    Reject { Read-PresentMon $On }
    Capture $On (@(20.0) * 2000 + @([double]::PositiveInfinity))
    Reject { Read-PresentMon $On }
    Capture $On (@(20.0) * 2000 + @(-1.0))
    Reject { Read-PresentMon $On }
    Capture $On @(1e308, 1e308)
    Reject { Read-PresentMon $On }
    Capture $On (@(20.0) * 2000)
    $Rows = @(Import-Csv -LiteralPath $On)
    $Rows[0].ProcessID = 456
    $Rows | Export-Csv -LiteralPath $On -NoTypeInformation
    Reject { Read-PresentMon $On }
    $Rows[0].ProcessID = 123
    $Rows[0].SwapChainAddress = '0x2'
    $Rows | Export-Csv -LiteralPath $On -NoTypeInformation
    Reject { Read-PresentMon $On }
    # The runner names the game's PID: a stray short swap chain is skipped, another process is not taken.
    if ((Read-PresentMon $On -ExpectedProcessId 123 -ExpectedApplication 'game.exe').Fps -ne 50) { throw 'Pick the usable game stream.' }
    Reject { Read-PresentMon $On -ExpectedProcessId 999 -ExpectedApplication 'game.exe' }
    # A runner folder's game.pid lets Compare-PresentMon skip the stray stream too.
    Reject { Compare-PresentMon $Off $On }
    Set-Content -LiteralPath (Join-Path $Temp 'game.pid') -Value '123'
    if ((Compare-PresentMon $Off $On) -join "`n" -notmatch '16[.,]67%') { throw 'Compare must use game.pid.' }
    Remove-Item -LiteralPath (Join-Path $Temp 'game.pid')
    $Rows | Select-Object Application, ProcessID, SwapChainAddress | Export-Csv -LiteralPath $On -NoTypeInformation
    Reject { Read-PresentMon $On }
    'PresentMon parser checks passed: weighted FPS, cost, locale, same file, short, NaN, infinity, negative, overflow, mixed streams, game PID, missing columns.'
} finally {
    Remove-Item -LiteralPath $Off, $On -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $Temp
}
