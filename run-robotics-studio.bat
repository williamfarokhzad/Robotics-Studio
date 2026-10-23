@echo off
title Robotics Studio - WIFA Robotics
echo ========================================================
echo           Launching Robotics Studio v0.0.1
echo                 by WIFA Robotics
echo ========================================================
echo.

set "CARGO_PATH=%USERPROFILE%\.cargo\bin"
set "PATH=%CARGO_PATH%;%PATH%"

cd /d "%~dp0"

if exist "target\x86_64-pc-windows-msvc\release\solidcraft.exe" (
    echo Starting Robotics Studio...
    start "" "target\x86_64-pc-windows-msvc\release\solidcraft.exe" --sample
    exit /b 0
)

if exist "target\release\solidcraft.exe" (
    echo Starting Robotics Studio...
    start "" "target\release\solidcraft.exe" --sample
    exit /b 0
)

echo Building Robotics Studio binary (first time setup)...
cargo xwin run --release --target x86_64-pc-windows-msvc -p solidcraft -- --sample
if errorlevel 1 (
    echo.
    echo [Error] Failed to launch Robotics Studio.
    pause
)
