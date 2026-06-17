# AutoKey-Rust 快速安装脚本
# PowerShell版本

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "AutoKey-Rust 自动安装和编译脚本" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan
Write-Host ""

# 步骤1: 检查Rust
Write-Host "[1/5] 检查Rust安装..." -ForegroundColor Yellow
$cargoPath = Get-Command cargo -ErrorAction SilentlyContinue
if ($cargoPath) {
    Write-Host "[√] Rust已安装" -ForegroundColor Green
    cargo --version
    Write-Host ""
} else {
    Write-Host "[×] Rust未安装" -ForegroundColor Red
    Write-Host ""

    # 步骤2: 下载Rust
    Write-Host "[2/5] 下载Rust安装器..." -ForegroundColor Yellow
    $rustupUrl = "https://win.rustup.rs/x86_64"
    $rustupInstaller = "$env:TEMP\rustup-init.exe"

    try {
        [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
        Invoke-WebRequest -Uri $rustupUrl -OutFile $rustupInstaller -UseBasicParsing
        Write-Host "[√] 下载完成" -ForegroundColor Green
        Write-Host ""
    } catch {
        Write-Host "[错误] 下载失败: $_" -ForegroundColor Red
        Write-Host "请手动访问 https://rustup.rs/ 安装" -ForegroundColor Yellow
        pause
        exit 1
    }

    # 步骤3: 安装Rust
    Write-Host "[3/5] 安装Rust（自动选择默认配置）..." -ForegroundColor Yellow
    Write-Host "这可能需要5-10分钟..." -ForegroundColor Gray
    Write-Host ""

    $process = Start-Process -FilePath $rustupInstaller -ArgumentList "-y" -Wait -PassThru

    if ($process.ExitCode -ne 0) {
        Write-Host "[错误] 安装失败" -ForegroundColor Red
        pause
        exit 1
    }

    Write-Host "[√] Rust安装完成" -ForegroundColor Green
    Write-Host ""

    # 步骤4: 刷新环境变量
    Write-Host "[4/5] 配置环境变量..." -ForegroundColor Yellow
    $env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"

    # 验证
    $cargoPath = Get-Command cargo -ErrorAction SilentlyContinue
    if (-not $cargoPath) {
        Write-Host "[×] 环境变量配置失败" -ForegroundColor Red
        Write-Host "请关闭此窗口，重新打开PowerShell后再运行此脚本" -ForegroundColor Yellow
        pause
        exit 1
    }

    Write-Host "[√] 环境变量配置完成" -ForegroundColor Green
    cargo --version
    rustc --version
    Write-Host ""
}

# 步骤5: 编译项目
Write-Host "[5/5] 编译AutoKey-Rust项目..." -ForegroundColor Yellow
Write-Host ""

Set-Location $PSScriptRoot

if (-not (Test-Path "Cargo.toml")) {
    Write-Host "[错误] 未找到项目文件 Cargo.toml" -ForegroundColor Red
    Write-Host "当前目录: $(Get-Location)" -ForegroundColor Gray
    pause
    exit 1
}

Write-Host "[提示] 首次编译需要下载依赖，预计8-12分钟" -ForegroundColor Cyan
Write-Host "[提示] 后续编译只需1-2分钟" -ForegroundColor Cyan
Write-Host ""
Write-Host "开始编译..." -ForegroundColor Yellow
Write-Host "----------------------------------------" -ForegroundColor Gray

$buildProcess = Start-Process -FilePath "cargo" -ArgumentList "build", "--release" -Wait -NoNewWindow -PassThru

Write-Host "----------------------------------------" -ForegroundColor Gray
Write-Host ""

if ($buildProcess.ExitCode -ne 0) {
    Write-Host "========================================" -ForegroundColor Red
    Write-Host "[错误] 编译失败" -ForegroundColor Red
    Write-Host "========================================" -ForegroundColor Red
    Write-Host ""
    Write-Host "请检查上方的错误信息" -ForegroundColor Yellow
    pause
    exit 1
}

Write-Host "========================================" -ForegroundColor Green
Write-Host "[成功] 编译完成！" -ForegroundColor Green
Write-Host "========================================" -ForegroundColor Green
Write-Host ""

$exePath = "target\release\autokeyrust.exe"
if (Test-Path $exePath) {
    Write-Host "产物位置: $exePath" -ForegroundColor Cyan
    $fileInfo = Get-Item $exePath
    Write-Host "文件大小: $([math]::Round($fileInfo.Length / 1MB, 2)) MB" -ForegroundColor Cyan
    Write-Host "修改时间: $($fileInfo.LastWriteTime)" -ForegroundColor Gray
    Write-Host ""
    Write-Host "[√] 可执行文件已生成" -ForegroundColor Green
    Write-Host ""

    Write-Host "按任意键运行程序..." -ForegroundColor Yellow
    $null = $Host.UI.RawUI.ReadKey("NoEcho,IncludeKeyDown")

    Write-Host ""
    Write-Host "启动 AutoKey-Rust..." -ForegroundColor Cyan
    Start-Process -FilePath $exePath

    Write-Host ""
    Write-Host "[提示] 程序已启动" -ForegroundColor Green
    Write-Host "[提示] 如需再次运行: .\$exePath" -ForegroundColor Gray
} else {
    Write-Host "[错误] 未找到编译产物" -ForegroundColor Red
}

Write-Host ""
pause
