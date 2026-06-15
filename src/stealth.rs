//! Anti-detection primitives for input injection.
//!
//! - Direct syscall to NtUserSendInput (bypasses IAT hooks on win32u.dll)
//! - dwExtraInfo randomization (avoids the sentinel value 0)
//! - PostMessage lParam randomization (fills reserved/unused bits with noise)
//! - Compile-time string obfuscation
//! - Anti-debug / anti-memory-scan helpers
//! - Memory protection (secure zeroing, anti-dump)

use std::arch::asm;
use std::ffi::c_void;

// ── dwExtraInfo randomization ─────────────────────────────────────────

/// Return a random `dwExtraInfo` value that is never 0.
///
/// Many anti-cheat/anti-macro systems flag `dwExtraInfo == 0` as injected
/// because real hardware input carries a non-zero QPC-based timestamp there.
/// We generate a random non-zero value to blend in.
#[inline]
pub fn random_extra_info() -> usize {
    let mut v = fastrand::usize(..);
    if v == 0 {
        v = fastrand::usize(1..);
    }
    v
}

// ── PostMessage lParam randomization ──────────────────────────────────

/// Randomize the unused/reserved bits of a WM_KEYDOWN / WM_KEYUP lParam.
///
/// lParam layout (32-bit):
///   Bits 0-15   : repeat count
///   Bits 16-23  : OEM scan code
///   Bit  24     : extended key flag
///   Bits 25-28  : reserved (documented as "do not use")
///   Bit  29     : context code (Alt held)
///   Bit  30     : previous key state
///   Bit  31     : transition state
///
/// We randomize bits 25-28 (reserved) and occasionally set bit 29
/// to make patterns less deterministic.
#[inline]
pub fn randomize_lparam(bits: u32, is_key_up: bool) -> isize {
    let mut lparam = bits;

    // Randomize reserved bits 25-28 with noise
    let reserved_noise = (fastrand::u32(..) & 0xF) << 25;
    lparam = (lparam & !(0xF << 25)) | reserved_noise;

    // Occasionally set context-code bit (bit 29) to simulate Alt being held
    // This happens ~2% of the time for keydown events
    if !is_key_up && fastrand::f64() < 0.02 {
        lparam |= 1 << 29;
    }

    lparam as isize
}

// ── Direct syscall: NtUserSendInput ───────────────────────────────────
//
// Bypasses the IAT entry for SendInput in win32u.dll.
// We resolve the syscall number at runtime from win32u.dll's export stub.

/// Raw INPUT structure matching the Win32 layout for keyboard input.
///
/// On x64, INPUT is 40 bytes:
///   type       : u32          (offset 0)
///   padding    : u32          (offset 4)
///   union      : [u8; 32]    (offset 8) — largest of MOUSEINPUT/KEYBDINPUT/HARDWAREINPUT
///
/// KEYBDINPUT inside the union:
///   wVk        : u16          (offset 8)
///   wScan      : u16          (offset 10)
///   dwFlags    : u32          (offset 12)
///   time       : u32          (offset 16)
///   padding    : u32          (offset 20)
///   dwExtraInfo: usize        (offset 24)
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
    _pad3: [u8; 8], // Padding to match full INPUT size (40 bytes on x64)
}

const INPUT_KEYBOARD: u32 = 1;
const KEYEVENTF_KEYUP: u32 = 0x0002;
const KEYEVENTF_EXTENDEDKEY: u32 = 0x0001;

/// Cached syscall number for NtUserSendInput.
/// Resolved once on first call.
static mut NTUSER_SEND_INPUT_NR: Option<u32> = None;
static NTUSER_RESOLVED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Send a keyboard event via direct syscall to NtUserSendInput.
///
/// Returns `false` if the syscall number cannot be resolved (caller
/// should fall back to the standard SendInput path).
pub fn send_input_syscall(vk_code: u16, is_key_up: bool, is_extended: bool) -> bool {
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

fn resolve_ntuser_send_input_syscall_cached() -> Option<u32> {
    // Fast path: already resolved
    if NTUSER_RESOLVED.load(std::sync::atomic::Ordering::Acquire) {
        // SAFETY: only written once before the flag is set
        unsafe { return NTUSER_SEND_INPUT_NR }
    }

    let nr = resolve_ntuser_send_input_syscall();
    // SAFETY: only one thread will win the CAS, others will see the flag
    unsafe { NTUSER_SEND_INPUT_NR = nr }
    NTUSER_RESOLVED.store(true, std::sync::atomic::Ordering::Release);
    nr
}

/// Resolve the syscall number for NtUserSendInput by scanning win32u.dll's
/// export stub for the `mov eax, <syscall_nr>` pattern.
fn resolve_ntuser_send_input_syscall() -> Option<u32> {
    let module = unsafe { LoadLibraryW(win32u_name()) };
    if module.is_null() {
        return None;
    }

    let proc = unsafe { GetProcAddress(module, ntuser_send_input_name()) };
    if proc.is_null() {
        return None;
    }

    // Scan for the B8 opcode (mov eax, imm32) in the first 32 bytes.
    let stub = proc as *const u8;
    unsafe {
        for offset in 0..32usize {
            if *stub.add(offset) == 0xB8 {
                let nr = std::ptr::read_unaligned(stub.add(offset + 1) as *const u32);
                // Syscall numbers for win32k are in the 0x1000+ range
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
/// Usage: `obfstr!("secret")` expands to code that XOR-decodes at runtime.
/// The key is derived from the string length to avoid a single global key.
#[macro_export]
macro_rules! obfstr {
    ($s:expr) => {{
        const INPUT: &[u8] = $s.as_bytes();
        const KEY: u8 = (INPUT.len() as u8).wrapping_mul(0xA7).wrapping_add(0x3C);
        const ENCODED: [u8; 256] = $crate::stealth::encode_bytes(INPUT, KEY);
        $crate::stealth::decode_bytes(&ENCODED, KEY)
    }};
}

/// Const-evaluated XOR encoding for byte slices.
pub const fn encode_bytes(input: &[u8], key: u8) -> [u8; 256] {
    let mut buf = [0u8; 256];
    let mut i = 0;
    while i < input.len() {
        buf[i] = input[i] ^ key;
        i += 1;
    }
    buf[255] = input.len() as u8;
    buf
}

/// Runtime XOR decoding — returns a heap-allocated String.
pub fn decode_bytes(encoded: &[u8; 256], key: u8) -> String {
    let len = encoded[255] as usize;
    let mut buf = Vec::with_capacity(len);
    for i in 0..len {
        buf.push(encoded[i] ^ key);
    }
    String::from_utf8_lossy(&buf).into_owned()
}

// ── Random thread name generation ─────────────────────────────────────

/// Generate a random thread name that looks like a legitimate system thread.
pub fn random_thread_name() -> String {
    const PREFIXES: &[&str] = &[
        "ntdll", "wer", "clr", "mswsock", "wmi",
        "winhttp", "dnsapi", "crypt32", "secur32", "uxinit",
        "dwm", "audioses", "conhost", "taskhostw", "sihost",
        "ctfmon", "SearchIndexer", "RuntimeBroker", "BackgroundTaskHost",
    ];
    const SUFFIXES: &[&str] = &[
        "Worker", "Callback", "Dispatch", "Timer", "Completion",
        "IoCompletion", "Wait", "Pool", "Init", "Shutdown",
    ];

    let prefix = PREFIXES[fastrand::usize(..PREFIXES.len())];
    let suffix = SUFFIXES[fastrand::usize(..SUFFIXES.len())];
    let id = fastrand::u32(..);
    format!("{prefix}{suffix}_{id:x}")
}

// ── Anti-debug helpers ────────────────────────────────────────────────

/// Check if a debugger is attached using IsDebuggerPresent.
#[allow(dead_code)]
pub fn is_debugger_present() -> bool {
    #[link(name = "kernel32")]
    extern "system" {
        fn IsDebuggerPresent() -> i32;
    }
    unsafe { IsDebuggerPresent() != 0 }
}

/// Check if a remote debugger is present using CheckRemoteDebuggerPresent.
#[allow(dead_code)]
pub fn is_remote_debugger_present() -> bool {
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

/// Combined anti-debug check.
#[allow(dead_code)]
pub fn debugger_detected() -> bool {
    is_debugger_present() || is_remote_debugger_present()
}

// ── Memory protection ─────────────────────────────────────────────────

/// Erase a sensitive buffer from memory by zeroing it.
/// Uses volatile writes to prevent the compiler from optimizing this away.
#[allow(dead_code)]
pub fn secure_zero(buf: &mut [u8]) {
    for byte in buf.iter_mut() {
        unsafe {
            std::ptr::write_volatile(byte, 0);
        }
    }
    std::sync::atomic::fence(std::sync::atomic::Ordering::SeqCst);
}

/// Check for common analysis tool modules loaded into our process.
/// Returns true if any known tool DLL is detected.
#[allow(dead_code)]
pub fn analysis_tool_detected() -> bool {
    const TOOLS: &[&str] = &[
        "sbiedll",       // Sandboxie
        "dbghelp",       // Debug helpers (often injected by debuggers)
        "api_log",       // API monitor
        "dir_watch",     // Directory watcher
        "pstorec",       // Password store
        "vmcheck",       // VM check
        "wpespy",        // WPE Pro
    ];

    for tool in TOOLS {
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

// ── Initialization ────────────────────────────────────────────────────

/// One-time anti-detection initialization.
/// Should be called early in `main` before any input is sent.
pub fn init() {
    // Seed the RNG for stealth operations
    fastrand::seed(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0),
    );

    // Pre-resolve the syscall number so we don't hit LoadLibrary on the
    // hot path during input injection.
    let _ = resolve_ntuser_send_input_syscall_cached();
}

// ── FFI helpers for win32u.dll resolution ─────────────────────────────

#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryW(lpFileName: *const u16) -> *mut c_void;
    fn GetProcAddress(hModule: *mut c_void, lpProcName: *const u8) -> *mut c_void;
}

fn win32u_name() -> *const u16 {
    static NAME: [u16; 11] = [
        b'w' as u16, b'i' as u16, b'n' as u16, b'3' as u16, b'2' as u16,
        b'u' as u16, b'.' as u16, b'd' as u16, b'l' as u16, b'l' as u16,
        0,
    ];
    NAME.as_ptr()
}

fn ntuser_send_input_name() -> *const u8 {
    static NAME: [u8; 16] = [
        b'N', b't', b'U', b's', b'e', b'r', b'S', b'e', b'n', b'd',
        b'I', b'n', b'p', b'u', b't', b'\0',
    ];
    NAME.as_ptr()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extra_info_is_never_zero() {
        for _ in 0..100 {
            assert_ne!(random_extra_info(), 0);
        }
    }

    #[test]
    fn lparam_preserves_key_bits() {
        let bits: u32 = 1 | (0x1E << 16);
        let result = randomize_lparam(bits, false) as u32;
        assert_eq!(result & 0xFFFF, 1);
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
