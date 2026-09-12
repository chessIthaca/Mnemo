@echo off

cd /d "%~dp0"

echo Installing npm dependencies (root + frontend workspace)...
call npm install
if errorlevel 1 (
    echo npm install failed.
    pause
    exit /b 1
)

rem The Tauri CLI is hoisted to the root workspace node_modules and must run
rem with src-tauri as the working directory or tauri.conf.json is not detected.
pushd src-tauri
if /i "%~1"=="bundle" (
    echo Building frontend + Rust binary + installers...
    call "..\node_modules\.bin\tauri.cmd" build
) else (
    echo Building frontend + Rust binary without installer. Run "build.bat bundle" for installers.
    call "..\node_modules\.bin\tauri.cmd" build --no-bundle
)
set TAURI_EXIT=%ERRORLEVEL%
popd

if not "%TAURI_EXIT%"=="0" (
    echo Build failed.
    pause
    exit /b 1
)

echo.
echo Build complete: target\release\mnemo-app.exe
