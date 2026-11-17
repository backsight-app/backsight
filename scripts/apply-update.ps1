# Installs a build staged by build-desktop.ps1. Backsight starts this from its Update button
# and then quits; this waits for it to close, copies the new files over and reopens it.
# Lives in the staged folder (dist-desktop/next) and runs from there.
param([int]$WaitPid = 0, [switch]$NoStart)
$ErrorActionPreference = 'Stop'
$Next = $PSScriptRoot
$App = Join-Path (Split-Path -Parent $Next) 'Backsight'
$Log = Join-Path $env:TEMP 'backsight-update.log'
# Quitting can take a while when a recording is still being finished.
# WaitForExit, not Get-Process afterwards: a process that just exited can still be listed for a
# moment while Windows tears it down, which once looked like "did not close".
$Waiting = if ($WaitPid) { Get-Process -Id $WaitPid -ErrorAction SilentlyContinue }
if ($Waiting -and -not $Waiting.WaitForExit(300000)) {
    # Still running: starting another copy would give two Backsight at once.
    "$(Get-Date -Format s) Backsight did not close; nothing installed" | Add-Content $Log
    exit 1
}
try {
    New-Item -ItemType Directory -Force -Path $App | Out-Null
    foreach ($File in Get-ChildItem -LiteralPath $Next -File | Where-Object Name -NotIn 'apply-update.ps1', 'update-error.txt') {
        $Target = Join-Path $App $File.Name
        # Copy beside, then swap: a failed copy never leaves a half-written backsight.exe. Retries
        # cover the moment Windows still holds the exe just after Backsight exits.
        for ($Try = 1; ; $Try++) {
            try {
                Copy-Item -LiteralPath $File.FullName -Destination "$Target.new" -Force
                Move-Item -LiteralPath "$Target.new" -Destination $Target -Force
                break
            } catch {
                if ($Try -ge 5) { throw }
                Start-Sleep -Seconds 1
            }
        }
    }
    Remove-Item -LiteralPath $Next -Recurse -Force
    "$(Get-Date -Format s) updated" | Add-Content $Log
} catch {
    "$(Get-Date -Format s) $_" | Add-Content $Log
    # The staged build stays, and Home shows this instead of offering the same update silently.
    if (Test-Path -LiteralPath $Next) {
        [IO.File]::WriteAllText((Join-Path $Next 'update-error.txt'), "The last update didn't install: $_")
    }
}
if (-not $NoStart) { Start-Process -FilePath (Join-Path $App 'backsight.exe') -WorkingDirectory $App }
