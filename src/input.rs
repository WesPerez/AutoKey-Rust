use anyhow::{anyhow, bail, Context, Result};
use windows::Win32::Foundation::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

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

        // Randomize the repeat count field (bits 0-15) to avoid fixed lParam patterns.
        // Real keyboard events have repeat count = 1 for typical key presses,
        // but varying it slightly (1-3) makes the pattern harder to fingerprint.
        let repeat_count = fastrand::u32(1..=3);
        bits = (bits & !0xFFFF) | repeat_count;

        // Occasionally add the "previous key state" flag (bit 30) on key-up
        // to mimic real keyboard driver behavior more closely.
        if is_key_up {
            bits |= (1 << 30) | (1 << 31);
            // Randomly set the "context code" bit (bit 29) on some key-up events
            // to simulate Alt-key context variations
            if fastrand::f64() < 0.05 {
                bits |= 1 << 29;
            }
        }

        PostMessageW(
            hwnd,
            if is_key_up { WM_KEYUP } else { WM_KEYDOWN },
            WPARAM(vk_code as usize),
            LPARAM(bits as isize),
        )
        .context(if is_key_up {
            "投递 KeyUp 失败"
        } else {
            "投递 KeyDown 失败"
        })?;
    }
    Ok(())
}

/// Generate a randomized dwExtraInfo value.
/// Instead of a fixed marker (like the old 0x41554B59 "AUKY"), we produce
/// small random values that blend in with normal keyboard driver output.
/// Most real keyboard events have dwExtraInfo = 0, but some drivers
/// (e.g., touch keyboard, remote desktop) set non-zero values.
fn random_extra_info() -> usize {
    // 85% chance of 0 (matches most real keyboard input)
    // 10% chance of a small random value (1-255)
    // 5% chance of a slightly larger value (256-65535)
    let r = fastrand::f64();
    if r < 0.85 {
        0
    } else if r < 0.95 {
        fastrand::u32(1..=255) as usize
    } else {
        fastrand::u32(256..=65535) as usize
    }
}

fn send_input_event(vk_code: u16, is_key_up: bool) -> Result<()> {
    // SAFETY: INPUT is fully initialized and cbSize exactly matches INPUT.
    unsafe {
        let mut flags = KEYBD_EVENT_FLAGS(0);
        if is_key_up {
            flags |= KEYEVENTF_KEYUP;
        }
        if is_extended_key(vk_code) {
            flags |= KEYEVENTF_EXTENDEDKEY;
        }

        let input = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(vk_code),
                    wScan: 0,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: random_extra_info(),
                },
            },
        };

        // Use direct syscall to bypass API hook detection on SendInput.
        let sent = crate::syscall::send_input_direct(&[input], std::mem::size_of::<INPUT>() as i32);
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

    #[test]
    fn random_extra_info_distribution() {
        let mut has_zero = false;
        let mut has_small = false;
        let mut has_large = false;
        for _ in 0..500 {
            let val = random_extra_info();
            if val == 0 {
                has_zero = true;
            } else if val <= 255 {
                has_small = true;
            } else {
                has_large = true;
            }
        }
        assert!(has_zero, "should produce 0 values");
        assert!(has_small, "should produce small non-zero values");
        // has_large is probabilistic, may not always trigger in 500 samples
    }
}
