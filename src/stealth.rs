//! Anti-detection primitives for input injection.
//!
//! - dwExtraInfo randomization (simulates QPC timestamp range)
//! - PostMessage lParam randomization (reserved bits + occasional repeat count)
//! - Compile-time string obfuscation

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

const REPEAT_COUNT_MASK: u32 = 0x0000_FFFF;
const RESERVED_BITS_MASK: u32 = 0x0F << 25;
const KEYDOWN_REPEAT_BUMP_RATE: f64 = 0.03;

/// Randomize low-risk metadata of a background `WM_KEYDOWN` lParam.
///
/// Key-down messages get reserved-bit noise and occasionally use repeat count
/// 2. Key-up messages stay standard so release semantics remain predictable.
/// We do NOT touch scan code, extended-key, Alt/context, or key state bits.
#[inline]
pub fn randomize_lparam(bits: u32, is_key_up: bool) -> isize {
    if is_key_up {
        return bits as isize;
    }

    let mut lparam = bits;

    let reserved_noise = (fastrand::u32(..) & 0x0F) << 25;
    lparam = (lparam & !RESERVED_BITS_MASK) | reserved_noise;

    if fastrand::f64() < KEYDOWN_REPEAT_BUMP_RATE {
        lparam = (lparam & !REPEAT_COUNT_MASK) | 2;
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

#[cfg(test)]
mod tests {
    use super::*;

    const SCAN_CODE_MASK: u32 = 0x00FF_0000;
    const EXTENDED_KEY_BIT: u32 = 1 << 24;
    const ALT_CONTEXT_BIT: u32 = 1 << 29;
    const PREVIOUS_STATE_BIT: u32 = 1 << 30;
    const TRANSITION_STATE_BIT: u32 = 1 << 31;

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
        assert_eq!(result & SCAN_CODE_MASK, bits & SCAN_CODE_MASK);
        assert_eq!(result & EXTENDED_KEY_BIT, bits & EXTENDED_KEY_BIT);
    }

    #[test]
    fn lparam_keyup_bits_preserved() {
        let bits: u32 = 1 | (0x1E << 16) | PREVIOUS_STATE_BIT | TRANSITION_STATE_BIT;
        let result = randomize_lparam(bits, true) as u32;
        assert_eq!(result, bits);
    }

    #[test]
    fn lparam_never_sets_bit29() {
        for _ in 0..1000 {
            let bits: u32 = 1 | (0x1E << 16);
            let result = randomize_lparam(bits, false) as u32;
            assert_eq!(
                result & ALT_CONTEXT_BIT,
                0,
                "bit 29 was set — triggers Alt+key shortcuts!"
            );
        }
    }

    #[test]
    fn lparam_keydown_repeat_count_is_one_or_two() {
        for _ in 0..1000 {
            let bits: u32 = 1 | (0x1E << 16);
            let result = randomize_lparam(bits, false) as u32;
            assert!(matches!(result & REPEAT_COUNT_MASK, 1 | 2));
        }
    }

    #[test]
    fn lparam_keydown_keeps_semantic_bits() {
        let bits: u32 = 1 | (0xE0 << 16) | EXTENDED_KEY_BIT;
        let semantic_mask = SCAN_CODE_MASK
            | EXTENDED_KEY_BIT
            | ALT_CONTEXT_BIT
            | PREVIOUS_STATE_BIT
            | TRANSITION_STATE_BIT;
        for _ in 0..1000 {
            let result = randomize_lparam(bits, false) as u32;
            assert_eq!(result & semantic_mask, bits & semantic_mask);
        }
    }

    #[test]
    fn obfstr_roundtrip() {
        let decoded = obfstr!("hello world");
        assert_eq!(decoded, "hello world");
    }

    #[test]
    fn random_thread_name_is_plausible() {
        let name = random_thread_name();
        assert!(!name.is_empty());
        assert!(name.len() > 5);
    }
}
