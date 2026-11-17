# Writes THIRD-PARTY-NOTICES.txt: the licenses of everything that ships inside Backsight.
# -Check writes nothing and fails when the committed file is out of date (CI, installer builds).
# Needs cargo-about 0.9 (`cargo install cargo-about --locked --features cli`) and app/node_modules
# (`cd app; npm ci`). Hand-written parts and settings are in scripts/third-party-notices/.
param([switch]$Check)
$ErrorActionPreference = 'Stop'
$ProjectRoot = Split-Path -Parent $PSScriptRoot
$Parts = Join-Path $PSScriptRoot 'third-party-notices'
$Output = Join-Path $ProjectRoot 'THIRD-PARTY-NOTICES.txt'
# node prints UTF-8; read it as such (license texts hold accented names and the copyright sign).
$PreviousEncoding = [Console]::OutputEncoding
[Console]::OutputEncoding = New-Object Text.UTF8Encoding $false

if (-not (Get-Command cargo-about -ErrorAction SilentlyContinue)) {
    throw 'cargo-about is missing. Install it with: cargo install cargo-about --locked --features cli'
}

# Rust crates: only what is compiled into backsight.exe for 64-bit Windows (about.toml).
$Json = Join-Path ([IO.Path]::GetTempPath()) "backsight-about-$PID.json"
try {
    cargo about generate --fail --locked --format json -c (Join-Path $Parts 'about.toml') `
        -m (Join-Path $ProjectRoot 'app/src-tauri/Cargo.toml') -o $Json
    if ($LASTEXITCODE -ne 0) { throw 'cargo-about failed. See its output above.' }
    $Crates = node (Join-Path $Parts 'crates.mjs') $Json
    if ($LASTEXITCODE -ne 0) { throw 'The Rust crates section could not be written. See the output above.' }
} finally {
    Remove-Item -LiteralPath $Json -ErrorAction SilentlyContinue
}

# JavaScript: only the packages that end up in the built window (app/dist).
Push-Location (Join-Path $ProjectRoot 'app')
try {
    $Script = node scripts/bundled-licenses.mjs
    if ($LASTEXITCODE -ne 0) { throw 'The JavaScript section could not be written. See the output above.' }
} finally {
    Pop-Location
    [Console]::OutputEncoding = $PreviousEncoding
}

$Rule = '=' * 72
$Text = @(
    [IO.File]::ReadAllText((Join-Path $Parts 'header.txt'))
    [IO.File]::ReadAllText((Join-Path $Parts 'rust-std.txt'))
    [IO.File]::ReadAllText((Join-Path $Parts 'webview2-sdk.txt'))
    ($Crates -join "`n")
    "$Rule`n5. JavaScript in Backsight's window`n$Rule`n`nBacksight's window is built from these JavaScript packages.`n`n" + ($Script -join "`n")
    [IO.File]::ReadAllText((Join-Path $Parts 'nsis.txt'))
) | ForEach-Object { ($_ -replace "`r`n", "`n").TrimEnd() }
# LF line ends, as .gitattributes stores every text file.
$Text = ($Text -join "`n`n`n") + "`n"

if ($Check) {
    $Old = if (Test-Path -LiteralPath $Output) { [IO.File]::ReadAllText($Output) -replace "`r`n", "`n" } else { '' }
    if ($Old -ne $Text) {
        throw 'THIRD-PARTY-NOTICES.txt is out of date. Run scripts/third-party-notices.ps1 and commit the result.'
    }
    Write-Output 'THIRD-PARTY-NOTICES.txt is up to date.'
} else {
    # UTF-8 without the BOM that Set-Content adds in Windows PowerShell.
    [IO.File]::WriteAllText($Output, $Text, (New-Object Text.UTF8Encoding $false))
    Write-Output "Wrote $Output"
}
