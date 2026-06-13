@echo off
echo ==========================================
echo Install Visual Studio Build Tools
echo ==========================================
echo.
echo IMPORTANT: You need Visual Studio Build Tools to compile Rust on Windows
echo.
echo This script will download the installer (~1-2 MB)
echo The actual installation will download ~6 GB and take 10-30 minutes
echo.
echo Press any key to start download...
pause >nul

echo.
echo Downloading installer...
powershell -Command "Invoke-WebRequest -Uri 'https://aka.ms/vs/17/release/vs_BuildTools.exe' -OutFile '%TEMP%\vs_buildtools.exe'"

if %errorlevel% neq 0 (
    echo [ERROR] Download failed
    echo.
    echo Please manually download from:
    echo https://visualstudio.microsoft.com/downloads/
    echo Look for "Build Tools for Visual Studio 2022"
    pause
    exit /b 1
)

echo [OK] Download complete
echo.
echo Starting installer...
echo.
echo IMPORTANT STEPS:
echo 1. In the installer, CHECK the "Desktop development with C++" workload
echo 2. Wait for installation to complete (10-30 minutes)
echo 3. After installation, close this window and run the build script again
echo.
echo Press any key to launch installer...
pause >nul

start "" "%TEMP%\vs_buildtools.exe"

echo.
echo ==========================================
echo Installer launched
echo ==========================================
echo.
echo Next steps:
echo 1. In the Visual Studio Installer window:
echo    - Check "Desktop development with C++"
echo    - Click Install button
echo 2. Wait for installation to complete
echo 3. Restart your computer (recommended)
echo 4. Run: cargo build --release
echo.
pause
