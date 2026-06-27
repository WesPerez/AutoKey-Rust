use anyhow::{anyhow, bail, Context, Result};
use windows::Win32::Foundation::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::stealth;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputTarget {
    Foreground,
    Window(isize),
}

pub fn key_down(target: InputTarget, vk_code: u16) -> Result<()> {
    send_key_event(target, vk_code, false)
}

pub fn key_up(target: InputTarget, vk_code: u16) -> Result<()> {
    send_key_event(target, vk_code, true)
}

fn send_key_event(target: InputTarget, vk_code: u16, is_key_up: bool) -> Result<()> {
    validate_vk(vk_code)?;
    match target {
        InputTarget::Foreground => send_input_event(vk_code, is_key_up),
        InputTarget::Window(hwnd) => post_window_event(hwnd, vk_code, is_key_up),
    }
}

fn post_window_event(hwnd: isize, vk_code: u16, is_key_up: bool) -> Result<()> {
    let hwnd = HWND(hwnd as *mut _);
    // SAFETY: hwnd is validated before use and the key message fields follow Win32 layout.
    unsafe {
        if !IsWindow(hwnd).as_bool() {
            bail!("绑定窗口已失效");
        }

        let scan_code = MapVirtualKeyW(vk_code as u32, MAPVK_VK_TO_VSC);
        let mut bits = 1u32 | (scan_code << 16);
        if is_extended_key(vk_code) {
            bits |= 1 << 24;
        }

        if is_key_up {
            bits |= (1 << 30) | (1 << 31);
        }

        // Randomize reserved/unused bits to avoid pattern detection
        let lparam = stealth::randomize_lparam(bits, is_key_up);

        PostMessageW(
            hwnd,
            if is_key_up { WM_KEYUP } else { WM_KEYDOWN },
            WPARAM(vk_code as usize),
            LPARAM(lparam),
        )
        .context(if is_key_up {
            "投递 KeyUp 失败"
        } else {
            "投递 KeyDown 失败"
        })?;
    }
    Ok(())
}

fn send_input_event(vk_code: u16, is_key_up: bool) -> Result<()> {
    let is_extended = is_extended_key(vk_code);

    // NOTE: Direct syscall to NtUserSendInput is disabled because:
    // 1. The syscall number resolution (0xB8 opcode scan) is heuristic and
    //    may match the wrong instruction, calling an unintended kernel function.
    // 2. Direct syscalls bypass the LLKHF_INJECTED flag that the kernel sets
    //    for SendInput events. Without this flag, our own WH_KEYBOARD_LL hook
    //    treats injected keys as real input, breaking the Alt toggle state machine.
    //
    // The standard SendInput path with randomized dwExtraInfo is sufficient
    // for anti-detection purposes.

    // SAFETY: INPUT is fully initialized and cbSize exactly matches INPUT.
    unsafe {
        let mut flags = KEYBD_EVENT_FLAGS(0);
        if is_key_up {
            flags |= KEYEVENTF_KEYUP;
        }
        if is_extended {
            flags |= KEYEVENTF_EXTENDEDKEY;
        }

        let scan_code = MapVirtualKeyW(vk_code as u32, MAPVK_VK_TO_VSC) as u16;
        let input = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(vk_code),
                    wScan: scan_code,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: stealth::random_extra_info(),
                },
            },
        };

        let sent = SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
        if sent != 1 {
            return Err(anyhow!("SendInput 仅发送了 {sent} 个事件"));
        }
    }
    Ok(())
}

fn validate_vk(vk_code: u16) -> Result<()> {
    if !(1..=254).contains(&vk_code) {
        bail!("虚拟键码 {vk_code} 超出有效范围 1..=254");
    }
    Ok(())
}

fn is_extended_key(vk: u16) -> bool {
    matches!(
        vk,
        0x21 | 0x22
            | 0x23
            | 0x24
            | 0x25
            | 0x26
            | 0x27
            | 0x28
            | 0x2D
            | 0x2E
            | 0x5B
            | 0x5C
            | 0x5D
            | 0x90
            | 0xA3
            | 0xA5
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_virtual_key_range() {
        assert!(validate_vk(0).is_err());
        assert!(validate_vk(0x41).is_ok());
        assert!(validate_vk(255).is_err());
    }
}
