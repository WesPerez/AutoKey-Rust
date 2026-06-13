//! Direct syscall implementation for bypassing API hook detection.
//!
//! On x64 Windows, NtUserSendInput in win32u.dll follows the pattern:
//!   mov r10, rcx      ; 4C 8B D1
//!   mov eax, XXXXXXXX ; B8 XX XX 00 00  (syscall number)
//!   ...
//!   syscall            ; 0F 05
//!
//! We extract the syscall number at runtime and invoke it directly,
//! bypassing both IAT hooks (user32.dll) and inline hooks (win32u.dll).

use once_cell::sync::Lazy;
use parking_lot::Mutex;
use windows::core::PCSTR;
use windows::core::PCWSTR;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::UI::Input::KeyboardAndMouse::INPUT;

struct SyscallInfo {
    number: u32,
}

static SYSCALL_CACHE: Lazy<Mutex<Option<SyscallInfo>>> = Lazy::new(|| Mutex::new(None));

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// Extract the syscall number from the NtUserSendInput stub in win32u.dll.
fn extract_syscall_number() -> Option<u32> {
    unsafe {
        let dll_name = to_wide("win32u.dll");
        let module = LoadLibraryW(PCWSTR(dll_name.as_ptr())).ok()?;

        let func_name = b"NtUserSendInput\0";
        let func_ptr = GetProcAddress(module, PCSTR(func_name.as_ptr()))?;

        // Read the first 16 bytes of the stub
        let bytes = std::slice::from_raw_parts(func_ptr as *const u8, 16);

        // Pattern: 4C 8B D1 B8 XX XX 00 00
        // This is the standard pattern for win32k syscalls on Windows 10/11 x64
        let number = if bytes.len() >= 8
            && bytes[0] == 0x4C
            && bytes[1] == 0x8B
            && bytes[2] == 0xD1
            && bytes[3] == 0xB8
        {
            Some(u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]))
        } else {
            // Alternative: scan for B8 pattern within first 8 bytes
            (0..6).find_map(|offset| {
                if bytes.get(offset) == Some(&0xB8) && offset + 4 <= bytes.len() {
                    let num = u32::from_le_bytes([
                        bytes[offset + 1],
                        bytes[offset + 2],
                        bytes[offset + 3],
                        bytes[offset + 4],
                    ]);
                    if num > 0 && num < 0x2000 {
                        return Some(num);
                    }
                }
                None
            })
        };

        let _ = FreeLibrary(module.0 as isize);
        number
    }
}

// FreeLibrary via raw FFI (not exposed in windows crate's LibraryLoader feature)
#[link(name = "kernel32")]
extern "system" {
    fn FreeLibrary(hLibModule: isize) -> i32;
}

/// Send input via direct syscall, bypassing API hooks.
///
/// On x86_64, extracts the NtUserSendInput syscall number from win32u.dll
/// and invokes it directly via the `syscall` instruction. This bypasses:
/// - IAT hooks on user32.dll's SendInput
/// - Inline hooks on win32u.dll's NtUserSendInput
///
/// Falls back to standard SendInput if the syscall number cannot be resolved
/// or on non-x86_64 architectures.
pub unsafe fn send_input_direct(inputs: &[INPUT], cb_size: i32) -> u32 {
    #[cfg(target_arch = "x86_64")]
    {
        let number = {
            let mut cache = SYSCALL_CACHE.lock();
            if cache.is_none() {
                *cache = extract_syscall_number().map(|n| SyscallInfo { number: n });
            }
            cache.as_ref().map(|info| info.number)
        };

        if let Some(syscall_num) = number {
            let result: u32;
            core::arch::asm!(
                "mov r10, rcx",
                "syscall",
                in("rax") syscall_num as u64,
                in("rcx") inputs.len() as u64,
                in("rdx") inputs.as_ptr() as u64,
                in("r8") cb_size as u64,
                lateout("rax") result,
                lateout("rcx") _,
                lateout("r11") _,
                options(nostack, preserves_flags),
            );
            return result;
        }
    }

    // Fallback: standard SendInput via user32.dll
    windows::Win32::UI::Input::KeyboardAndMouse::SendInput(inputs, cb_size)
}
