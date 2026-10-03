@echo off
setlocal

pushd "%~dp0" >nul
if errorlevel 1 (
    echo Could not open the OpenRaid directory. >&2
    exit /b 1
)

set "CARGO=cargo"
where cargo >nul 2>&1
if not errorlevel 1 goto build

if exist "%USERPROFILE%\.cargo\bin\cargo.exe" (
    set "CARGO=%USERPROFILE%\.cargo\bin\cargo.exe"
    goto build
)

echo Rust is required. Install it from https://rustup.rs and run this script again. >&2
popd
exit /b 1

:build
call "%CARGO%" build --release --locked
set "EXIT_CODE=%ERRORLEVEL%"
if not "%EXIT_CODE%"=="0" goto done
set "OUTPUT_DIR=%CD%\target"
if defined CARGO_TARGET_DIR (
    for %%I in ("%CARGO_TARGET_DIR%") do set "OUTPUT_DIR=%%~fI"
)
echo.
echo Built OpenRaid: %OUTPUT_DIR%\release\openraid.exe

:done
popd
exit /b %EXIT_CODE%
