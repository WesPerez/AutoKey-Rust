param(
    [string]$ExePath = "",
    [int]$StartupWaitSeconds = 18
)

$ErrorActionPreference = "Stop"
$projectRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
if (-not $ExePath) {
    $ExePath = Join-Path $projectRoot "target\release\AutoKeyRust.exe"
}
$ExePath = (Resolve-Path $ExePath).Path

Add-Type @"
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;

public static class AutoKeyWindowProbe {
    public delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumWindowsProc callback, IntPtr lParam);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint pid);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr hWnd);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern int GetWindowText(IntPtr hWnd, StringBuilder text, int count);

    public static string[] VisibleWindowsForProcess(int targetPid) {
        var result = new List<string>();
        EnumWindows(delegate(IntPtr hwnd, IntPtr ignored) {
            uint pid;
            GetWindowThreadProcessId(hwnd, out pid);
            if (pid == targetPid && IsWindowVisible(hwnd)) {
                var title = new StringBuilder(512);
                GetWindowText(hwnd, title, title.Capacity);
                result.Add(hwnd.ToInt64() + "|" + title.ToString());
            }
            return true;
        }, IntPtr.Zero);
        return result.ToArray();
    }
}
"@

function Wait-Until([string]$Label, [scriptblock]$Condition, [int]$Seconds) {
    $deadline = [DateTime]::UtcNow.AddSeconds($Seconds)
    while ([DateTime]::UtcNow -lt $deadline) {
        if (& $Condition) { return }
        Start-Sleep -Milliseconds 100
    }
    throw "Timed out waiting for $Label"
}

function Find-StagedProcess([string]$Path, [DateTime]$StartedAfter) {
    $escaped = $Path.Replace("\", "\\")
    Get-CimInstance Win32_Process -Filter "Name='AutoKeyRust.exe'" |
        Where-Object {
            $created = if ($_.CreationDate -is [DateTime]) {
                $_.CreationDate
            } else {
                [Management.ManagementDateTimeConverter]::ToDateTime($_.CreationDate)
            }
            $_.ExecutablePath -eq $Path -and
            $created -ge $StartedAfter
        } |
        Select-Object -First 1
}

$smokeId = ([guid]::NewGuid().ToString("N")).Substring(0, 10)
$smokeRoot = Join-Path ([IO.Path]::GetTempPath()) "autokey-rust-packaged-smoke-$smokeId"
$appRoot = Join-Path $smokeRoot "app"
$appData = Join-Path $smokeRoot "appdata"
$startupDir = Join-Path $appData "Microsoft\Windows\Start Menu\Programs\Startup"
$stagedExe = Join-Path $appRoot "AutoKeyRust.exe"
$linkPath = Join-Path $startupDir "AutoKey-Rust.lnk"
$firstPid = $null
$secondPid = $null
$originalAppDataRoot = $env:AUTOKEY_APPDATA_ROOT
$originalInstanceId = $env:AUTOKEY_INSTANCE_ID

New-Item -ItemType Directory -Path $appRoot,$startupDir | Out-Null
Copy-Item -LiteralPath $ExePath -Destination $stagedExe

try {
    $shell = New-Object -ComObject WScript.Shell
    $shortcut = $shell.CreateShortcut($linkPath)
    $shortcut.TargetPath = $stagedExe
    $shortcut.Arguments = "--autostart"
    $shortcut.WorkingDirectory = $appRoot
    $shortcut.Save()

    $readBack = $shell.CreateShortcut($linkPath)
    if ($readBack.TargetPath -ne $stagedExe) { throw "shortcut target mismatch" }
    if ($readBack.Arguments -ne "--autostart") { throw "shortcut arguments mismatch" }
    if ($readBack.WorkingDirectory -ne $appRoot) { throw "shortcut working directory mismatch" }

    $env:AUTOKEY_APPDATA_ROOT = $appData
    $env:AUTOKEY_INSTANCE_ID = $smokeId
    $launchStarted = [DateTime]::Now.AddSeconds(-1)
    Start-Process -FilePath $linkPath | Out-Null

    $firstProcess = $null
    Wait-Until "shortcut-launched process" {
        $script:firstProcess = Find-StagedProcess $stagedExe $launchStarted
        $null -ne $script:firstProcess
    } $StartupWaitSeconds
    $firstPid = [int]$firstProcess.ProcessId
    if ($firstProcess.CommandLine -notmatch '--autostart') {
        throw "shortcut-launched process is missing --autostart: $($firstProcess.CommandLine)"
    }

    $appLog = Join-Path $appData "AutoKey-Rust\logs\app.log"
    Wait-Until "startup and renderer log" {
        if (-not (Test-Path -LiteralPath $appLog)) { return $false }
        $content = Get-Content -LiteralPath $appLog -Raw
        $content.Contains("autostart=true") -and $content.Contains("active=glow")
    } $StartupWaitSeconds
    Wait-Until "autostart viewport hiding" {
        $content = Get-Content -LiteralPath $appLog -Raw
        $content.Contains("main viewport hidden after first render")
    } $StartupWaitSeconds

    Wait-Until "autostart main window hidden" {
        @([AutoKeyWindowProbe]::VisibleWindowsForProcess($firstPid) | Where-Object {
            ($_.Split('|', 2)[1]).Length -gt 0
        }).Count -eq 0
    } $StartupWaitSeconds

    $second = Start-Process -FilePath $stagedExe -PassThru
    $secondPid = $second.Id
    if (-not $second.WaitForExit($StartupWaitSeconds * 1000)) {
        throw "second instance did not exit"
    }
    if ($second.ExitCode -ne 0) { throw "second instance exit code was $($second.ExitCode)" }

    Wait-Until "first instance wake" {
        @([AutoKeyWindowProbe]::VisibleWindowsForProcess($firstPid) | Where-Object {
            ($_.Split('|', 2)[1]).Length -gt 0
        }).Count -gt 0
    } $StartupWaitSeconds

    $logContent = Get-Content -LiteralPath $appLog -Raw
    if (-not $logContent.Contains("activation received; showing main viewport")) {
        throw "activation log was not written"
    }

    [pscustomobject]@{
        PackagedStartupSmoke = $true
        ShortcutPath = $linkPath
        ShortcutTarget = $readBack.TargetPath
        ShortcutArguments = $readBack.Arguments
        ShortcutWorkingDirectory = $readBack.WorkingDirectory
        FirstProcessId = $firstPid
        FirstCommandLine = $firstProcess.CommandLine
        AutostartMainWindowHidden = $true
        SecondInstanceExitCode = $second.ExitCode
        FirstInstanceWakeVisible = $true
        AppLog = $appLog
    } | Format-List
}
finally {
    $env:AUTOKEY_APPDATA_ROOT = $originalAppDataRoot
    $env:AUTOKEY_INSTANCE_ID = $originalInstanceId
    foreach ($pidToStop in @($secondPid, $firstPid)) {
        if ($pidToStop) {
            $process = Get-CimInstance Win32_Process -Filter "ProcessId=$pidToStop" -ErrorAction SilentlyContinue
            if ($process -and $process.ExecutablePath -eq $stagedExe) {
                Stop-Process -Id $pidToStop -Force -ErrorAction SilentlyContinue
                Wait-Process -Id $pidToStop -ErrorAction SilentlyContinue
            }
        }
    }
    if (Test-Path -LiteralPath $smokeRoot) {
        $removed = $false
        for ($attempt = 0; $attempt -lt 20; $attempt++) {
            try {
                Remove-Item -LiteralPath $smokeRoot -Recurse -Force
                $removed = $true
                break
            } catch {
                Start-Sleep -Milliseconds 250
            }
        }
        if (-not $removed) {
            throw "could not remove smoke-owned root after process exit: $smokeRoot"
        }
    }
}
