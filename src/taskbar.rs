use std::fs;
use std::sync::Once;
use windows::core::{PCWSTR, PROPVARIANT};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Storage::EnhancedStorage::PKEY_AppUserModel_RelaunchIconResource;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Shell::PropertiesSystem::{
    IPropertyStore, PSCoerceToCanonicalValue, SHGetPropertyStoreForWindow,
};
use windows::Win32::UI::Shell::{ITaskbarList3, TaskbarList};
use windows::Win32::UI::WindowsAndMessaging::*;

pub struct TaskbarDecoration {
    enabled: bool,
    base_hicon: Option<isize>,
    overlay_hicon: Option<isize>,
}

impl TaskbarDecoration {
    pub fn new() -> Self {
        Self {
            enabled: std::env::var_os("AUTOKEY_TASKBAR_DECORATIONS").is_some(),
            base_hicon: None,
            overlay_hicon: None,
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn update(&mut self, is_running: bool, config_name: &str) -> bool {
        if !self.enabled {
            return true;
        }
        let Some(hwnd) = crate::window::find_own_hwnd() else {
            return false;
        };
        let base_ok = update_base_icon(hwnd, is_running, &mut self.base_hicon);
        let overlay_ok =
            update_overlay_icon(hwnd, is_running, config_name, &mut self.overlay_hicon);
        let relaunch_ok = update_relaunch_icon_resource(hwnd, is_running, config_name);
        base_ok && overlay_ok && relaunch_ok
    }
}

impl Drop for TaskbarDecoration {
    fn drop(&mut self) {
        for hicon in [self.base_hicon.take(), self.overlay_hicon.take()]
            .into_iter()
            .flatten()
        {
            unsafe {
                let _ = DestroyIcon(HICON(hicon as *mut _));
            }
        }
    }
}

fn create_hicon(is_running: bool) -> Option<isize> {
    const SIZE: u32 = crate::icon::ICON_SIZE as u32;
    let rgba = crate::icon::render_icon_rgba_unbadged(is_running);
    create_hicon_from_rgba(SIZE, &rgba)
}

fn create_overlay_hicon(is_running: bool, config_name: &str) -> Option<isize> {
    const SIZE: u32 = 64;
    let rgba = crate::icon::render_taskbar_overlay_rgba_at(SIZE as usize, is_running, config_name);
    create_hicon_from_rgba(SIZE, &rgba)
}

pub(crate) fn create_hicon_from_rgba(size: u32, rgba: &[u8]) -> Option<isize> {
    unsafe {
        let hdc = GetDC(None);
        if hdc.is_invalid() {
            return None;
        }
        let mut bmi: BITMAPINFO = std::mem::zeroed();
        bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = size as i32;
        bmi.bmiHeader.biHeight = -(size as i32);
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = 0;

        let mut bits_ptr: *mut std::ffi::c_void = std::ptr::null_mut();
        let color = match CreateDIBSection(hdc, &bmi, DIB_RGB_COLORS, &mut bits_ptr, None, 0) {
            Ok(bitmap) => bitmap,
            Err(_) => {
                let _ = ReleaseDC(None, hdc);
                return None;
            }
        };
        let _ = ReleaseDC(None, hdc);
        if bits_ptr.is_null() {
            let _ = DeleteObject(color);
            return None;
        }

        let bits = bits_ptr as *mut u8;
        for index in 0..(size * size) as usize {
            let r = rgba[index * 4];
            let g = rgba[index * 4 + 1];
            let b = rgba[index * 4 + 2];
            let a = rgba[index * 4 + 3];
            *bits.add(index * 4) = (b as u16 * a as u16 / 255) as u8;
            *bits.add(index * 4 + 1) = (g as u16 * a as u16 / 255) as u8;
            *bits.add(index * 4 + 2) = (r as u16 * a as u16 / 255) as u8;
            *bits.add(index * 4 + 3) = a;
        }

        let mask_row_bytes = (size.div_ceil(32) * 4) as usize;
        let mask_data = vec![0u8; mask_row_bytes * size as usize];
        let mask = CreateBitmap(
            size as i32,
            size as i32,
            1,
            1,
            Some(mask_data.as_ptr() as *const _),
        );
        let info = ICONINFO {
            fIcon: TRUE,
            hbmMask: mask,
            hbmColor: color,
            ..Default::default()
        };
        let icon = match CreateIconIndirect(&info) {
            Ok(icon) => icon,
            Err(_) => {
                let _ = DeleteObject(color);
                let _ = DeleteObject(mask);
                return None;
            }
        };
        let _ = DeleteObject(color);
        let _ = DeleteObject(mask);
        (!icon.is_invalid()).then_some(icon.0 as isize)
    }
}

fn update_base_icon(hwnd: isize, running: bool, old_icon: &mut Option<isize>) -> bool {
    let Some(icon) = create_hicon(running) else {
        return false;
    };
    unsafe {
        let hwnd = HWND(hwnd as *mut _);
        let _ = SendMessageW(hwnd, WM_SETICON, WPARAM(0), LPARAM(icon));
        let _ = SendMessageW(hwnd, WM_SETICON, WPARAM(1), LPARAM(icon));
        let _ = SendMessageW(hwnd, WM_SETICON, WPARAM(2), LPARAM(icon));
    }
    replace_icon(old_icon, icon);
    true
}

fn update_overlay_icon(
    hwnd: isize,
    running: bool,
    config_name: &str,
    old_icon: &mut Option<isize>,
) -> bool {
    let Some(icon) = create_overlay_hicon(running, config_name) else {
        return false;
    };
    let description = format!(
        "AutoKeyRust {}",
        crate::icon::config_badge_text(config_name)
    );
    let description: Vec<u16> = description.encode_utf16().chain(Some(0)).collect();
    let result = unsafe {
        static COM_INIT: Once = Once::new();
        COM_INIT.call_once(|| {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        });
        let taskbar: windows::core::Result<ITaskbarList3> =
            CoCreateInstance(&TaskbarList, None, CLSCTX_INPROC_SERVER);
        taskbar.and_then(|taskbar| {
            taskbar.HrInit()?;
            taskbar.SetOverlayIcon(
                HWND(hwnd as *mut _),
                HICON(icon as *mut _),
                PCWSTR(description.as_ptr()),
            )
        })
    };
    if result.is_ok() {
        replace_icon(old_icon, icon);
        true
    } else {
        unsafe {
            let _ = DestroyIcon(HICON(icon as *mut _));
        }
        false
    }
}

fn update_relaunch_icon_resource(hwnd: isize, running: bool, config_name: &str) -> bool {
    let badge = crate::icon::config_badge_text(config_name);
    let status = if running { "running" } else { "stopped" };
    let path = crate::config::app_directory().join(format!("taskbar-{status}-{badge}.ico"));
    let Some(parent) = path.parent() else {
        return false;
    };
    if fs::create_dir_all(parent).is_err()
        || fs::write(&path, crate::icon::render_icon_ico_unbadged(running)).is_err()
    {
        return false;
    }
    let resource = format!("{},0", path.display());
    unsafe {
        let hwnd = HWND(hwnd as *mut _);
        let Ok(store) = SHGetPropertyStoreForWindow::<_, IPropertyStore>(hwnd) else {
            return false;
        };
        let mut value = PROPVARIANT::from(resource.as_str());
        if PSCoerceToCanonicalValue(&PKEY_AppUserModel_RelaunchIconResource, &mut value).is_err() {
            return false;
        }
        store
            .SetValue(&PKEY_AppUserModel_RelaunchIconResource, &value)
            .and_then(|_| store.Commit())
            .is_ok()
    }
}

fn replace_icon(slot: &mut Option<isize>, icon: isize) {
    if let Some(old) = slot.replace(icon) {
        unsafe {
            let _ = DestroyIcon(HICON(old as *mut _));
        }
    }
}
