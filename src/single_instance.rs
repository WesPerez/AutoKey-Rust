use anyhow::Result;
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use windows::core::PCWSTR;
use windows::Win32::Foundation::*;
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, SetEvent, WaitForSingleObject,
};

const MUTEX_NAME: &str = "Local\\AutoKeyRust.SingleInstance.v2";
const ACTIVATE_EVENT_NAME: &str = "Local\\AutoKeyRust.Activate.v2";

pub struct SingleInstance {
    mutex_handle: HANDLE,
    activation_event: HANDLE,
}

impl SingleInstance {
    pub fn try_acquire() -> Result<Option<Self>> {
        // SAFETY: Names are NUL-terminated and every returned handle is closed by its owner.
        unsafe {
            let event_name = to_wide(ACTIVATE_EVENT_NAME);
            let activation_event = CreateEventW(None, true, false, PCWSTR(event_name.as_ptr()))?;
            let mutex_name = to_wide(MUTEX_NAME);
            let mutex_handle = match CreateMutexW(None, false, PCWSTR(mutex_name.as_ptr())) {
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
