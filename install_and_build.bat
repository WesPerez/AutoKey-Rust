@echo off
chcp 65001 >nul
setlocal enabledelayedexpansion

REM 防止窗口自动关闭
if "%1" neq "ELEVATED" (
    echo 请稍候，正在启动...
    cmd /c "%~f0" ELEVATED
    pause
    exit /b
)

cls
echo ========================================
echo AutoKey-Rust 自动安装和编译脚本
echo ========================================
echo.
echo [日志] 脚本启动成功
echo [日志] 当前目录: %CD%
echo.

REM 检查管理员权限
net session >nul 2>&1
if %errorlevel% neq 0 (
    echo [提示] 建议以管理员权限运行，但非必需
    echo.
)

REM 步骤1: 检查Rust安装
echo [1/5] 检查Rust安装...
where cargo >nul 2>&1
if %errorlevel% equ 0 (
    echo [√] Rust已安装
    cargo --version
    goto :BUILD
)

echo [×] Rust未安装
echo.

REM 步骤2: 下载Rust安装器
echo [2/5] 下载Rust安装器...
set RUSTUP_URL=https://win.rustup.rs/x86_64
set RUSTUP_INSTALLER=%TEMP%\rustup-init.exe

echo 下载地址: %RUSTUP_URL%
echo 保存位置: %RUSTUP_INSTALLER%
echo.

powershell -Command "& {[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12; Invoke-WebRequest -Uri '%RUSTUP_URL%' -OutFile '%RUSTUP_INSTALLER%' -UseBasicParsing}"

if %errorlevel% neq 0 (
    echo [错误] 下载失败，请手动访问 https://rustup.rs/ 安装
    pause
    exit /b 1
)

echo [√] 下载完成
echo.

REM 步骤3: 安装Rust
echo [3/5] 安装Rust（这可能需要几分钟）...
echo.
echo 安装选项说明:
echo   1) Proceed with installation (default) - 推荐
echo   2) Customize installation
echo   3) Cancel installation
echo.
echo [提示] 直接按回车选择默认安装
echo.

"%RUSTUP_INSTALLER%" -y

if %errorlevel% neq 0 (
    echo [错误] 安装失败
    pause
    exit /b 1
)

echo.
echo [√] Rust安装完成
echo.

REM 步骤4: 刷新环境变量
echo [4/5] 配置环境变量...
set PATH=%USERPROFILE%\.cargo\bin;%PATH%

REM 验证安装
cargo --version >nul 2>&1
if %errorlevel% neq 0 (
    echo [×] Rust未正确安装
    echo.
    echo 请关闭此窗口，重新打开终端后再运行此脚本
    pause
    exit /b 1
)

echo [√] 环境变量配置完成
cargo --version
rustc --version
echo.

:BUILD
REM 步骤5: 编译项目
echo [5/5] 编译AutoKey-Rust项目...
echo.

cd /d E:\Project\Common\AutoKey-Rust

if not exist "Cargo.toml" (
    echo [错误] 未找到项目文件 Cargo.toml
    echo 当前目录: %CD%
    pause
    exit /b 1
)

echo [提示] 首次编译需要下载依赖，预计8-12分钟
echo [提示] 后续编译只需1-2分钟
echo.
echo 开始编译...
echo ----------------------------------------

cargo build --release

if %errorlevel% neq 0 (
    echo.
    echo ========================================
    echo [错误] 编译失败
    echo ========================================
    echo.
    echo 请检查上方的错误信息
    echo 如有代码错误，请联系开发者修复
    echo.
    pause
    exit /b 1
)

echo ----------------------------------------
echo.
echo ========================================
echo [成功] 编译完成！
echo ========================================
echo.
echo 产物位置: target\release\autokey.exe
echo.

if exist "target\release\autokey.exe" (
    echo 文件信息:
    dir target\release\autokey.exe | find "autokey.exe"
    echo.
    echo [√] 可执行文件已生成
    echo.

    echo 按任意键运行程序...
    pause >nul

    echo.
    echo 启动 AutoKey-Rust...
    start "" "target\release\autokey.exe"

    echo.
    echo [提示] 程序已在后台启动
    echo [提示] 如需再次运行: target\release\autokey.exe
) else (
    echo [错误] 未找到编译产物
)

echo.
pause
