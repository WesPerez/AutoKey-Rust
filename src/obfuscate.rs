//! Compile-time string obfuscation and memory protection.
//!
//! Prevents simple string scanning of the binary by XOR-encrypting
//! string literals at compile time and decrypting them at runtime.
//! Also provides secure memory zeroing and page locking.

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

/// Securely zero a byte buffer.
/// Uses volatile writes to prevent the compiler from optimizing away the zeroing.
#[inline]
pub fn secure_zero(data: &mut [u8]) {
    for byte in data.iter_mut() {
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
    std::sync::atomic::fence(std::sync::atomic::Ordering::SeqCst);
}

/// A string that decrypts on first access and can be securely zeroed.
pub struct SecureString {
    data: Option<Vec<u8>>,
    encrypted: Vec<u8>,
    key: u8,
}

impl SecureString {
    pub fn new(s: &str) -> Self {
        let key = ((s.len() as u64).wrapping_mul(0xA5).wrapping_add(0x37) & 0xFF) as u8;
        let encrypted: Vec<u8> = s.bytes().map(|b| b ^ key).collect();
        Self {
            data: None,
            encrypted,
            key,
        }
    }

    pub fn get(&mut self) -> &str {
        if self.data.is_none() {
            let decrypted: Vec<u8> = self.encrypted.iter().map(|b| b ^ self.key).collect();
            self.data = Some(decrypted);
        }
        unsafe { std::str::from_utf8_unchecked(self.data.as_ref().unwrap()) }
    }

    /// Zero the decrypted data from memory.
    pub fn zero(&mut self) {
        if let Some(ref mut data) = self.data {
            secure_zero(data);
        }
        self.data = None;
    }
}

impl Drop for SecureString {
    fn drop(&mut self) {
        self.zero();
    }
}

/// Lock memory to prevent paging to disk (Windows VirtualLock).
pub fn lock_memory(ptr: *const u8, len: usize) -> bool {
    use windows::Win32::System::Memory::VirtualLock;
    unsafe { VirtualLock(ptr as *const _, len).is_ok() }
}

/// Unlock previously locked memory (Windows VirtualUnlock).
pub fn unlock_memory(ptr: *const u8, len: usize) -> bool {
    use windows::Win32::System::Memory::VirtualUnlock;
    unsafe { VirtualUnlock(ptr as *const _, len).is_ok() }
}

/// Generate a random thread name that doesn't reveal the application identity.
pub fn random_thread_name() -> String {
    let pool = [
        "worker", "scheduler", "listener", "handler", "monitor",
        "dispatcher", "processor", "runner", "watcher", "service",
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
    fn secure_string_roundtrip() {
        let mut s = SecureString::new("test secret");
        assert_eq!(s.get(), "test secret");
        s.zero();
        assert!(s.data.is_none());
        // After zeroing, get() re-decrypts from encrypted storage
        assert_eq!(s.get(), "test secret");
    }

    #[test]
    fn secure_zero_clears() {
        let mut buf = vec![0xAAu8; 32];
        secure_zero(&mut buf);
        assert!(buf.iter().all(|&b| b == 0));
    }

    #[test]
    fn random_thread_name_varies() {
        let names: std::collections::HashSet<String> =
            (0..20).map(|_| random_thread_name()).collect();
        assert!(names.len() > 1);
    }
}
