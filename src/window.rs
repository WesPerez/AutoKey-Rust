use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use windows::Win32::Foundation::*;
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::obfstr;

#[derive(Clone, Debug)]
pub struct WindowInfo {
    pub hwnd: isize,
    pub title: String,
}

pub fn is_window_valid(hwnd: isize) -> bool {
    // SAFETY: IsWindow only inspects this opaque handle value.
    !hwnd.is_negative() && unsafe { IsWindow(HWND(hwnd as *mut _)).as_bool() }
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
    let mut found = 0isize;
    // SAFETY: EnumWindows is synchronous, so the output pointer remains valid.
    unsafe {
        let _ = EnumWindows(
            Some(find_own_main_window),
            LPARAM((&mut found as *mut isize) as isize),
        );
        if found == 0 {
            return false;
        }

        let hwnd = HWND(found as *mut _);
        let _ = ShowWindowAsync(hwnd, SW_RESTORE);
        let _ = ShowWindowAsync(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
    true
}

pub fn request_own_main_window_close() -> bool {
    let Some(hwnd) = find_own_hwnd() else {
        return false;
    };
    // SAFETY: Posting WM_CLOSE to our own top-level window requests the normal
    // eframe close path instead of bypassing Drop/autosave with process::exit.
    unsafe { PostMessageW(HWND(hwnd as *mut _), WM_CLOSE, WPARAM(0), LPARAM(0)).is_ok() }
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
    if lparam.0 == 0 || !IsWindowVisible(hwnd).as_bool() {
        return TRUE;
    }

    let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
    if ex_style & WS_EX_TOOLWINDOW.0 != 0 {
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
    if !title.starts_with(&obfstr!("调度器")) {
        return TRUE;
    }

    *(lparam.0 as *mut isize) = hwnd.0 as isize;
    FALSE
}
