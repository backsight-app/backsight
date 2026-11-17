# Offline regression checks using disposable archives, destinations and download caches.
$ErrorActionPreference = 'Stop'
$Root = Split-Path -Parent $PSScriptRoot
$Sandbox = Join-Path $Root "target/ffmpeg-fetch-check-$([guid]::NewGuid())"
$PreviousTemp = $env:TEMP
$OriginalDownload = Get-Item Function:Invoke-WebRequest -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force "$Sandbox/scripts", "$Sandbox/cache", "$Sandbox/package/ffmpeg/bin", "$Sandbox/package/ffmpeg/lib", "$Sandbox/package/ffmpeg/include/libavcodec" | Out-Null
try {
    $env:TEMP = "$Sandbox/cache"
    Set-Content "$Sandbox/package/ffmpeg/include/libavcodec/avcodec.h" 'header'
    foreach ($Library in @('avcodec-62', 'avdevice-62', 'avfilter-11', 'avformat-62', 'avutil-60', 'swresample-6', 'swscale-9')) {
        Set-Content "$Sandbox/package/ffmpeg/bin/$Library.dll" $Library
        Set-Content "$Sandbox/package/ffmpeg/lib/$($Library.Split('-')[0]).lib" $Library
    }
    Set-Content "$Sandbox/package/ffmpeg/bin/ffmpeg.exe" 'ffmpeg'
    Set-Content "$Sandbox/package/ffmpeg/bin/ffprobe.exe" 'ffprobe'
    $FixtureZip = "$Sandbox/fixture.zip"
    Compress-Archive -LiteralPath "$Sandbox/package/ffmpeg" -DestinationPath $FixtureZip
    $Hash = (Get-FileHash $FixtureZip).Hash
    $Fetcher = "$Sandbox/scripts/fetch-ffmpeg.ps1"
    $Source = Get-Content "$PSScriptRoot/fetch-ffmpeg.ps1" -Raw
    # Pins the fixture in place of the default download's (uncommented) SHA-256.
    $Pinned = $Source -replace '(?m)^(\s*\$Sha256 = ")[0-9a-fA-F]{64}(")', "`${1}$Hash`${2}"
    if ($Pinned -eq $Source) { throw 'No pinned SHA-256 found in fetch-ffmpeg.ps1.' }
    $Pinned | Set-Content $Fetcher
    function Expect-Failure([scriptblock] $Run, [string] $Message) {
        $Failed = $false
        try { & $Run } catch { $Failed = $true }
        if (-not $Failed) { throw $Message }
    }
    & $Fetcher -SlimArchive $FixtureZip -SlimSha256 $Hash
    & $Fetcher -SlimArchive $FixtureZip -SlimSha256 $Hash
    $Dll = "$Sandbox/target/ffmpeg-slim/bin/avcodec-62.dll"
    Set-Content $Dll 'tampered'
    Expect-Failure { & $Fetcher -SlimArchive $FixtureZip -SlimSha256 $Hash } 'Altered DLL accepted.'
    Copy-Item "$Sandbox/package/ffmpeg/bin/avcodec-62.dll" $Dll -Force
    Remove-Item -LiteralPath $Dll
    Expect-Failure { & $Fetcher -SlimArchive $FixtureZip -SlimSha256 $Hash } 'Missing DLL accepted.'
    Copy-Item "$Sandbox/package/ffmpeg/bin/avcodec-62.dll" $Dll
    Set-Content "$Sandbox/target/ffmpeg-slim/bin/unexpected.dll" 'extra'
    Expect-Failure { & $Fetcher -SlimArchive $FixtureZip -SlimSha256 $Hash } 'Extra DLL accepted.'
    Expect-Failure { & $Fetcher -SlimArchive $FixtureZip -SlimSha256 ('0' * 64) } 'Wrong archive checksum accepted.'

    $Cache = "$env:TEMP/ffmpeg-8.1.3-backsight-slim-win64-gpl-shared.zip"
    Set-Content $Cache 'corrupt cache'
    function Invoke-WebRequest { param($Uri, $OutFile, [switch] $UseBasicParsing) Set-Content $OutFile 'corrupt download' }
    Expect-Failure { & $Fetcher } 'Invalid download accepted.'
    if ((Get-Content $Cache -Raw).Trim() -ne 'corrupt cache') { throw 'Bad download replaced cache.' }
    if (@(Get-ChildItem "$env:TEMP/*.part").Count) { throw 'Download partial leaked.' }
    function Invoke-WebRequest { param($Uri, $OutFile, [switch] $UseBasicParsing) Copy-Item $FixtureZip $OutFile }
    & $Fetcher
    if ((Get-FileHash $Cache).Hash -ne $Hash) { throw 'Corrupt cache was not replaced.' }
    if (-not (Test-Path "$Sandbox/third_party/ffmpeg/bin/avcodec-62.dll")) { throw 'Full package was not installed.' }
    & $Fetcher
    Remove-Item -LiteralPath "$Sandbox/third_party/ffmpeg/.sha256"
    Expect-Failure { & $Fetcher } 'An unstamped (older) FFmpeg was accepted.'
    Write-Host 'FFmpeg fetch checks passed.'
} finally {
    $env:TEMP = $PreviousTemp
    if ($OriginalDownload) { Set-Item Function:Invoke-WebRequest $OriginalDownload.ScriptBlock }
    else { Remove-Item Function:Invoke-WebRequest -ErrorAction SilentlyContinue }
    $Resolved = (Resolve-Path -LiteralPath $Sandbox).ProviderPath
    if ($Resolved -eq $Sandbox -and $Resolved.StartsWith((Join-Path $Root 'target') + [IO.Path]::DirectorySeparatorChar)) {
        Remove-Item -LiteralPath $Resolved -Recurse -Force
    }
}
