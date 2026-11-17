@echo off
setlocal
set "APP=%~dp0dist-desktop\Backsight\backsight.exe"
if not exist "%APP%" (
  echo Backsight has not been built yet.
  echo Run scripts\build-desktop.ps1 from PowerShell first.
  pause
  exit /b 1
)
start "" /D "%~dp0dist-desktop\Backsight" "%APP%"
