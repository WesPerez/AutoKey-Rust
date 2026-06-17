@echo off
cls
echo ==========================================
echo AutoKey-Rust Build Script
echo ==========================================
echo.

REM Step 1: Check Rust
echo [Step 1/5] Checking Rust installation...
where cargo >nul 2>&1
if %errorlevel% equ 0 (
    echo [OK] Rust is installed
    cargo --version
    echo.
    goto BUILD
)

echo [INFO] Rust not found, installing...
echo.

REM Step 2: Download Rust installer
echo [Step 2/5] Downloading Rust installer...
set RUSTUP_URL=https://win.rustup.rs/x86_64
set RUSTUP_INSTALLER=%TEMP%\rustup-init.exe

echo Downloading from: %RUSTUP_URL%
echo.

powershell -NoProfile -ExecutionPolicy Bypass -Command "Invoke-WebRequest -Uri '%RUSTUP_URL%' -OutFile '%RUSTUP_INSTALLER%' -UseBasicParsing"

if %errorlevel% neq 0 (
    echo [ERROR] Download failed
    echo.
    echo Please manually visit: https://rustup.rs/
    echo.
    goto END
)

echo [OK] Download complete
echo.

REM Step 3: Install Rust
echo [Step 3/5] Installing Rust...
echo.
echo The installer will open in a new window
echo Press ENTER in that window to use default settings
echo Please wait, do NOT close this window
echo.
echo Press any key to start installation...
pause >nul

echo.
echo Installing, please wait...
start /wait "" "%RUSTUP_INSTALLER%" -y

if %errorlevel% neq 0 (
    echo [ERROR] Installation failed
    goto END
)

echo.
echo [OK] Rust installed successfully
echo.

REM Step 4: Update PATH
echo [Step 4/5] Updating environment...
set PATH=%USERPROFILE%\.cargo\bin;%PATH%

timeout /t 2 /nobreak >nul

where cargo >nul 2>&1
if %errorlevel% neq 0 (
    echo.
    echo [WARN] Environment not refreshed yet
    echo.
    echo Please:
    echo 1. Close this window
    echo 2. Open a new Command Prompt
    echo 3. Run this script again
    echo.
    goto END
)

echo [OK] Environment configured
cargo --version
rustc --version
echo.

:BUILD
REM Step 5: Build project
echo [Step 5/5] Building AutoKey-Rust...
echo.

cd /d "%~dp0"

if not exist "Cargo.toml" (
    echo [ERROR] Cargo.toml not found
    echo Expected: %CD%\Cargo.toml
    echo Current dir: %CD%
    echo.
    goto END
)

echo Project directory: %CD%
echo First build takes 8-12 minutes
echo Downloading dependencies and compiling...
echo.
echo ==========================================

cargo build --release

echo ==========================================
echo.

if %errorlevel% neq 0 (
    echo [ERROR] Build failed
    echo.
    echo Common issues:
    echo 1. Missing MSVC tools - Install Visual Studio Build Tools
    echo 2. Code errors - Please report the error message
    echo.
    goto END
)

echo [SUCCESS] Build complete!
echo.
echo Output: target\release\autokeyrust.exe
echo.

if exist "target\release\autokeyrust.exe" (
    for %%F in ("target\release\autokeyrust.exe") do (
        echo File size: %%~zF bytes
    )
    echo.

    echo Press any key to run the program...
    pause >nul

    echo.
    echo Starting AutoKey-Rust...
    start "" "target\release\autokeyrust.exe"

    echo.
    echo [INFO] Program started
    echo [INFO] To run again: target\release\autokeyrust.exe
) else (
    echo [ERROR] Executable not found
)

:END
echo.
echo ==========================================
echo Script finished
echo ==========================================
echo.
echo Press any key to exit...
pause >nul
