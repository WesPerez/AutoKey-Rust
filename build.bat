@echo off
setlocal
cd /d "%~dp0"

where cargo >nul 2>&1
if errorlevel 1 (
    echo Rust/Cargo was not found. Install Rust from https://rustup.rs/
    exit /b 1
)

echo Running tests...
cargo test --all-targets
if errorlevel 1 exit /b 1

echo Building release executable...
cargo build --release
if errorlevel 1 exit /b 1

echo Done: %CD%\target\release\AutoKeyRust.exe
