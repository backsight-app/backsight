# PresentMon --v1_metrics: https://github.com/GameTechDev/PresentMon/blob/main/README-ConsoleApplication.md
function Read-PresentMon([string]$Path, [int]$ExpectedProcessId = 0, [string]$ExpectedApplication) {
    $Rows = @(Import-Csv -LiteralPath $Path)
    if ($Rows.Count -lt 2) { throw "Not enough frames in $Path" }
    foreach ($Column in @('Application', 'ProcessID', 'SwapChainAddress', 'MsBetweenPresents')) {
        if ($Rows[0].PSObject.Properties.Name -notcontains $Column) {
            throw "Missing $Column in $Path. Export PresentMon with --v1_metrics."
        }
    }
    $Streams = @($Rows | Group-Object Application, ProcessID, SwapChainAddress)
    if ($ExpectedProcessId) {
        foreach ($Stream in $Streams) {
            $First = $Stream.Group[0]
            if ($First.ProcessID -ne "$ExpectedProcessId") { continue }
            if ($ExpectedApplication -and $First.Application -ne $ExpectedApplication) { continue }
            try { return Measure-PresentMonStream $Stream.Group $Path } catch { continue }
        }
        throw "No usable 30-second game stream in $Path for $ExpectedApplication (PID $ExpectedProcessId)."
    }
    if ($Streams.Count -ne 1) { throw "Select one game process and swap chain in $Path before comparing." }
    Measure-PresentMonStream $Rows $Path
}

function Measure-PresentMonStream($Rows, [string]$Path) {
    $Total = 0.0
    $Frames = 0
    foreach ($Row in $Rows) {
        $Ms = 0.0
        if (-not [double]::TryParse($Row.MsBetweenPresents, [Globalization.NumberStyles]::Float,
            [Globalization.CultureInfo]::InvariantCulture, [ref]$Ms) -or
            [double]::IsNaN($Ms) -or [double]::IsInfinity($Ms) -or $Ms -lt 0) {
            throw "Invalid frame interval in $Path"
        }
        # The first frame has no preceding present and may report zero.
        if ($Ms -eq 0 -and $Frames -eq 0) { continue }
        if ($Ms -eq 0) { throw "Zero frame interval in $Path" }
        $Total += $Ms
        if ([double]::IsInfinity($Total)) { throw "Frame interval total overflow in $Path" }
        $Frames++
    }
    if ($Total -lt 30000) { throw "Capture at least 30 seconds of gameplay in $Path." }
    $Fps = 1000 * $Frames / $Total
    if ([double]::IsNaN($Fps) -or [double]::IsInfinity($Fps) -or $Fps -le 0) {
        throw "Invalid FPS in $Path"
    }
    [pscustomobject]@{ Application = $Rows[0].Application; Fps = $Fps; Seconds = $Total / 1000 }
}

# recording-cost.ps1 leaves game.pid beside frames.csv; use it so a stray second swap chain is ignored.
function Read-RunPresentMon([string]$Path) {
    $PidFile = Join-Path (Split-Path -Parent ([IO.Path]::GetFullPath($Path))) 'game.pid'
    if (Test-Path -LiteralPath $PidFile -PathType Leaf) {
        return Read-PresentMon $Path -ExpectedProcessId ([int](Get-Content -LiteralPath $PidFile -Raw).Trim())
    }
    Read-PresentMon $Path
}

function Compare-PresentMon([string]$OffCsv, [string]$OnCsv) {
    if ([IO.Path]::GetFullPath($OffCsv) -eq [IO.Path]::GetFullPath($OnCsv)) {
        throw 'Replay-off and replay-on captures must be different files.'
    }
    $Off = Read-RunPresentMon $OffCsv
    $On = Read-RunPresentMon $OnCsv
    if ($Off.Application -ne $On.Application) { throw 'The captures must show the same game.' }
    $Cost = 100 * (1 - $On.Fps / $Off.Fps)
    if ([double]::IsNaN($Cost) -or [double]::IsInfinity($Cost)) { throw 'FPS cost is outside the supported range.' }
    @(
        "- PresentMon application: $($Off.Application)",
        ('- Baseline: {0:F2} FPS ({1:F1} s); replay on: {2:F2} FPS ({3:F1} s)' -f $Off.Fps, $Off.Seconds, $On.Fps, $On.Seconds),
        ('- FPS cost: {0:F2}% (budget: under 5%; supplied captures only)' -f $Cost),
        "- Baseline CSV: $([IO.Path]::GetFullPath($OffCsv))",
        "- Replay-on CSV: $([IO.Path]::GetFullPath($OnCsv))",
        '- Qualification remains pending review of matching scene, settings, GPU load, and repeat runs.'
    )
}
