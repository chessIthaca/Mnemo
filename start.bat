@echo off

cd /d "%~dp0"

rem The Tauri CLI is installed in the root workspace node_modules, not frontend.
rem Sync node_modules with package.json BEFORE any branch that invokes the CLI.
rem Without this, a dependency added to package.json after the last install is
rem missing at build time and the frontend `tsc` step dies with
rem "Cannot find module ...". npm install is a fast no-op when already in sync.
echo Syncing npm dependencies...
call npm install
if errorlevel 1 (
    echo npm install failed. Fix the errors above and run start.bat again.
    pause
    goto :end
)

rem `start.bat dev` must fail with a friendly message (not a raw
rem "not recognized") if the CLI still is not there after the install.
if not exist "node_modules\.bin\tauri.cmd" (
    echo Tauri CLI not found after npm install. Check package.json.
    pause
    goto :end
)

rem Tauri shells out to cargo; without it the build dies with a cryptic
rem "cargo metadata ... program not found". rustup installs to
rem %USERPROFILE%\.cargo\bin, which a console opened before the install does
rem not have on PATH yet - pick it up so a fresh install works without relogin.
where cargo >nul 2>&1
if errorlevel 1 if exist "%USERPROFILE%\.cargo\bin\cargo.exe" set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
where cargo >nul 2>&1
if errorlevel 1 (
    echo Rust toolchain not found - cargo is not on PATH.
    echo Install it from https://rustup.rs ^(stable, with the MSVC C++ Build Tools^),
    echo then run start.bat again.
    pause
    goto :end
)

if /i "%~1"=="dev" (
    pushd src-tauri
    call "..\node_modules\.bin\tauri.cmd" dev
    popd
    goto :end
)

rem A running instance locks the app exe (the release build) and makes the
rem build fail with "Access is denied". Close it gracefully (WM_CLOSE; the
rem app's exit hook shuts down cleanly) before rebuilding - the script
rem relaunches the app at the end anyway.
taskkill /IM mnemo-app.exe >nul 2>&1

echo Building frontend + Rust binary, no installer...
pushd src-tauri
call "..\node_modules\.bin\tauri.cmd" build --no-bundle
set TAURI_EXIT=%ERRORLEVEL%
popd
if not "%TAURI_EXIT%"=="0" (
    echo Build failed.
    pause
    goto :end
)

echo Starting Mnemo...
target\release\mnemo-app.exe

:end
