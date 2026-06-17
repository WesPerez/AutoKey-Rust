use crate::hotkey::{is_modifier, normalize_vk, VK_ALT, VK_CONTROL};
use crate::{window, AppCommand, UiAction};
use anyhow::{anyhow, Result};
use once_cell::sync::Lazy;
use parking_lot::{Mutex, RwLock};
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_LMENU, VK_MENU, INPUT, INPUT_0,
    INPUT_KEYBOARD, KEYBDINPUT, SendInput,
};
use windows::Win32::UI::WindowsAndMessaging::*;

const RIGHT_DRAG_THRESHOLD_SQUARED: i64 = 64;

type SharedBoundWindow = Arc<RwLock<Option<isize>>>;

static COMMAND_SENDER: Lazy<Mutex<Option<Sender<AppCommand>>>> = Lazy::new(|| Mutex::new(None));
static UI_SENDER: Lazy<Mutex<Option<Sender<UiAction>>>> = Lazy::new(|| Mutex::new(None));
static BOUND_WINDOW: Lazy<Mutex<Option<SharedBoundWindow>>> = Lazy::new(|| Mutex::new(None));
static STATUS: Lazy<Mutex<Option<Arc<RwLock<String>>>>> = Lazy::new(|| Mutex::new(None));
static PRESSED_KEYS: Lazy<Mutex<BTreeSet<u16>>> = Lazy::new(|| Mutex::new(BTreeSet::new()));
static SUPPRESSED_KEYS: Lazy<Mutex<BTreeSet<u16>>> = Lazy::new(|| Mutex::new(BTreeSet::new()));
static HOTKEY_FIRED: AtomicBool = AtomicBool::new(false);
static LEFT_ALT_DOWN: AtomicBool = AtomicBool::new(false);
static LEFT_ALT_SOLO: AtomicBool = AtomicBool::new(false);
static RIGHT_DRAGGING: AtomicBool = AtomicBool::new(false);
static RIGHT_DRAG_START: Lazy<Mutex<(i32, i32)>> = Lazy::new(|| Mutex::new((0, 0)));
static ALT_SYNTHETIC_DOWN: AtomicBool = AtomicBool::new(false);
static CAPTURE_MODE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

pub struct GlobalHooks {
    thread_id: u32,
    worker: Option<JoinHandle<()>>,
}

impl GlobalHooks {
    pub fn install(
        command_sender: Sender<AppCommand>,
        ui_sender: Sender<UiAction>,
        bound_window: Arc<RwLock<Option<isize>>>,
        status: Arc<RwLock<String>>,
    ) -> Result<Self> {
        *COMMAND_SENDER.lock() = Some(command_sender);
        *UI_SENDER.lock() = Some(ui_sender);
        *BOUND_WINDOW.lock() = Some(bound_window);
        *STATUS.lock() = Some(status);

        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name(crate::stealth::random_thread_name())
            .spawn(move || {
                let result = install_hooks();
                let _ = ready_tx.send(
                    result
                        .as_ref()
                        .map(|(thread_id, _, _)| *thread_id)
                        .map_err(|error| error.to_string()),
                );

                if let Ok((_, keyboard, mouse)) = result {
                    // SAFETY: Both hooks and the message queue belong to this thread.
                    unsafe {
                        let mut message = MSG::default();
                        loop {
                            let result = GetMessageW(&mut message, None, 0, 0).0;
                            if result <= 0 {
                                break;
                            }
                            let _ = TranslateMessage(&message);
                            DispatchMessageW(&message);
                        }
                        let _ = UnhookWindowsHookEx(mouse);
                        let _ = UnhookWindowsHookEx(keyboard);
                    }
                }
            })?;

        match ready_rx.recv() {
            Ok(Ok(thread_id)) => Ok(Self {
                thread_id,
                worker: Some(worker),
            }),
            Ok(Err(error)) => {
                let _ = worker.join();
                clear_globals();
                Err(anyhow!(error))
            }
            Err(error) => {
                let _ = worker.join();
                clear_globals();
                Err(error.into())
            }
        }
    }

    pub fn begin_key_capture() {
        PRESSED_KEYS.lock().clear();
        SUPPRESSED_KEYS.lock().clear();
        CAPTURE_MODE.store(1, Ordering::Release);
    }

    pub fn cancel_capture() {
        CAPTURE_MODE.store(0, Ordering::Release);
        PRESSED_KEYS.lock().clear();
    }
}

impl Drop for GlobalHooks {
    fn drop(&mut self) {
        // SAFETY: thread_id belongs to the live hook thread after its queue was created.
        unsafe {
            let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        clear_globals();
    }
}

fn clear_globals() {
    *COMMAND_SENDER.lock() = None;
    *UI_SENDER.lock() = None;
    *BOUND_WINDOW.lock() = None;
    *STATUS.lock() = None;
    PRESSED_KEYS.lock().clear();
    SUPPRESSED_KEYS.lock().clear();
    HOTKEY_FIRED.store(false, Ordering::Relaxed);
    LEFT_ALT_DOWN.store(false, Ordering::Relaxed);
    LEFT_ALT_SOLO.store(false, Ordering::Relaxed);
    ALT_SYNTHETIC_DOWN.store(false, Ordering::Relaxed);
    RIGHT_DRAGGING.store(false, Ordering::Relaxed);
    CAPTURE_MODE.store(0, Ordering::Relaxed);
}

fn install_hooks() -> Result<(u32, HHOOK, HHOOK)> {
    // SAFETY: Both callbacks have the system ABI and remain valid for the hook lifetime.
    unsafe {
        let thread_id = GetCurrentThreadId();
        let mut message = MSG::default();
        let _ = PeekMessageW(&mut message, None, 0, 0, PM_NOREMOVE);
        let module = GetModuleHandleW(None)?;
        let keyboard = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), module, 0)?;
        match SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), module, 0) {
            Ok(mouse) => Ok((thread_id, keyboard, mouse)),
            Err(error) => {
                let _ = UnhookWindowsHookEx(keyboard);
                Err(error.into())
            }
        }
    }
}

unsafe extern "system" fn keyboard_proc(code: i32, w_param: WPARAM, l_param: LPARAM) -> LRESULT {
    if code >= 0 && l_param.0 != 0 {
        let event = &*(l_param.0 as *const KBDLLHOOKSTRUCT);
        if event.flags.0 & LLKHF_INJECTED.0 == 0 && handle_keyboard_event(event, w_param.0 as u32) {
            return LRESULT(1);
        }
    }
    CallNextHookEx(None, code, w_param, l_param)
}

fn handle_keyboard_event(event: &KBDLLHOOKSTRUCT, message: u32) -> bool {
    let is_key_down = message == WM_KEYDOWN || message == WM_SYSKEYDOWN;
    let is_key_up = message == WM_KEYUP || message == WM_SYSKEYUP;
    let raw_vk = event.vkCode as u16;
    let vk = normalize_vk(raw_vk);
    let was_suppressed = is_key_up && SUPPRESSED_KEYS.lock().remove(&vk);
    let is_left_alt = is_physical_left_alt(event);
    let mut suppress_current_event = false;

    if is_key_down {
        let inserted = PRESSED_KEYS.lock().insert(vk);

        // Left-Alt-as-toggle state machine.
        //
        // `LEFT_ALT_SOLO` means "Alt was pressed on its own and no other key has
        // been pressed since". Releasing it in that state toggles running.
        //
        // The previous implementation used a 300ms cooldown after each toggle
        // which silently swallowed every Alt press during that window — that is
        // exactly why rapid Alt presses stopped working (pressing Alt twice
        // within 300ms dropped the second press entirely and left the state
        // machine out of sync). The cooldown is removed; the edge-triggered
        // solo logic below is sufficient and self-correcting: every clean
        // press→release of solo Alt fires exactly one toggle.
        if is_left_alt {
            if !LEFT_ALT_DOWN.swap(true, Ordering::Relaxed) {
                // Solo is only possible if no other key is currently held.
                let others_held = PRESSED_KEYS.lock().iter().any(|&k| k != VK_ALT);
                LEFT_ALT_SOLO.store(!others_held, Ordering::Relaxed);
                // Suppress the Alt key-down when it's potentially solo and the
                // foreground window is NOT our own app. When our own window is
                // focused, let the Alt key through so the app's UI stays
                // responsive (the toggle still fires via the hook on key-up).
                if LEFT_ALT_SOLO.load(Ordering::Relaxed) && !is_own_window_focused() {
                    SUPPRESSED_KEYS.lock().insert(vk);
                }
            }
        } else if inserted && LEFT_ALT_DOWN.load(Ordering::Relaxed) {
            // Any other key while Alt is held cancels the solo gesture.
            LEFT_ALT_SOLO.store(false, Ordering::Relaxed);
            // If the Alt key-down was suppressed as a potential solo gesture,
            // replay it exactly once so the foreground app sees the Alt+Key
            // combination. Remove from SUPPRESSED_KEYS after injection to
            // prevent duplicate injections on subsequent keys.
            if SUPPRESSED_KEYS.lock().remove(&VK_ALT) {
                ALT_SYNTHETIC_DOWN.store(true, Ordering::Relaxed);
                unsafe {
                    let input = INPUT {
                        r#type: INPUT_KEYBOARD,
                        Anonymous: INPUT_0 {
                            ki: KEYBDINPUT {
                                wVk: VK_MENU,
                                wScan: 0,
                                dwFlags: KEYBD_EVENT_FLAGS(0),
                                time: 0,
                                dwExtraInfo: 0,
                            },
                        },
                    };
                    let _ = SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
                }
            }
        }

        let capture_mode = CAPTURE_MODE.load(Ordering::Acquire);
        if inserted && capture_mode == 1 {
            SUPPRESSED_KEYS.lock().insert(vk);
            LEFT_ALT_SOLO.store(false, Ordering::Relaxed);
            if raw_vk == 0x1B {
                CAPTURE_MODE.store(0, Ordering::Release);
                send_ui_action(UiAction::CapturedKey(0));
            } else if !crate::hotkey::is_modifier(vk) {
                CAPTURE_MODE.store(0, Ordering::Release);
                send_ui_action(UiAction::CapturedKey(vk));
            }
            return true;
        }

        if inserted && !HOTKEY_FIRED.load(Ordering::Relaxed) {
            let pressed = PRESSED_KEYS
                .lock()
                .iter()
                .copied()
                .map(normalize_vk)
                .collect();
            if handle_registered_hotkey(&pressed) {
                HOTKEY_FIRED.store(true, Ordering::Relaxed);
                LEFT_ALT_SOLO.store(false, Ordering::Relaxed);
                let mut suppressed = SUPPRESSED_KEYS.lock();
                for &vk in PRESSED_KEYS.lock().iter() {
                    suppressed.insert(vk);
                }
            }
        }
    } else if is_key_up {
        PRESSED_KEYS.lock().remove(&vk);
        if !is_modifier(vk) || PRESSED_KEYS.lock().is_empty() {
            HOTKEY_FIRED.store(false, Ordering::Relaxed);
        }

        // When a suppressed modifier key (Ctrl, Shift, Win — but not Alt) is
        // released, the key-down already passed through to the system but the
        // key-up is suppressed here. Inject a synthetic key-up so the system
        // releases the modifier state and the key doesn't get "stuck" (requiring
        // a manual press to clear). Alt is handled separately below with its own
        // synthetic injection logic.
        if was_suppressed && is_modifier(vk) && !is_left_alt {
            unsafe {
                let input = INPUT {
                    r#type: INPUT_KEYBOARD,
                    Anonymous: INPUT_0 {
                        ki: KEYBDINPUT {
                            wVk: VIRTUAL_KEY(vk),
                            wScan: 0,
                            dwFlags: KEYEVENTF_KEYUP,
                            time: 0,
                            dwExtraInfo: 0,
                        },
                    },
                };
                let _ = SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
            }
        }

        if is_left_alt {
            LEFT_ALT_DOWN.store(false, Ordering::Relaxed);
            let was_solo = LEFT_ALT_SOLO.swap(false, Ordering::Relaxed);
            if was_solo {
                // When our own window is the foreground, egui handles the Alt
                // toggle directly (see gui.rs update()). Skipping the hook-based
                // toggle here avoids double-toggling.
                if !is_own_window_focused() {
                    if let Some(sender) = COMMAND_SENDER.try_lock().and_then(|guard| guard.clone()) {
                        let _ = sender.send(AppCommand::ToggleRunning);
                    }
                }
            }
            // When our own window is focused, let the Alt key-up pass through
            // normally — no suppression and no synthetic injection. The toggle
            // already fired above, and the app's own UI receives the key event
            // as usual.
            //
            // Exception: if the key-down was suppressed (started in a different
            // window) or a synthetic key-down was injected, we still need to
            // inject a synthetic key-up to release the system's Alt state.
            let needs_synthetic_up = was_suppressed
                || ALT_SYNTHETIC_DOWN.swap(false, Ordering::Relaxed)
                || (!is_own_window_focused() && was_solo);
            if needs_synthetic_up {
                unsafe {
                    let input = INPUT {
                        r#type: INPUT_KEYBOARD,
                        Anonymous: INPUT_0 {
                            ki: KEYBDINPUT {
                                wVk: VK_MENU,
                                wScan: 0,
                                dwFlags: KEYEVENTF_KEYUP,
                                time: 0,
                                dwExtraInfo: 0,
                            },
                        },
                    };
                    let _ = SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
                }
                suppress_current_event = true;
            }
        }
    }

    was_suppressed
        || CAPTURE_MODE.load(Ordering::Acquire) != 0
        || SUPPRESSED_KEYS.lock().contains(&vk)
        || suppress_current_event
}

/// Identify a physical Left Alt key event.
///
/// Prefers the dedicated `VK_LMENU` (0xA4). Falls back to `VK_MENU` (0x12)
/// only when the event is not an extended key (extended flag ⇒ Right Alt) and
/// the scan code matches the Alt scan code 0x38. The scan-code check is a
/// secondary signal; the extended flag alone is enough to reject Right Alt,
/// which is what matters for correctness here.
fn is_physical_left_alt(event: &KBDLLHOOKSTRUCT) -> bool {
    let vk = event.vkCode as u16;
    if vk == VK_LMENU.0 {
        return true;
    }
    if vk == VK_MENU.0 {
        return event.flags.0 & LLKHF_EXTENDED.0 == 0;
    }
    false
}

/// Check whether the foreground window is our own main window.
/// When true, we skip Alt key suppression so the app's own UI can handle
/// keyboard events normally (e.g. the start/stop button still works).
fn is_own_window_focused() -> bool {
    let Some(own_hwnd) = window::find_own_hwnd() else {
        return false;
    };
    let fg = unsafe { GetForegroundWindow() };
    !fg.is_invalid() && fg.0 as isize == own_hwnd
}

fn handle_registered_hotkey(pressed: &BTreeSet<u16>) -> bool {
    let bind_window = BTreeSet::from([VK_CONTROL, VK_ALT, 0x20]);
    if pressed == &bind_window {
        bind_window_under_cursor();
        return true;
    }

    false
}

unsafe extern "system" fn mouse_proc(code: i32, w_param: WPARAM, l_param: LPARAM) -> LRESULT {
    if code >= 0 && l_param.0 != 0 {
        let event = &*(l_param.0 as *const MSLLHOOKSTRUCT);
        match w_param.0 as u32 {
            WM_RBUTTONDOWN => {
                RIGHT_DRAGGING.store(true, Ordering::Relaxed);
                *RIGHT_DRAG_START.lock() = (event.pt.x, event.pt.y);
            }
            WM_RBUTTONUP if RIGHT_DRAGGING.swap(false, Ordering::Relaxed) => {
                let (start_x, start_y) = *RIGHT_DRAG_START.lock();
                let dx = i64::from(event.pt.x - start_x);
                let dy = i64::from(event.pt.y - start_y);
                if dx * dx + dy * dy >= RIGHT_DRAG_THRESHOLD_SQUARED {
                    bind_window_at(event.pt);
                }
            }
            _ => {}
        }
    }
    CallNextHookEx(None, code, w_param, l_param)
}

fn bind_window_under_cursor() {
    // SAFETY: GetCursorPos initializes the POINT on success.
    unsafe {
        let mut point = POINT::default();
        if GetCursorPos(&mut point).is_ok() {
            bind_window_at(point);
        } else {
            // Fallback: bind the current foreground window
            // (useful for games running with higher privileges)
            let foreground = GetForegroundWindow();
            if !foreground.0.is_null() {
                let hwnd = foreground.0 as isize;
                if !window::is_own_process_window(hwnd) {
                    if let Some(bound_window) =
                        BOUND_WINDOW.try_lock().and_then(|guard| guard.clone())
                    {
                        *bound_window.write() = Some(hwnd);
                    }
                    let title = window::get_window_title(hwnd);
                    if let Some(status) = STATUS.try_lock().and_then(|guard| guard.clone()) {
                        *status.write() = if title.is_empty() {
                            format!("已绑定窗口 {hwnd:#x}")
                        } else {
                            format!("已绑定: {title}")
                        };
                    }
                }
            }
        }
    }
}

fn bind_window_at(point: POINT) {
    // SAFETY: WindowFromPoint/GetAncestor return opaque handles that are validated below.
    unsafe {
        let child = WindowFromPoint(point);
        let desktop = GetDesktopWindow();
        let shell = GetShellWindow();

        let target: HWND = if child.0.is_null() || child == desktop || child == shell {
            // Fallback: use the foreground window if WindowFromPoint returns
            // null or the desktop/shell window (happens with elevated games like DNF/MapleStory)
            GetForegroundWindow()
        } else {
            let root = GetAncestor(child, GA_ROOT);
            if root.0.is_null() || root == desktop || root == shell {
                GetForegroundWindow()
            } else {
                root
            }
        };

        if target.0.is_null() || target == desktop || target == shell {
            return;
        }

        let hwnd = target.0 as isize;
        if window::is_own_process_window(hwnd) {
            return;
        }

        if let Some(bound_window) = BOUND_WINDOW.try_lock().and_then(|guard| guard.clone()) {
            *bound_window.write() = Some(hwnd);
        }
        let title = window::get_window_title(hwnd);
        if let Some(status) = STATUS.try_lock().and_then(|guard| guard.clone()) {
            *status.write() = if title.is_empty() {
                format!("已绑定窗口 {hwnd:#x}")
            } else {
                format!("已绑定: {title}")
            };
        }
    }
}

fn send_ui_action(action: UiAction) {
    if let Some(sender) = UI_SENDER.try_lock().and_then(|guard| guard.clone()) {
        let _ = sender.send(action);
    }
}
