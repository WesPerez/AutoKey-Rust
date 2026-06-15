//! Anti-detection primitives for input injection.
//!
//! - Direct syscall to NtUserSendInput (bypasses IAT hooks on win32u.dll)
//! - dwExtraInfo randomization (simulates QPC timestamp range)
//! - PostMessage lParam randomization (reserved bits + occasional repeat count)
//! - Compile-time string obfuscation
//! - Anti-debug / anti-analysis helpers (active in init)
//! - Memory protection (secure zeroing)

use std::arch::asm;
use std::ffi::c_void;

// ── dwExtraInfo randomization ─────────────────────────────────────────

/// Return a random `dwExtraInfo` value that mimics a QPC timestamp.
///
/// Real hardware input carries a QPC-based value in `dwExtraInfo`.
/// We generate values in a plausible range to avoid the "completely
/// random 64-bit" fingerprint.
#[inline]
pub fn random_extra_info() -> usize {
    let lo: u64 = 100_000_000;
    let hi: u64 = 5_000_000_000;
    let v = lo + fastrand::u64(..) % (hi - lo);
    v as usize
}

// ── PostMessage lParam randomization ──────────────────────────────────

/// Randomize the unused/reserved bits of a WM_KEYDOWN / WM_KEYUP lParam.
///
/// We randomize bits 25-28 (reserved) and occasionally bump the repeat
/// count to 2 to simulate a held key. We do NOT touch bit 29 because
/// setting it would make the target app interpret the key as Alt+Key.
#[inline]
pub fn randomize_lparam(bits: u32, is_key_up: bool) -> isize {
    let mut lparam = bits;

    // Randomize reserved bits 25-28 with noise
    let reserved_noise = (fastrand::u32(..) & 0xF) << 25;
    lparam = (lparam & !(0xF << 25)) | reserved_noise;

    // For keydown events, occasionally set repeat count to 2 (~3% of the time)
    if !is_key_up && fastrand::f64() < 0.03 {
        lparam = (lparam & !0xFFFF) | 2;
    }

    lparam as isize
}

// ── Direct syscall: NtUserSendInput ───────────────────────────────────

/// Raw INPUT structure matching the Win32 layout for keyboard input.
/// Total: 40 bytes on x64 (matches sizeof(INPUT)).
#[repr(C)]
#[derive(Default)]
#[allow(non_snake_case)]
struct RawKeyboardInput {
    r#type: u32,
    _pad1: u32,
    wVk: u16,
    wScan: u16,
    dwFlags: u32,
    time: u32,
    _pad2: u32,
    dwExtraInfo: usize,
    _pad3: [u8; 8],
}

const INPUT_KEYBOARD: u32 = 1;
const KEYEVENTF_KEYUP: u32 = 0x0002;
const KEYEVENTF_EXTENDEDKEY: u32 = 0x0001;

/// Cached syscall number — resolved once, thread-safe.
static NTUSER_SEND_INPUT_NR: std::sync::OnceLock<Option<u32>> = std::sync::OnceLock::new();

/// Send a keyboard event via direct syscall to NtUserSendInput.
///
/// Only available on x86_64. Returns `false` on other architectures or
/// if the syscall number cannot be resolved (caller should fall back
/// to the standard SendInput path).
pub fn send_input_syscall(vk_code: u16, is_key_up: bool, is_extended: bool) -> bool {
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = (vk_code, is_key_up, is_extended);
        false
    }

    #[cfg(target_arch = "x86_64")]
    {
        let syscall_nr = match resolve_ntuser_send_input_syscall_cached() {
            Some(nr) => nr,
            None => return false,
        };

        let mut flags: u32 = 0;
        if is_key_up {
            flags |= KEYEVENTF_KEYUP;
        }
        if is_extended {
            flags |= KEYEVENTF_EXTENDEDKEY;
        }

        let input = RawKeyboardInput {
            r#type: INPUT_KEYBOARD,
            _pad1: 0,
            wVk: vk_code,
            wScan: 0,
            dwFlags: flags,
            time: 0,
            _pad2: 0,
            dwExtraInfo: random_extra_info(),
            _pad3: [0u8; 8],
        };

        let input_ptr: *const RawKeyboardInput = &input;
        let cb_size = std::mem::size_of::<RawKeyboardInput>() as i32;

        let result: i32;
        unsafe {
            asm!(
                "mov r10, rcx",
                "syscall",
                in("eax") syscall_nr,
                in("rcx") 1i32,
                in("rdx") input_ptr,
                in("r8")  cb_size,
                lateout("rax") result,
                lateout("rcx") _,
                lateout("r11") _,
            );
        }

        result == 1
    }
}

fn resolve_ntuser_send_input_syscall_cached() -> Option<u32> {
    *NTUSER_SEND_INPUT_NR.get_or_init(resolve_ntuser_send_input_syscall)
}

fn resolve_ntuser_send_input_syscall() -> Option<u32> {
    // Manually decode obfuscated DLL/function names to avoid plaintext in binary.
    // We can't use the obfstr! macro here because it's defined in this same module.
    const DLL_KEY: u8 = (b"win32u.dll".len() as u8).wrapping_mul(0xA7).wrapping_add(0x3C);
    const DLL_ENCODED: [u8; 256] = encode_bytes(b"win32u.dll", DLL_KEY);
    let dll_decoded = decode_bytes(&DLL_ENCODED[..9], DLL_KEY);
    let dll_name = obfstr_to_wide(&dll_decoded);

    const FUNC_KEY: u8 = (b"NtUserSendInput".len() as u8).wrapping_mul(0xA7).wrapping_add(0x3C);
    const FUNC_ENCODED: [u8; 256] = encode_bytes(b"NtUserSendInput", FUNC_KEY);
    let func_decoded = decode_bytes(&FUNC_ENCODED[..15], FUNC_KEY);
    let func_name = obfstr_to_ansi(&func_decoded);

    let module = unsafe { LoadLibraryW(dll_name.as_ptr()) };
    if module.is_null() {
        return None;
    }

    let proc = unsafe { GetProcAddress(module, func_name.as_ptr()) };
    if proc.is_null() {
        return None;
    }

    // Scan for the B8 opcode (mov eax, imm32) in the first 32 bytes.
    let stub = proc as *const u8;
    unsafe {
        for offset in 0..32usize {
            if *stub.add(offset) == 0xB8 {
                let nr = std::ptr::read_unaligned(stub.add(offset + 1) as *const u32);
                if nr >= 0x1000 && nr < 0x20000 {
                    return Some(nr);
                }
            }
        }
    }

    None
}

// ── String obfuscation ────────────────────────────────────────────────

/// Compile-time XOR-based string obfuscation.
///
/// Usage: `obfstr!("secret")` — decodes at runtime, returns `String`.
/// The key is derived from string length. Encoded data is stored in a
/// fixed 256-byte array (only first N bytes are meaningful).
#[macro_export]
macro_rules! obfstr {
    ($s:expr) => {{
        const INPUT: &[u8] = $s.as_bytes();
        const KEY: u8 = (INPUT.len() as u8).wrapping_mul(0xA7).wrapping_add(0x3C);
        const ENCODED: [u8; 256] = $crate::stealth::encode_bytes(INPUT, KEY);
        $crate::stealth::decode_bytes(&ENCODED[..INPUT.len()], KEY)
    }};
}

/// Const-evaluated XOR encoding.
pub const fn encode_bytes(input: &[u8], key: u8) -> [u8; 256] {
    let mut buf = [0u8; 256];
    let mut i = 0;
    while i < input.len() && i < 256 {
        buf[i] = input[i] ^ key;
        i += 1;
    }
    buf
}

/// Runtime XOR decoding — returns a heap-allocated String.
pub fn decode_bytes(encoded: &[u8], key: u8) -> String {
    let mut buf = Vec::with_capacity(encoded.len());
    for &byte in encoded {
        buf.push(byte ^ key);
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// Convert a decoded obfstr String to a wide (UTF-16, NUL-terminated) Vec<u16>
/// for use with Win32 wide-string APIs.
fn obfstr_to_wide(s: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    std::ffi::OsStr::new(s).encode_wide().chain(Some(0)).collect()
}

/// Convert a decoded obfstr String to an ANSI (NUL-terminated) Vec<u8>
/// for use with GetProcAddress etc.
fn obfstr_to_ansi(s: &str) -> Vec<u8> {
    s.bytes().chain(Some(0)).collect()
}

// ── Random thread name generation ─────────────────────────────────────

/// Generate a random thread name that looks like a legitimate system thread.
/// Uses obfuscated prefix/suffix to avoid plaintext patterns in the binary.
pub fn random_thread_name() -> String {
    let prefix = match fastrand::usize(..17) {
        0 => obfstr!("ntdll"),
        1 => obfstr!("wer"),
        2 => obfstr!("clr"),
        3 => obfstr!("mswsock"),
        4 => obfstr!("wmi"),
        5 => obfstr!("winhttp"),
        6 => obfstr!("dnsapi"),
        7 => obfstr!("crypt32"),
        8 => obfstr!("secur32"),
        9 => obfstr!("uxinit"),
        10 => obfstr!("dwm"),
        11 => obfstr!("audioses"),
        12 => obfstr!("conhost"),
        13 => obfstr!("taskhostw"),
        14 => obfstr!("sihost"),
        15 => obfstr!("ctfmon"),
        _ => obfstr!("RuntimeBroker"),
    };
    let suffix = match fastrand::usize(..10) {
        0 => obfstr!("Worker"),
        1 => obfstr!("Callback"),
        2 => obfstr!("Dispatch"),
        3 => obfstr!("Timer"),
        4 => obfstr!("Completion"),
        5 => obfstr!("IoCompletion"),
        6 => obfstr!("Wait"),
        7 => obfstr!("Pool"),
        8 => obfstr!("Init"),
        _ => obfstr!("Shutdown"),
    };
    let id = fastrand::u32(..);
    format!("{prefix}{suffix}_{id:x}")
}

// ── Anti-debug helpers ────────────────────────────────────────────────

fn is_debugger_present_check() -> bool {
    #[link(name = "kernel32")]
    extern "system" {
        fn IsDebuggerPresent() -> i32;
    }
    unsafe { IsDebuggerPresent() != 0 }
}

fn is_remote_debugger_present_check() -> bool {
    #[link(name = "kernel32")]
    extern "system" {
        fn CheckRemoteDebuggerPresent(hProcess: isize, pbDebuggerPresent: *mut i32) -> i32;
    }
    let mut present = 0i32;
    unsafe {
        CheckRemoteDebuggerPresent(-1isize, &mut present);
    }
    present != 0
}

fn analysis_tool_detected_check() -> bool {
    // Use obfstr! for tool DLL names to avoid plaintext in binary
    let tools: Vec<String> = [
        obfstr!("sbiedll"), obfstr!("dbghelp"), obfstr!("api_log"),
        obfstr!("dir_watch"), obfstr!("pstorec"), obfstr!("vmcheck"),
        obfstr!("wpespy"),
    ].into_iter().collect();

    for tool in &tools {
        let name_wide: Vec<u16> = tool.encode_utf16().chain(Some(0)).collect();
        #[link(name = "kernel32")]
        extern "system" {
            fn GetModuleHandleW(lpModuleName: *const u16) -> isize;
        }
        unsafe {
            if GetModuleHandleW(name_wide.as_ptr()) != 0 {
                return true;
            }
        }
    }
    false
}

// ── Memory protection ─────────────────────────────────────────────────

/// Erase a sensitive buffer from memory by zeroing it.
pub fn secure_zero(buf: &mut [u8]) {
    for byte in buf.iter_mut() {
        unsafe {
            std::ptr::write_volatile(byte, 0);
        }
    }
    std::sync::atomic::fence(std::sync::atomic::Ordering::SeqCst);
}

// ── Initialization ────────────────────────────────────────────────────

/// One-time anti-detection initialization.
pub fn init() {
    // Seed the RNG for stealth operations
    fastrand::seed(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0),
    );

    // Pre-resolve the syscall number
    let _ = resolve_ntuser_send_input_syscall_cached();

    // Active anti-debug: set flags if debugger/analysis tools detected
    if is_debugger_present_check() || is_remote_debugger_present_check() {
        DEBUGGER_DETECTED.store(true, std::sync::atomic::Ordering::Release);
    }

    if analysis_tool_detected_check() {
        ANALYSIS_DETECTED.store(true, std::sync::atomic::Ordering::Release);
    }
}

static DEBUGGER_DETECTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static ANALYSIS_DETECTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Returns true if a debugger was detected at startup.
pub fn is_debugger_detected() -> bool {
    DEBUGGER_DETECTED.load(std::sync::atomic::Ordering::Acquire)
}

/// Returns true if analysis tools were detected at startup.
pub fn is_analysis_detected() -> bool {
    ANALYSIS_DETECTED.load(std::sync::atomic::Ordering::Acquire)
}

// ── FFI helpers ───────────────────────────────────────────────────────

#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryW(lpFileName: *const u16) -> *mut c_void;
    fn GetProcAddress(hModule: *mut c_void, lpProcName: *const u8) -> *mut c_void;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extra_info_in_qpc_range() {
        for _ in 0..100 {
            let v = random_extra_info();
            assert!(v >= 100_000_000, "too small: {v}");
            assert!(v < 5_000_000_001, "too large: {v}");
        }
    }

    #[test]
    fn lparam_preserves_key_bits() {
        let bits: u32 = 1 | (0x1E << 16);
        let result = randomize_lparam(bits, false) as u32;
        assert_eq!((result >> 16) & 0xFF, 0x1E);
    }

    #[test]
    fn lparam_keyup_bits_preserved() {
        let bits: u32 = 1 | (0x1E << 16) | (1 << 30) | (1 << 31);
        let result = randomize_lparam(bits, true) as u32;
        assert_eq!(result & (1 << 30), 1 << 30);
        assert_eq!(result & (1 << 31), 1 << 31);
        assert_eq!((result >> 16) & 0xFF, 0x1E);
    }

    #[test]
    fn lparam_never_sets_bit29() {
        for _ in 0..1000 {
            let bits: u32 = 1 | (0x1E << 16);
            let result = randomize_lparam(bits, false) as u32;
            assert_eq!(result & (1 << 29), 0, "bit 29 was set — triggers Alt+key shortcuts!");
        }
    }

    #[test]
    fn obfstr_roundtrip() {
        let decoded = obfstr!("hello world");
        assert_eq!(decoded, "hello world");
    }

    #[test]
    fn secure_zero_clears_buffer() {
        let mut buf = vec![0xABu8; 32];
        secure_zero(&mut buf);
        assert!(buf.iter().all(|&b| b == 0));
    }

    #[test]
    fn random_thread_name_is_plausible() {
        let name = random_thread_name();
        assert!(!name.is_empty());
        assert!(name.len() > 5);
    }
}
