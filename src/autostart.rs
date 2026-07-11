use anyhow::{bail, Context, Result};
use once_cell::sync::Lazy;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};
use winreg::enums::{HKEY_CURRENT_USER, KEY_WRITE};
use winreg::RegKey;

pub const STARTUP_LINK_NAME: &str = "AutoKey-Rust.lnk";
pub const STARTUP_ARGUMENT: &str = "--autostart";
const TASK_PATH: &str = r"\";

const POWERSHELL_TIMEOUT: Duration = Duration::from_secs(30);
const STATUS_CACHE_TTL: Duration = Duration::from_secs(30);
const RUN_KEY_PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const STARTUP_APPROVED_RUN: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
const STARTUP_APPROVED_RUN32: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run32";
const STARTUP_APPROVED_FOLDER: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\StartupFolder";
const CURRENT_RUN_VALUE: &str = "AutoKeyRust";
const LEGACY_RUST_RUN_VALUE: &str = "{A1B2C3D4-E5F6-7890-ABCD-EF1234567890}";
const LEGACY_CSHARP_RUN_VALUE: &str = "AutoKey";

static STATUS_CACHE: Lazy<Mutex<Option<(Instant, AutostartStatus)>>> =
    Lazy::new(|| Mutex::new(None));

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutostartStatus {
    pub supported: bool,
    pub enabled: bool,
    pub link_path: PathBuf,
    pub target_path: PathBuf,
    pub arguments: String,
    pub working_dir: PathBuf,
    pub backend: Option<AutostartBackend>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutostartBackend {
    StartupShortcut,
    ElevatedScheduledTask,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ShortcutInfo {
    target: PathBuf,
    arguments: String,
    working_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ScheduledTaskInfo {
    target: PathBuf,
    arguments: String,
    working_dir: PathBuf,
    run_level: String,
    state: String,
}

#[derive(Debug, Clone)]
struct StartupManager {
    link_path: PathBuf,
    target_path: PathBuf,
    arguments: String,
    working_dir: PathBuf,
    task_name: String,
}

impl StartupManager {
    fn current() -> Result<Self> {
        let appdata = std::env::var_os("AUTOKEY_APPDATA_ROOT")
            .or_else(|| std::env::var_os("APPDATA"))
            .context("APPDATA 未设置")?;
        let target_path = std::env::current_exe().context("无法确定程序路径")?;
        let working_dir = target_path
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .to_path_buf();
        Ok(Self {
            link_path: startup_link_path_from_appdata(appdata),
            target_path,
            arguments: STARTUP_ARGUMENT.to_owned(),
            working_dir,
            task_name: scheduled_task_name(),
        })
    }

    fn status(&self) -> Result<AutostartStatus> {
        let shortcut = read_shortcut_info(&self.link_path)?;
        let task = read_scheduled_task(&self.task_name)?;
        Ok(self.status_from_info(shortcut, startup_folder_disabled(), task))
    }

    fn status_from_info(
        &self,
        info: Option<ShortcutInfo>,
        disabled_by_windows: bool,
        task: Option<ScheduledTaskInfo>,
    ) -> AutostartStatus {
        let shortcut_enabled = !disabled_by_windows
            && info.is_some_and(|info| {
                same_path(&info.target, &self.target_path)
                    && info.arguments.trim().eq_ignore_ascii_case(&self.arguments)
                    && same_path(&info.working_dir, &self.working_dir)
            });
        let task_enabled = task.is_some_and(|task| {
            same_path(&task.target, &self.target_path)
                && task.arguments.trim().eq_ignore_ascii_case(&self.arguments)
                && same_path(&task.working_dir, &self.working_dir)
                && task.run_level.eq_ignore_ascii_case("Highest")
                && !task.state.eq_ignore_ascii_case("Disabled")
        });
        let backend = if task_enabled {
            Some(AutostartBackend::ElevatedScheduledTask)
        } else if shortcut_enabled {
            Some(AutostartBackend::StartupShortcut)
        } else {
            None
        };
        AutostartStatus {
            supported: cfg!(windows),
            enabled: backend.is_some(),
            link_path: self.link_path.clone(),
            target_path: self.target_path.clone(),
            arguments: self.arguments.clone(),
            working_dir: self.working_dir.clone(),
            backend,
        }
    }

    fn set_enabled(&self, enabled: bool) -> Result<AutostartStatus> {
        cleanup_previous_registry_autostart()?;
        if enabled {
            let current = self.status()?;
            let elevated = current_process_is_elevated();
            let desired_backend = desired_backend(elevated);
            if current.backend == Some(desired_backend)
                || (!elevated && current.backend == Some(AutostartBackend::ElevatedScheduledTask))
            {
                return Ok(current);
            }
            if elevated {
                remove_matching_shortcut(&self.link_path, &self.target_path)?;
                write_scheduled_task(
                    &self.task_name,
                    &self.target_path,
                    &self.arguments,
                    &self.working_dir,
                )?;
            } else {
                if let Some(parent) = self.link_path.parent() {
                    fs::create_dir_all(parent).context("无法创建 Windows 启动文件夹")?;
                }
                write_shortcut(
                    &self.link_path,
                    &self.target_path,
                    &self.arguments,
                    &self.working_dir,
                )?;
                clear_startup_folder_approval()?;
            }
        } else {
            remove_matching_shortcut(&self.link_path, &self.target_path)?;
            clear_startup_folder_approval()?;
            remove_scheduled_task(&self.task_name)?;
        }

        let verified = self.status()?;
        if verified.enabled != enabled {
            bail!(
                "开机启动快捷方式写入后验证失败: link={} target={} args={} working_dir={}",
                verified.link_path.display(),
                verified.target_path.display(),
                verified.arguments,
                verified.working_dir.display()
            );
        }
        Ok(verified)
    }
}

pub fn status() -> Result<AutostartStatus> {
    StartupManager::current()?.status()
}

pub fn is_enabled() -> bool {
    if let Ok(cache) = STATUS_CACHE.lock() {
        if let Some((checked_at, status)) = cache.as_ref() {
            if checked_at.elapsed() < STATUS_CACHE_TTL {
                return status.enabled;
            }
        }
    }
    let status = status().unwrap_or_else(|_| unsupported_status());
    let enabled = status.enabled;
    if let Ok(mut cache) = STATUS_CACHE.lock() {
        *cache = Some((Instant::now(), status));
    }
    enabled
}

pub fn set_enabled(enabled: bool) -> Result<AutostartStatus> {
    let status = StartupManager::current()?.set_enabled(enabled)?;
    if let Ok(mut cache) = STATUS_CACHE.lock() {
        *cache = Some((Instant::now(), status.clone()));
    }
    Ok(status)
}

fn unsupported_status() -> AutostartStatus {
    AutostartStatus {
        supported: false,
        enabled: false,
        link_path: PathBuf::new(),
        target_path: PathBuf::new(),
        arguments: STARTUP_ARGUMENT.to_owned(),
        working_dir: PathBuf::new(),
        backend: None,
    }
}

pub fn startup_link_path_from_appdata(appdata: impl AsRef<Path>) -> PathBuf {
    appdata
        .as_ref()
        .join("Microsoft")
        .join("Windows")
        .join("Start Menu")
        .join("Programs")
        .join("Startup")
        .join(STARTUP_LINK_NAME)
}

fn read_shortcut_info(path: &Path) -> Result<Option<ShortcutInfo>> {
    if !path.exists() {
        return Ok(None);
    }
    let script = format!(
        "$s=New-Object -ComObject WScript.Shell; \
         $l=$s.CreateShortcut({}); \
         Write-Output $l.TargetPath; \
         Write-Output $l.Arguments; \
         Write-Output $l.WorkingDirectory",
        ps_quote(path)
    );
    let output = run_powershell(&script)?;
    let lines = output.lines().collect::<Vec<_>>();
    let target = lines.first().map(|line| line.trim()).unwrap_or_default();
    if target.is_empty() {
        return Ok(None);
    }
    Ok(Some(ShortcutInfo {
        target: PathBuf::from(target),
        arguments: lines
            .get(1)
            .map(|line| line.trim())
            .unwrap_or_default()
            .to_owned(),
        working_dir: PathBuf::from(lines.get(2).map(|line| line.trim()).unwrap_or_default()),
    }))
}

fn write_shortcut(
    link_path: &Path,
    target_path: &Path,
    arguments: &str,
    working_dir: &Path,
) -> Result<()> {
    let script = format!(
        "$s=New-Object -ComObject WScript.Shell; \
         $l=$s.CreateShortcut({}); \
         $l.TargetPath={}; \
         $l.Arguments={}; \
         $l.WorkingDirectory={}; \
         $l.Save()",
        ps_quote(link_path),
        ps_quote(target_path),
        ps_quote(arguments),
        ps_quote(working_dir)
    );
    run_powershell(&script).map(|_| ())
}

fn remove_matching_shortcut(link_path: &Path, target_path: &Path) -> Result<()> {
    let Some(info) = read_shortcut_info(link_path)? else {
        return Ok(());
    };
    if same_path(&info.target, target_path) {
        fs::remove_file(link_path).context("无法删除开机启动快捷方式")?;
    }
    Ok(())
}

fn scheduled_task_name() -> String {
    let identity = [
        std::env::var("USERDOMAIN").ok(),
        std::env::var("USERNAME").ok(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("_");
    let identity = identity
        .chars()
        .filter(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
        .take(80)
        .collect::<String>();
    if identity.is_empty() {
        "AutoKey-Rust Startup".to_owned()
    } else {
        format!("AutoKey-Rust Startup - {identity}")
    }
}

fn read_scheduled_task(task_name: &str) -> Result<Option<ScheduledTaskInfo>> {
    let script = format!(
        "$t=Get-ScheduledTask -TaskPath {} -TaskName {} -ErrorAction SilentlyContinue; \
         if($null -eq $t){{exit 0}}; \
         $a=$t.Actions | Select-Object -First 1; \
         Write-Output $a.Execute; \
         Write-Output $a.Arguments; \
         Write-Output $a.WorkingDirectory; \
         Write-Output $t.Principal.RunLevel; \
         Write-Output $t.State",
        ps_quote(TASK_PATH),
        ps_quote(task_name)
    );
    let output = run_powershell(&script)?;
    let lines = output.lines().map(str::trim).collect::<Vec<_>>();
    let target = lines.first().copied().unwrap_or_default();
    if target.is_empty() {
        return Ok(None);
    }
    Ok(Some(ScheduledTaskInfo {
        target: PathBuf::from(target),
        arguments: lines.get(1).copied().unwrap_or_default().to_owned(),
        working_dir: PathBuf::from(lines.get(2).copied().unwrap_or_default()),
        run_level: lines.get(3).copied().unwrap_or_default().to_owned(),
        state: lines.get(4).copied().unwrap_or_default().to_owned(),
    }))
}

fn write_scheduled_task(
    task_name: &str,
    target_path: &Path,
    arguments: &str,
    working_dir: &Path,
) -> Result<()> {
    let script = format!(
        "$identity=[System.Security.Principal.WindowsIdentity]::GetCurrent().Name; \
         $action=New-ScheduledTaskAction -Execute {} -Argument {} -WorkingDirectory {}; \
         $trigger=New-ScheduledTaskTrigger -AtLogOn -User $identity; \
         $principal=New-ScheduledTaskPrincipal -UserId $identity -LogonType Interactive -RunLevel Highest; \
         Register-ScheduledTask -TaskPath {} -TaskName {} -Action $action -Trigger $trigger -Principal $principal -Description 'AutoKey-Rust per-user startup' -Force | Out-Null",
        ps_quote(target_path),
        ps_quote(arguments),
        ps_quote(working_dir),
        ps_quote(TASK_PATH),
        ps_quote(task_name)
    );
    run_powershell(&script).map(|_| ())
}

fn remove_scheduled_task(task_name: &str) -> Result<()> {
    if read_scheduled_task(task_name)?.is_none() {
        return Ok(());
    }
    let script = format!(
        "Unregister-ScheduledTask -TaskPath {} -TaskName {} -Confirm:$false -ErrorAction Stop",
        ps_quote(TASK_PATH),
        ps_quote(task_name)
    );
    run_powershell(&script)
        .map(|_| ())
        .context("无法删除管理员开机启动计划任务，请以管理员身份运行后重试")
}

fn current_process_is_elevated() -> bool {
    unsafe { windows::Win32::UI::Shell::IsUserAnAdmin().as_bool() }
}

fn desired_backend(elevated: bool) -> AutostartBackend {
    if elevated {
        AutostartBackend::ElevatedScheduledTask
    } else {
        AutostartBackend::StartupShortcut
    }
}

fn run_powershell(script: &str) -> Result<String> {
    let mut command = Command::new("powershell.exe");
    command
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    apply_no_window_flag(&mut command);
    let mut child = command
        .spawn()
        .context("无法启动 PowerShell 创建快捷方式")?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < POWERSHELL_TIMEOUT => {
                thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                bail!("PowerShell 快捷方式操作超时");
            }
            Err(error) => return Err(error.into()),
        }
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        bail!(
            "PowerShell 快捷方式操作失败: {}",
            if error.is_empty() {
                "未知错误"
            } else {
                &error
            }
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[cfg(windows)]
fn apply_no_window_flag(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(0x0800_0000);
}

#[cfg(not(windows))]
fn apply_no_window_flag(_command: &mut Command) {}

fn cleanup_previous_registry_autostart() -> Result<()> {
    let current_user = RegKey::predef(HKEY_CURRENT_USER);
    let mut removed_legacy_csharp = false;
    if let Ok(run_key) = current_user.open_subkey_with_flags(RUN_KEY_PATH, KEY_WRITE) {
        delete_value_if_present(&run_key, CURRENT_RUN_VALUE)?;
        delete_value_if_present(&run_key, LEGACY_RUST_RUN_VALUE)?;
        if let Ok(command) = run_key.get_value::<String, _>(LEGACY_CSHARP_RUN_VALUE) {
            if shortcut_target_from_command(&command).is_some_and(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.eq_ignore_ascii_case("AutoKey.exe")
                            || name.eq_ignore_ascii_case("KeyScheduler.exe")
                    })
            }) {
                delete_value_if_present(&run_key, LEGACY_CSHARP_RUN_VALUE)?;
                removed_legacy_csharp = true;
            }
        }
    }
    for path in [STARTUP_APPROVED_RUN, STARTUP_APPROVED_RUN32] {
        if let Ok(key) = current_user.open_subkey_with_flags(path, KEY_WRITE) {
            delete_value_if_present(&key, CURRENT_RUN_VALUE)?;
            delete_value_if_present(&key, LEGACY_RUST_RUN_VALUE)?;
            if removed_legacy_csharp {
                delete_value_if_present(&key, LEGACY_CSHARP_RUN_VALUE)?;
            }
        }
    }
    Ok(())
}

fn clear_startup_folder_approval() -> Result<()> {
    let current_user = RegKey::predef(HKEY_CURRENT_USER);
    if let Ok(key) = current_user.open_subkey_with_flags(STARTUP_APPROVED_FOLDER, KEY_WRITE) {
        delete_value_if_present(&key, STARTUP_LINK_NAME)?;
    }
    Ok(())
}

fn startup_folder_disabled() -> bool {
    let current_user = RegKey::predef(HKEY_CURRENT_USER);
    let Ok(key) = current_user.open_subkey(STARTUP_APPROVED_FOLDER) else {
        return false;
    };
    let Ok(value) = key.get_raw_value(STARTUP_LINK_NAME) else {
        return false;
    };
    value.bytes.first().copied() == Some(3)
}

fn delete_value_if_present(key: &RegKey, name: &str) -> Result<()> {
    if let Err(error) = key.delete_value(name) {
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(error.into());
        }
    }
    Ok(())
}

fn shortcut_target_from_command(command: &str) -> Option<PathBuf> {
    let command = command.trim();
    if let Some(rest) = command.strip_prefix('"') {
        return rest.find('"').map(|end| PathBuf::from(&rest[..end]));
    }
    command.split_whitespace().next().map(PathBuf::from)
}

fn ps_quote<T: PathOrStr + ?Sized>(value: &T) -> String {
    let value = value.as_os_string();
    format!("'{}'", value.to_string_lossy().replace('\'', "''"))
}

fn same_path(left: &Path, right: &Path) -> bool {
    let left = left.canonicalize().unwrap_or_else(|_| left.to_path_buf());
    let right = right.canonicalize().unwrap_or_else(|_| right.to_path_buf());
    left.to_string_lossy()
        .eq_ignore_ascii_case(&right.to_string_lossy())
}

trait PathOrStr {
    fn as_os_string(&self) -> OsString;
}

impl PathOrStr for Path {
    fn as_os_string(&self) -> OsString {
        self.as_os_str().to_os_string()
    }
}

impl PathOrStr for PathBuf {
    fn as_os_string(&self) -> OsString {
        self.as_os_str().to_os_string()
    }
}

impl PathOrStr for str {
    fn as_os_string(&self) -> OsString {
        OsString::from(self)
    }
}

impl PathOrStr for String {
    fn as_os_string(&self) -> OsString {
        OsString::from(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn startup_link_uses_user_startup_folder() {
        let path = startup_link_path_from_appdata(Path::new(r"C:\Users\me\AppData\Roaming"));
        assert_eq!(path.file_name().unwrap(), STARTUP_LINK_NAME);
        assert!(path.to_string_lossy().contains("Start Menu"));
        assert!(path.to_string_lossy().contains("Startup"));
    }

    #[test]
    fn status_requires_target_arguments_and_working_directory() {
        let manager = StartupManager {
            link_path: PathBuf::from(r"C:\Startup\AutoKey-Rust.lnk"),
            target_path: PathBuf::from(r"C:\Apps\AutoKeyRust.exe"),
            arguments: STARTUP_ARGUMENT.to_owned(),
            working_dir: PathBuf::from(r"C:\Apps"),
            task_name: "AutoKey-Rust Startup - test".to_owned(),
        };
        let matching = ShortcutInfo {
            target: manager.target_path.clone(),
            arguments: STARTUP_ARGUMENT.to_owned(),
            working_dir: manager.working_dir.clone(),
        };
        assert!(
            manager
                .status_from_info(Some(matching.clone()), false, None)
                .enabled
        );
        assert!(
            !manager
                .status_from_info(Some(matching.clone()), true, None)
                .enabled
        );
        let mut wrong_args = matching;
        wrong_args.arguments.clear();
        assert!(
            !manager
                .status_from_info(Some(wrong_args), false, None)
                .enabled
        );
    }

    #[test]
    fn elevation_selects_scheduled_task_backend() {
        assert_eq!(desired_backend(false), AutostartBackend::StartupShortcut);
        assert_eq!(
            desired_backend(true),
            AutostartBackend::ElevatedScheduledTask
        );
    }

    #[test]
    fn matching_highest_task_is_enabled() {
        let manager = StartupManager {
            link_path: PathBuf::from(r"C:\Startup\AutoKey-Rust.lnk"),
            target_path: PathBuf::from(r"C:\Apps\AutoKeyRust.exe"),
            arguments: STARTUP_ARGUMENT.to_owned(),
            working_dir: PathBuf::from(r"C:\Apps"),
            task_name: "AutoKey-Rust Startup - test".to_owned(),
        };
        let task = ScheduledTaskInfo {
            target: manager.target_path.clone(),
            arguments: STARTUP_ARGUMENT.to_owned(),
            working_dir: manager.working_dir.clone(),
            run_level: "Highest".to_owned(),
            state: "Ready".to_owned(),
        };
        let status = manager.status_from_info(None, false, Some(task));
        assert!(status.enabled);
        assert_eq!(
            status.backend,
            Some(AutostartBackend::ElevatedScheduledTask)
        );
    }

    #[cfg(windows)]
    #[test]
    fn writes_reads_and_removes_isolated_shortcut() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("autokey-rust-startup-{stamp}"));
        let manager = StartupManager {
            link_path: root.join("Startup").join(STARTUP_LINK_NAME),
            target_path: std::env::current_exe().unwrap(),
            arguments: STARTUP_ARGUMENT.to_owned(),
            working_dir: std::env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .to_path_buf(),
            task_name: "AutoKey-Rust Startup - isolated-test".to_owned(),
        };
        fs::create_dir_all(manager.link_path.parent().unwrap()).unwrap();
        write_shortcut(
            &manager.link_path,
            &manager.target_path,
            &manager.arguments,
            &manager.working_dir,
        )
        .unwrap();
        assert!(
            manager
                .status_from_info(read_shortcut_info(&manager.link_path).unwrap(), false, None)
                .enabled
        );
        fs::remove_file(&manager.link_path).unwrap();
        assert!(
            !manager
                .status_from_info(read_shortcut_info(&manager.link_path).unwrap(), false, None)
                .enabled
        );
        let _ = fs::remove_dir_all(root);
    }
}
