@echo off
echo ========================================
echo 快速编译脚本（假设Rust已安装）
echo ========================================
echo.

cd /d "%~dp0"

if not exist "Cargo.toml" (
    echo [错误] 未找到 Cargo.toml
    echo 当前目录: %CD%
    pause
    exit /b 1
)

echo 当前目录: %CD%
echo.
echo 开始编译...
echo.

cargo build --release

if %errorlevel% neq 0 (
    echo.
    echo [错误] 编译失败，请检查上方错误信息
    echo.
    pause
    exit /b 1
)

echo.
echo ========================================
echo 编译成功！
echo ========================================
echo.
echo 产物: target\release\autokeyrust.exe
echo.

if exist "target\release\autokeyrust.exe" (
    echo 按任意键运行...
    pause >nul
    start "" "target\release\autokeyrust.exe"
) else (
    echo [错误] 未找到编译产物
)

pause
