//! Compile-time string obfuscation and memory protection.
//!
//! Prevents simple string scanning of the binary by XOR-encrypting
//! string literals at compile time and decrypting them at runtime.
//! Thread names are randomized to avoid advertising the application identity.

/// Obfuscate a string literal at compile time.
///
/// The string is XOR-encrypted and stored as encrypted bytes in the binary.
/// At runtime, it's decrypted on the heap and returned as an owned String.
#[macro_export]
macro_rules! obfstr {
    ($s:literal) => {{
        const INPUT: &[u8] = $s.as_bytes();
        const LEN: usize = INPUT.len();
        const KEY: u8 = ((LEN as u64).wrapping_mul(0xA5).wrapping_add(0x37) & 0xFF) as u8;

        const fn xor_encrypt(data: &[u8], key: u8, len: usize) -> [u8; 256] {
            let mut buf = [0u8; 256];
            let mut i = 0;
            while i < len {
                buf[i] = data[i] ^ key;
                i += 1;
            }
            buf
        }

        const ENCRYPTED: [u8; 256] = xor_encrypt(INPUT, KEY, LEN);

        let mut decrypted = Vec::with_capacity(LEN);
        let mut i = 0;
        while i < LEN {
            decrypted.push(ENCRYPTED[i] ^ KEY);
            i += 1;
        }

        unsafe { String::from_utf8_unchecked(decrypted) }
    }};
}

/// Generate a random thread name that doesn't reveal the application identity.
pub fn random_thread_name() -> String {
    let pool = [
        "worker",
        "scheduler",
        "listener",
        "handler",
        "monitor",
        "dispatcher",
        "processor",
        "runner",
        "watcher",
        "service",
    ];
    let idx = fastrand::usize(..pool.len());
    let suffix = fastrand::u32(..1000);
    format!("{}-{}", pool[idx], suffix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn obfstr_roundtrip() {
        let s = obfstr!("AutoKey");
        assert_eq!(s, "AutoKey");

        let empty = obfstr!("");
        assert_eq!(empty, "");

        let chinese = obfstr!("按键调度器");
        assert_eq!(chinese, "按键调度器");
    }

    #[test]
    fn random_thread_name_varies() {
        let names: std::collections::HashSet<String> =
            (0..20).map(|_| random_thread_name()).collect();
        assert!(names.len() > 1);
    }
}
