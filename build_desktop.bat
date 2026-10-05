@echo off
setlocal

pushd "%~dp0" >nul
if errorlevel 1 (
    echo Could not open the OpenRaid directory. >&2
    exit /b 1
)

where cargo >nul 2>&1
if not errorlevel 1 goto node_check
if exist "%USERPROFILE%\.cargo\bin\cargo.exe" (
    set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
    goto node_check
)
echo Rust is required. Install it from https://rustup.rs and run this script again. >&2
set "EXIT_CODE=1"
goto done

:node_check
where node >nul 2>&1
if errorlevel 1 goto missing_node
where npm >nul 2>&1
if errorlevel 1 goto missing_node
node -e "const [major, minor] = process.versions.node.split('.').map(Number); if (major < 22 || (major === 22 && minor < 12)) { console.error('The optional desktop build requires Node.js 22.12+.'); process.exit(1); }"
set "EXIT_CODE=%ERRORLEVEL%"
if not "%EXIT_CODE%"=="0" goto done
node -e "const result = require('node:child_process').spawnSync('rustc', ['--version'], {encoding:'utf8'}); const version = /rustc (\d+)\.(\d+)/.exec(result.stdout || ''); if (result.status !== 0 || !version || Number(version[1]) < 1 || (Number(version[1]) === 1 && Number(version[2]) < 90)) { console.error('The optional desktop build requires Rust 1.90+. Update your active toolchain with rustup update stable.'); process.exit(1); }"
set "EXIT_CODE=%ERRORLEVEL%"
if not "%EXIT_CODE%"=="0" goto done

set "OUTPUT_DIR=%CD%\desktop\src-tauri\target"
if defined CARGO_TARGET_DIR (
    for %%I in ("%CARGO_TARGET_DIR%") do set "OUTPUT_DIR=%%~fI"
)
if defined CARGO_TARGET_DIR set "CARGO_TARGET_DIR=%OUTPUT_DIR%"
call npm ci --prefix desktop
set "EXIT_CODE=%ERRORLEVEL%"
if not "%EXIT_CODE%"=="0" goto done
call npm run tauri --prefix desktop -- build --no-bundle -- --locked
set "EXIT_CODE=%ERRORLEVEL%"
if not "%EXIT_CODE%"=="0" goto done
echo.
echo Built OpenRaid desktop: %OUTPUT_DIR%\release\openraid-desktop.exe
goto done

:missing_node
echo The optional desktop build requires Node.js 22.12+ and npm. >&2
set "EXIT_CODE=1"

:done
popd
exit /b %EXIT_CODE%
