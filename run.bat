@echo off
setlocal

set "CARGO=cargo"
where cargo >nul 2>&1
if not errorlevel 1 goto launch

if exist "%USERPROFILE%\.cargo\bin\cargo.exe" (
    set "CARGO=%USERPROFILE%\.cargo\bin\cargo.exe"
    goto launch
)

echo Rust is required. Install it from https://rustup.rs and run this script again. >&2
exit /b 1

:launch
if "%~1"=="" (
    "%CARGO%" run --manifest-path "%~dp0Cargo.toml" --release --locked -- setup
) else (
    "%CARGO%" run --manifest-path "%~dp0Cargo.toml" --release --locked -- %*
)
set "EXIT_CODE=%ERRORLEVEL%"
exit /b %EXIT_CODE%
