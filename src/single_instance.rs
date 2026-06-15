use anyhow::Result;
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use windows::core::PCWSTR;
use windows::Win32::Foundation::*;
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, SetEvent, WaitForSingleObject,
};

// Obfuscated identifiers — decoded at runtime to avoid static string scanning.
// These replace the previous hardcoded GUIDs which were trivially searchable.

fn mutex_name() -> Vec<u16> {
    to_wide(&crate::obfstr!("Local\\{B3F7A2E1-9C4D-4A8B-B5E6-F1D2C3A4B5E6}"))
}

fn event_name() -> Vec<u16> {
    to_wide(&crate::obfstr!("Local\\{D4E8F3A2-1B5C-4D7E-A9F0-E1D2C3B4A5F6}"))
}

pub struct SingleInstance {
    mutex_handle: HANDLE,
    activation_event: HANDLE,
}

impl SingleInstance {
    pub fn try_acquire() -> Result<Option<Self>> {
        // SAFETY: Names are NUL-terminated and every returned handle is closed by its owner.
        unsafe {
            let event_name_wide = event_name();
            let activation_event = CreateEventW(None, false, false, PCWSTR(event_name_wide.as_ptr()))?;
            let mutex_name_wide = mutex_name();
            let mutex_handle = match CreateMutexW(None, false, PCWSTR(mutex_name_wide.as_ptr())) {
                Ok(handle) => handle,
                Err(error) => {
                    let _ = CloseHandle(activation_event);
                    return Err(error.into());
                }
            };
            if GetLastError() == ERROR_ALREADY_EXISTS {
                let _ = SetEvent(activation_event);
                let _ = CloseHandle(activation_event);
                let _ = CloseHandle(mutex_handle);
                return Ok(None);
            }

            Ok(Some(Self {
                mutex_handle,
                activation_event,
            }))
        }
    }

    pub fn activation_handle(&self) -> isize {
        self.activation_event.0 as isize
    }

    pub fn wait_for_activation(handle: isize, timeout_ms: u32) -> bool {
        let handle = HANDLE(handle as *mut _);
        // SAFETY: The handle is owned by the live first instance while callers wait on it.
        unsafe { WaitForSingleObject(handle, timeout_ms) == WAIT_OBJECT_0 }
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        // SAFETY: Both handles are owned by this value and closed exactly once.
        unsafe {
            let _ = CloseHandle(self.activation_event);
            let _ = CloseHandle(self.mutex_handle);
        }
    }
}

fn to_wide(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(Some(0)).collect()
}
