@echo off
rem Builds Mouse without borders. The installer and the portable EXE are
rem created in the "Release" folder next to this file.
setlocal
cd /d "%~dp0"

where corepack >nul 2>nul
if errorlevel 1 (
  echo Node.js is not installed. Install it from https://nodejs.org and run this file again.
  pause
  exit /b 1
)

rem pnpm comes with Node.js through Corepack. If it is not switched on for the
rem whole PC, switch it on in a folder for this user only (no admin needed).
set "COREPACK_ENABLE_DOWNLOAD_PROMPT=0"
where pnpm >nul 2>nul
if errorlevel 1 (
  if not exist "%LOCALAPPDATA%\VSA-pnpm" mkdir "%LOCALAPPDATA%\VSA-pnpm"
  call corepack enable --install-directory "%LOCALAPPDATA%\VSA-pnpm" pnpm
  if errorlevel 1 goto :failed
  set "PATH=%LOCALAPPDATA%\VSA-pnpm;%PATH%"
)

where cargo >nul 2>nul
if errorlevel 1 (
  echo Rust is not installed. Install it from https://rustup.rs ^(choose the MSVC toolchain^).
  pause
  exit /b 1
)

call pnpm install
if errorlevel 1 goto :failed

call pnpm tauri build
if errorlevel 1 goto :failed

if not exist "Release" mkdir "Release"
copy /y "target\release\MouseWithoutBorders.exe" "Release\MouseWithoutBorders.exe" >nul
if errorlevel 1 goto :failed
copy /y "target\release\bundle\nsis\*-setup.exe" "Release\" >nul
if errorlevel 1 goto :failed

echo.
echo Done. Files are in: %~dp0Release
dir /b "Release"
pause
exit /b 0

:failed
echo.
echo The build failed. Please read the messages above.
pause
exit /b 1
