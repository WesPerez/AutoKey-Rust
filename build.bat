@echo off
setlocal
cd /d "%~dp0"

where cargo >nul 2>&1
if errorlevel 1 (
    echo Rust/Cargo was not found. Install Rust from https://rustup.rs/
    exit /b 1
)

echo Running tests...
cargo test --locked --all-targets
if errorlevel 1 exit /b 1

echo Building release executable...
cargo build --locked --release
if errorlevel 1 exit /b 1

echo Running packaged startup smoke...
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%CD%\scripts\packaged-startup-smoke.ps1" -ExePath "%CD%\target\release\AutoKeyRust.exe"
if errorlevel 1 exit /b 1

if not exist "%CD%\dist" mkdir "%CD%\dist"
copy /y "%CD%\target\release\AutoKeyRust.exe" "%CD%\dist\AutoKeyRust.exe" >nul
if errorlevel 1 exit /b 1

certutil -hashfile "%CD%\dist\AutoKeyRust.exe" SHA256 > "%CD%\dist\AutoKeyRust.exe.sha256"
if errorlevel 1 exit /b 1

echo Done: %CD%\dist\AutoKeyRust.exe
