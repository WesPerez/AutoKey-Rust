//! Anti-detection primitives for input injection.
//!
//! - dwExtraInfo randomization (simulates QPC timestamp range)
//! - PostMessage lParam randomization (reserved bits + occasional repeat count)
//! - Compile-time string obfuscation
//! - Anti-debug / anti-analysis helpers (active in init)
//! - Memory protection (secure zeroing)

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
    // Only check for tools that are NOT commonly loaded by normal Windows.
    // "dbghelp" is excluded because it's loaded by many normal processes.
    let tools: Vec<String> = [
        obfstr!("sbiedll"),      // Sandboxie
        obfstr!("api_log"),      // API Monitor
        obfstr!("dir_watch"),    // Directory watcher
        obfstr!("pstorec"),      // Password store
        obfstr!("vmcheck"),      // VM check
        obfstr!("wpespy"),       // WPE Pro
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
#[allow(dead_code)]
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
#[allow(dead_code)]
pub fn is_debugger_detected() -> bool {
    DEBUGGER_DETECTED.load(std::sync::atomic::Ordering::Acquire)
}

/// Returns true if analysis tools were detected at startup.
#[allow(dead_code)]
pub fn is_analysis_detected() -> bool {
    ANALYSIS_DETECTED.load(std::sync::atomic::Ordering::Acquire)
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
