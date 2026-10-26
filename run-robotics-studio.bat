@echo off
setlocal
title Robotics Studio - WIFA Robotics
cd /d "%~dp0"
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"

if exist "solidcraft.exe" (
    start "" "%~dp0solidcraft.exe" %*
    exit /b 0
)
if exist "RoboticsStudio.exe" (
    start "" "%~dp0RoboticsStudio.exe" %*
    exit /b 0
)
if exist "target\release\solidcraft.exe" (
    start "" "%~dp0target\release\solidcraft.exe" %*
    exit /b 0
)
if exist "target\x86_64-pc-windows-msvc\release\solidcraft.exe" (
    start "" "%~dp0target\x86_64-pc-windows-msvc\release\solidcraft.exe" %*
    exit /b 0
)

where cargo >nul 2>&1
if errorlevel 1 (
    echo [Error] No desktop executable or Rust installation found. 1>&2
    echo Extract the Windows portable ZIP, or install Rust and Visual Studio C++ Build Tools. 1>&2
    exit /b 1
)
echo Building Robotics Studio and its MCP command-line tool... 1>&2
cargo build --release --locked -p solidcraft -p solidcraft-cli
if errorlevel 1 exit /b 1
start "" "%~dp0target\release\solidcraft.exe" %*
