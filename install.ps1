# Simple Rust installer and build script

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Step 1: Installing Rust" -ForegroundColor Yellow
Write-Host "========================================" -ForegroundColor Cyan
Write-Host ""

# Download installer
$rustupUrl = "https://win.rustup.rs/x86_64"
$rustupPath = "$env:TEMP\rustup-init.exe"

if (-not (Test-Path $rustupPath)) {
    Write-Host "Downloading Rust installer..." -ForegroundColor Yellow
    Invoke-WebRequest -Uri $rustupUrl -OutFile $rustupPath -UseBasicParsing
    Write-Host "Download complete" -ForegroundColor Green
} else {
    Write-Host "Installer already downloaded" -ForegroundColor Green
}

Write-Host ""
Write-Host "Installing Rust (this takes 5-10 minutes)..." -ForegroundColor Yellow
Write-Host "Please wait..." -ForegroundColor Gray
Write-Host ""

# Install Rust silently
$process = Start-Process -FilePath $rustupPath -ArgumentList "-y" -Wait -PassThru -NoNewWindow

if ($process.ExitCode -ne 0) {
    Write-Host "Installation failed. Please run manually:" -ForegroundColor Red
    Write-Host "  $rustupPath" -ForegroundColor White
    exit 1
}

Write-Host "Rust installed successfully!" -ForegroundColor Green
Write-Host ""

# Refresh environment
$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Step 2: Building AutoKey-Rust" -ForegroundColor Yellow
Write-Host "========================================" -ForegroundColor Cyan
Write-Host ""

Set-Location $PSScriptRoot

Write-Host "Project directory: $(Get-Location)" -ForegroundColor Gray
Write-Host "First build takes 8-12 minutes..." -ForegroundColor Yellow
Write-Host ""

# Build
cargo build --release

if ($LASTEXITCODE -eq 0) {
    Write-Host ""
    Write-Host "========================================" -ForegroundColor Green
    Write-Host "BUILD SUCCESSFUL!" -ForegroundColor Green
    Write-Host "========================================" -ForegroundColor Green
    Write-Host ""
    Write-Host "Executable: target\release\autokeyrust.exe" -ForegroundColor Cyan

    if (Test-Path "target\release\autokeyrust.exe") {
        $size = (Get-Item "target\release\autokeyrust.exe").Length
        Write-Host "Size: $([math]::Round($size/1MB, 2)) MB" -ForegroundColor Gray
        Write-Host ""
        Write-Host "Starting program..." -ForegroundColor Yellow
        Start-Process "target\release\autokeyrust.exe"
        Write-Host "Program started!" -ForegroundColor Green
    }
} else {
    Write-Host ""
    Write-Host "Build failed. Check errors above." -ForegroundColor Red
}

Write-Host ""
Write-Host "Press any key to exit..."
$null = $Host.UI.RawUI.ReadKey("NoEcho,IncludeKeyDown")
