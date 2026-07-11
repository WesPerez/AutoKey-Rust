use anyhow::{bail, Context, Result};
use parking_lot::RwLock;
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::sync::Arc;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::{InvalidateRect, UpdateWindow};
use windows::Win32::System::Threading::{
    GetCurrentProcessId, GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::*;

#[derive(Clone, Debug)]
pub struct WindowInfo {
    pub hwnd: isize,
    pub title: String,
}

#[derive(Debug)]
struct ProcessHandle(isize);

impl ProcessHandle {
    fn open(process_id: u32) -> Result<Self> {
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id) }
            .context("无法打开目标进程进行身份验证")?;
        Ok(Self(handle.0 as isize))
    }

    fn is_alive(&self) -> bool {
        let mut exit_code = 0u32;
        unsafe {
            GetExitCodeProcess(HANDLE(self.0 as *mut _), &mut exit_code).is_ok()
                && exit_code == STILL_ACTIVE.0 as u32
        }
    }
}

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(HANDLE(self.0 as *mut _));
        }
    }
}

#[derive(Clone, Debug)]
pub struct BoundWindow {
    hwnd: isize,
    process_id: u32,
    thread_id: u32,
    class_name: String,
    title_at_bind: String,
    process: Arc<ProcessHandle>,
}

impl BoundWindow {
    pub fn hwnd(&self) -> isize {
        self.hwnd
    }

    pub fn title(&self) -> String {
        let current = get_window_title(self.hwnd);
        if current.is_empty() {
            self.title_at_bind.clone()
        } else {
            current
        }
    }

    pub fn validate(&self) -> bool {
        if !self.process.is_alive() || !is_window_valid(self.hwnd) {
            return false;
        }
        let Some((thread_id, process_id)) = window_owner(self.hwnd) else {
            return false;
        };
        process_id == self.process_id
            && thread_id == self.thread_id
            && get_window_class(self.hwnd).is_some_and(|name| name == self.class_name)
    }
}

#[derive(Clone, Debug)]
pub struct BindingSnapshot {
    generation: u64,
    target: BoundWindow,
}

impl BindingSnapshot {
    pub fn target(&self) -> &BoundWindow {
        &self.target
    }
}

#[derive(Default)]
struct BindingState {
    generation: u64,
    target: Option<BoundWindow>,
}

#[derive(Default)]
pub struct WindowBinding {
    state: RwLock<BindingState>,
}

impl WindowBinding {
    pub fn bind_candidate(&self, hwnd: isize) -> Result<BindingSnapshot> {
        let target = capture_bound_window(hwnd)?;
        let mut state = self.state.write();
        state.generation = state.generation.wrapping_add(1);
        state.target = Some(target.clone());
        Ok(BindingSnapshot {
            generation: state.generation,
            target,
        })
    }

    pub fn clear(&self) {
        let mut state = self.state.write();
        state.generation = state.generation.wrapping_add(1);
        state.target = None;
    }

    pub fn snapshot(&self) -> Option<BindingSnapshot> {
        let state = self.state.read();
        state.target.clone().map(|target| BindingSnapshot {
            generation: state.generation,
            target,
        })
    }

    pub fn is_current(&self, snapshot: &BindingSnapshot) -> bool {
        let state = self.state.read();
        state.generation == snapshot.generation
            && state.target.as_ref().is_some_and(BoundWindow::validate)
    }

    pub fn clear_if_current(&self, snapshot: &BindingSnapshot) -> bool {
        let mut state = self.state.write();
        if state.generation != snapshot.generation {
            return false;
        }
        state.generation = state.generation.wrapping_add(1);
        state.target = None;
        true
    }

    pub fn with_current_target<T>(
        &self,
        snapshot: &BindingSnapshot,
        action: impl FnOnce(&BoundWindow) -> Result<T>,
    ) -> Result<T> {
        let state = self.state.read();
        if state.generation != snapshot.generation {
            bail!("绑定窗口已更改");
        }
        let target = state.target.as_ref().context("绑定窗口已解除")?;
        if !target.validate() {
            bail!("绑定窗口身份已变化或窗口已失效");
        }
        action(target)
    }
}

pub fn is_window_valid(hwnd: isize) -> bool {
    // SAFETY: IsWindow only inspects this opaque handle value.
    hwnd != 0 && unsafe { IsWindow(HWND(hwnd as *mut _)).as_bool() }
}

fn window_owner(hwnd: isize) -> Option<(u32, u32)> {
    if !is_window_valid(hwnd) {
        return None;
    }
    let mut process_id = 0u32;
    let thread_id =
        unsafe { GetWindowThreadProcessId(HWND(hwnd as *mut _), Some(&mut process_id)) };
    (thread_id != 0 && process_id != 0).then_some((thread_id, process_id))
}

fn get_window_class(hwnd: isize) -> Option<String> {
    let mut buffer = [0u16; 256];
    let copied = unsafe { GetClassNameW(HWND(hwnd as *mut _), &mut buffer) };
    (copied > 0).then(|| {
        OsString::from_wide(&buffer[..copied as usize])
            .to_string_lossy()
            .into_owned()
    })
}

fn capture_bound_window(hwnd: isize) -> Result<BoundWindow> {
    let hwnd = bindable_root_hwnd(hwnd).context("未找到可绑定的目标窗口")?;
    let (thread_id, process_id) = window_owner(hwnd).context("无法读取目标窗口身份")?;
    let class_name = get_window_class(hwnd).context("无法读取目标窗口类")?;
    if class_name.is_empty() {
        bail!("目标窗口类为空");
    }
    let target = BoundWindow {
        hwnd,
        process_id,
        thread_id,
        class_name,
        title_at_bind: get_window_title(hwnd),
        process: Arc::new(ProcessHandle::open(process_id)?),
    };
    if !target.validate() {
        bail!("目标窗口在绑定过程中已变化，请重试");
    }
    Ok(target)
}

pub fn is_own_process_window(hwnd: isize) -> bool {
    if !is_window_valid(hwnd) {
        return false;
    }
    // SAFETY: The process-id pointer is valid and hwnd is treated as an opaque handle.
    unsafe {
        let mut process_id = 0u32;
        GetWindowThreadProcessId(HWND(hwnd as *mut _), Some(&mut process_id));
        process_id == GetCurrentProcessId()
    }
}

pub fn is_bindable_window(hwnd: isize) -> bool {
    if !is_window_valid(hwnd) || is_own_process_window(hwnd) {
        return false;
    }

    unsafe {
        let hwnd = HWND(hwnd as *mut _);
        if is_shell_or_desktop(hwnd) || !IsWindowVisible(hwnd).as_bool() {
            return false;
        }

        let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        ex_style & WS_EX_TOOLWINDOW.0 == 0
    }
}

fn bindable_root_hwnd(hwnd: isize) -> Option<isize> {
    if !is_window_valid(hwnd) {
        return None;
    }

    unsafe {
        let hwnd = HWND(hwnd as *mut _);
        for candidate in [
            GetAncestor(hwnd, GA_ROOT),
            GetAncestor(hwnd, GA_ROOTOWNER),
            hwnd,
        ] {
            let candidate_hwnd = candidate.0 as isize;
            if is_bindable_window(candidate_hwnd) {
                return Some(candidate_hwnd);
            }
        }
    }

    None
}

pub fn find_own_hwnd() -> Option<isize> {
    let mut found = 0isize;
    unsafe {
        let _ = EnumWindows(
            Some(find_own_main_window),
            LPARAM((&mut found as *mut isize) as isize),
        );
    }
    if found == 0 {
        None
    } else {
        Some(found)
    }
}

pub fn restore_own_main_window() -> bool {
    let Some(hwnd) = find_own_hwnd() else {
        return false;
    };
    unsafe {
        let hwnd = HWND(hwnd as *mut _);
        let _ = ShowWindowAsync(hwnd, SW_RESTORE);
        let _ = ShowWindowAsync(hwnd, SW_SHOW);
        let _ = InvalidateRect(hwnd, None, true);
        let _ = UpdateWindow(hwnd);
        let _ = SetForegroundWindow(hwnd);
    }
    true
}

pub fn hide_own_main_window() -> bool {
    let Some(hwnd) = find_own_hwnd() else {
        return false;
    };
    unsafe { ShowWindowAsync(HWND(hwnd as *mut _), SW_HIDE).as_bool() }
}

fn is_shell_or_desktop(hwnd: HWND) -> bool {
    unsafe {
        hwnd.0.is_null()
            || hwnd == GetDesktopWindow()
            || hwnd == GetShellWindow()
            || hwnd == GetAncestor(GetShellWindow(), GA_ROOT)
    }
}

pub fn get_window_title(hwnd: isize) -> String {
    // SAFETY: The UTF-16 buffer is writable and sized from GetWindowTextLengthW.
    unsafe {
        let hwnd = HWND(hwnd as *mut _);
        let length = GetWindowTextLengthW(hwnd);
        if length <= 0 {
            return String::new();
        }

        let mut buffer = vec![0u16; length as usize + 1];
        let copied = GetWindowTextW(hwnd, &mut buffer);
        if copied <= 0 {
            return String::new();
        }

        OsString::from_wide(&buffer[..copied as usize])
            .to_string_lossy()
            .trim()
            .to_owned()
    }
}

pub fn enumerate_windows() -> Vec<WindowInfo> {
    let mut windows: Vec<WindowInfo> = Vec::new();
    // SAFETY: EnumWindows is synchronous, so the vector pointer remains valid.
    unsafe {
        let _ = EnumWindows(
            Some(enum_window_callback),
            LPARAM((&mut windows as *mut Vec<WindowInfo>) as isize),
        );
    }
    windows.sort_by_key(|window| window.title.to_lowercase());
    windows.dedup_by_key(|window| window.hwnd);
    windows
}

unsafe extern "system" fn enum_window_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
    if lparam.0 == 0 || !is_bindable_window(hwnd.0 as isize) {
        return TRUE;
    }

    let title = get_window_title(hwnd.0 as isize);
    if !title.is_empty() {
        let windows = &mut *(lparam.0 as *mut Vec<WindowInfo>);
        windows.push(WindowInfo {
            hwnd: hwnd.0 as isize,
            title,
        });
    }

    TRUE
}

unsafe extern "system" fn find_own_main_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
    if lparam.0 == 0 {
        return FALSE;
    }

    let mut process_id = 0u32;
    GetWindowThreadProcessId(hwnd, Some(&mut process_id));
    if process_id != GetCurrentProcessId() {
        return TRUE;
    }

    let title = get_window_title(hwnd.0 as isize);
    // Cache the marker to avoid repeated obfstr! decoding in the EnumWindows callback
    use once_cell::sync::Lazy;
    static MARKER: Lazy<String> = Lazy::new(|| crate::obfstr!("调度器"));
    if !title.starts_with(MARKER.as_str()) {
        return TRUE;
    }

    *(lparam.0 as *mut isize) = hwnd.0 as isize;
    FALSE
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_target(hwnd: isize) -> BoundWindow {
        let process_id = unsafe { GetCurrentProcessId() };
        BoundWindow {
            hwnd,
            process_id,
            thread_id: 1,
            class_name: "test".to_owned(),
            title_at_bind: "test".to_owned(),
            process: Arc::new(ProcessHandle::open(process_id).unwrap()),
        }
    }

    #[test]
    fn stale_snapshot_cannot_clear_new_binding() {
        let binding = WindowBinding::default();
        {
            let mut state = binding.state.write();
            state.generation = 1;
            state.target = Some(dummy_target(1));
        }
        let stale = binding.snapshot().unwrap();
        {
            let mut state = binding.state.write();
            state.generation = 2;
            state.target = Some(dummy_target(2));
        }

        assert!(!binding.clear_if_current(&stale));
        assert_eq!(binding.snapshot().unwrap().target().hwnd(), 2);
    }

    #[test]
    fn stale_snapshot_cannot_dispatch_to_old_target() {
        let binding = WindowBinding::default();
        {
            let mut state = binding.state.write();
            state.generation = 1;
            state.target = Some(dummy_target(1));
        }
        let stale = binding.snapshot().unwrap();
        {
            let mut state = binding.state.write();
            state.generation = 2;
            state.target = Some(dummy_target(2));
        }

        let called = std::sync::atomic::AtomicBool::new(false);
        let result = binding.with_current_target(&stale, |_| {
            called.store(true, std::sync::atomic::Ordering::Release);
            Ok(())
        });
        assert!(result.is_err());
        assert!(!called.load(std::sync::atomic::Ordering::Acquire));
    }

    #[test]
    fn invalid_window_identity_is_rejected() {
        assert!(!dummy_target(0).validate());
    }
}
