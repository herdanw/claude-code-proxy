@echo off
echo.
echo  ══════════════════════════════════════════
echo   ⚡ Claude Code Proxy — Build ^& Run
echo  ══════════════════════════════════════════
echo.

:: Check if Rust is installed
where cargo >nul 2>nul
if %ERRORLEVEL% neq 0 (
    echo  [ERROR] Rust is not installed.
    echo  Install it with:  winget install Rustlang.Rust.MSVC
    echo  Or download from: https://rustup.rs
    pause
    exit /b 1
)

:: Build in release mode
echo  Building release binary...
cargo build --release
if %ERRORLEVEL% neq 0 (
    echo  [ERROR] Build failed.
    pause
    exit /b 1
)

echo.
echo  Build complete: target\release\claude-proxy.exe
echo.

:: Check if target was provided as argument
if "%~1"=="" (
    set /p TARGET="  Enter your API target URL: "
) else (
    set TARGET=%~1
)

echo.
echo  Starting proxy...
echo.
target\release\claude-proxy.exe --target %TARGET%
