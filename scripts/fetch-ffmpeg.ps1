# Installs the pinned FFmpeg headers, import libraries, DLLs and test tools.
# Default: ./scripts/fetch-ffmpeg.ps1
# Candidate: ./scripts/fetch-ffmpeg.ps1 -SlimArchive <zip> -SlimSha256 <expected SHA-256>
# The candidate goes to target/ffmpeg-slim for isolated verification.
[CmdletBinding(DefaultParameterSetName = "Full")]
param(
    [Parameter(Mandatory, ParameterSetName = "Slim")]
    [string] $SlimArchive,
    [Parameter(Mandatory, ParameterSetName = "Slim")]
    [ValidatePattern('^[0-9a-fA-F]{64}$')]
    [string] $SlimSha256
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
$Slim = $PSCmdlet.ParameterSetName -eq "Slim"
if ($Slim) {
    $Version = "8.1.3 slim"
    $Dest = Join-Path $Root "target/ffmpeg-slim"
    $Zip = (Get-Item -LiteralPath $SlimArchive).FullName
    $Sha256 = $SlimSha256
} else {
    # Backsight's slim build (.github/workflows/ffmpeg.yml), published with its sources. CI, the
    # tests and the app all use it. It has no libvmaf, so quality.rs scores PSNR and SSIM only.
    $Version = "8.1.3 slim"
    $Repo = "backsight-app/backsight"
    $Tag = "ffmpeg-8.1.3-slim-r1"
    $Asset = "ffmpeg-8.1.3-backsight-slim-win64-gpl-shared.zip"
    $Url = "https://github.com/$Repo/releases/download/$Tag/$Asset"
    $Sha256 = "7d4797c783fef2f5d073c8d7679c43cfcd3640da30420bdc52e39032df7578da"
    # Fallback until the slim build has proven itself (and for VMAF scores): the full GPL build,
    # about 245 MB of DLLs. Never build the installer from it.
    # $Version = "8.1"; $Repo = $null
    # $Asset = "ffmpeg-$Version-full_build-shared.zip"
    # $Url = "https://github.com/GyanD/codexffmpeg/releases/download/$Version/$Asset"
    # $Sha256 = "e184e012ca9a3527ed45d61f8e250c50b07b8661d90849820ea754239049efb4"
    $Dest = Join-Path $Root "third_party/ffmpeg"
    $Zip = Join-Path $env:TEMP $Asset
}

$Header = "include/libavcodec/avcodec.h"
function Assert-FfmpegLayout($Directory) {
    $RequiredFiles = @($Header, "bin/ffmpeg.exe", "bin/ffprobe.exe")
    foreach ($Library in @("avcodec-62", "avdevice-62", "avfilter-11", "avformat-62", "avutil-60", "swresample-6", "swscale-9")) {
        $RequiredFiles += "bin/$Library.dll"
        $RequiredFiles += "lib/$($Library.Split('-')[0]).lib"
    }
    foreach ($Required in $RequiredFiles) {
        if (-not (Test-Path -LiteralPath (Join-Path $Directory $Required) -PathType Leaf)) {
            throw "FFmpeg directory '$Directory' is missing '$Required'."
        }
    }
}
$Stamp = Join-Path $Dest '.sha256'
if (-not $Slim -and (Test-Path -LiteralPath (Join-Path $Dest $Header) -PathType Leaf)) {
    Assert-FfmpegLayout $Dest
    # An unstamped or differently stamped folder is another FFmpeg (for example the old full build).
    if (-not (Test-Path -LiteralPath $Stamp -PathType Leaf) -or (Get-Content -LiteralPath $Stamp -Raw).Trim() -ne $Sha256) {
        throw "'$Dest' holds a different FFmpeg. Move it aside (for a link, 'cmd /c rmdir' it) and rerun."
    }
    Write-Host "FFmpeg $Version already in $Dest"
    exit 0
}

if (-not $Slim -and ((-not (Test-Path -LiteralPath $Zip -PathType Leaf)) -or
    (Get-FileHash -LiteralPath $Zip -Algorithm SHA256).Hash -ne $Sha256)) {
    Write-Host "Downloading FFmpeg $Version..."
    $Download = "$Zip.$([guid]::NewGuid()).part"
    try {
        try {
            Invoke-WebRequest $Url -OutFile $Download -UseBasicParsing
        } catch {
            # A private repository's release needs a signed-in gh (CI passes GH_TOKEN).
            if (-not $Repo) { throw }
            if (-not (Get-Command gh -ErrorAction SilentlyContinue)) {
                throw "Couldn't download $Asset ($($_.Exception.Message)). Install GitHub CLI (winget install GitHub.cli), run 'gh auth login' and retry."
            }
            gh release download $Tag --repo $Repo --pattern $Asset --output $Download --clobber
            if ($LASTEXITCODE) { throw "Couldn't download $Asset. Run 'gh auth login' and retry." }
        }
        if ((Get-FileHash -LiteralPath $Download -Algorithm SHA256).Hash -ne $Sha256) {
            throw "Downloaded FFmpeg checksum mismatch. The cache was not changed."
        }
        Move-Item -LiteralPath $Download -Destination $Zip -Force
    } finally {
        if (Test-Path -LiteralPath $Download) { Remove-Item -LiteralPath $Download }
    }
}

$Actual = (Get-FileHash -LiteralPath $Zip -Algorithm SHA256).Hash.ToLowerInvariant()
if ($Actual -ne $Sha256) {
    throw "Checksum mismatch for '$Zip'. Expected $Sha256, got $Actual."
}

if (-not $Slim -and (Test-Path -LiteralPath $Dest)) {
    throw "FFmpeg destination already exists at '$Dest'. Move it aside to install this archive."
}

$Parent = Split-Path -Parent $Dest
New-Item -ItemType Directory -Force $Parent | Out-Null
$Stage = Join-Path $Parent "ffmpeg-extract-$([guid]::NewGuid())"
New-Item -ItemType Directory $Stage | Out-Null
try {
    Expand-Archive -LiteralPath $Zip -DestinationPath $Stage
    $Entries = @(Get-ChildItem -LiteralPath $Stage -Force)
    if ($Entries.Count -ne 1 -or -not $Entries[0].PSIsContainer) {
        throw "Expected one FFmpeg directory in '$Zip'."
    }
    $Inner = $Entries[0].FullName
    Assert-FfmpegLayout $Inner
    if ($Slim -and (Test-Path -LiteralPath $Dest)) {
        # Compare every extracted file; an archive stamp cannot authenticate installed DLLs.
        $ExpectedFiles = @(Get-ChildItem -LiteralPath $Inner -File -Recurse -Force)
        $InstalledFiles = @(Get-ChildItem -LiteralPath $Dest -File -Recurse -Force | Where-Object FullName -ne (Join-Path $Dest '.sha256'))
        if ($ExpectedFiles.Count -ne $InstalledFiles.Count) {
            throw "FFmpeg candidate files changed. Move '$Dest' aside and retry."
        }
        foreach ($File in $ExpectedFiles) {
            $Relative = $File.FullName.Substring($Inner.Length + 1)
            $Installed = Join-Path $Dest $Relative
            if (-not (Test-Path -LiteralPath $Installed -PathType Leaf) -or
                (Get-FileHash -LiteralPath $File.FullName -Algorithm SHA256).Hash -ne
                (Get-FileHash -LiteralPath $Installed -Algorithm SHA256).Hash) {
                throw "FFmpeg candidate file '$Relative' changed. Move '$Dest' aside and retry."
            }
        }
    } else {
        [IO.Directory]::Move($Inner, $Dest)
        if (-not $Slim) { Set-Content -LiteralPath $Stamp -Value $Sha256 -NoNewline }
    }
} finally {
    $Resolved = (Resolve-Path -LiteralPath $Stage).ProviderPath
    if ($Resolved -eq $Stage) { Remove-Item -LiteralPath $Resolved -Recurse -Force }
}

Write-Host "FFmpeg $Version ready in $Dest (sha256 $Actual)"
