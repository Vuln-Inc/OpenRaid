@echo off
setlocal

pushd "%~dp0" >nul
if errorlevel 1 (
    echo Could not open the OpenRaid directory. >&2
    exit /b 1
)

set "CARGO=cargo"
where cargo >nul 2>&1
if not errorlevel 1 goto launch

if exist "%USERPROFILE%\.cargo\bin\cargo.exe" (
    set "CARGO=%USERPROFILE%\.cargo\bin\cargo.exe"
    goto launch
)

echo Rust is required. Install it from https://rustup.rs and run this script again. >&2
popd
exit /b 1

:launch
if "%~1"=="" (
    "%CARGO%" run --release --locked -- setup
) else (
    "%CARGO%" run --release --locked -- %*
)
set "EXIT_CODE=%ERRORLEVEL%"
popd
exit /b %EXIT_CODE%
