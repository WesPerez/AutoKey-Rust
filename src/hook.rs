use crate::hotkey::{is_modifier, normalize_vk, Hotkey, VK_ALT, VK_CONTROL, VK_SHIFT, VK_WIN};
use crate::{window, AppCommand, UiAction};
use anyhow::{anyhow, Result};
use once_cell::sync::Lazy;
use parking_lot::{Mutex, RwLock};
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_LMENU, VK_MENU};
use windows::Win32::UI::WindowsAndMessaging::*;

const RIGHT_DRAG_THRESHOLD_SQUARED: i64 = 64;
const CYCLE_HOTKEY_ID: i32 = 1;

/// The cycle hotkey string currently registered via RegisterHotKey.
/// Updated by `update_hotkeys` and read by the hook thread.
static CYCLE_HOTKEY_STR: Lazy<Mutex<String>> = Lazy::new(|| Mutex::new(String::new()));

/// The thread ID of the hook thread, used to post wake-up messages.
static HOOK_THREAD_ID: AtomicU32 = AtomicU32::new(0);

/// Custom message to wake up the hook thread for hotkey re-registration.
const WM_REFRESH_CYCLE_HOTKEY: u32 = WM_USER + 100;

type SharedBoundWindow = Arc<RwLock<Option<isize>>>;

#[derive(Clone, Default)]
struct HookBindings {
    cycle: Option<Hotkey>,
}

static COMMAND_SENDER: Lazy<Mutex<Option<Sender<AppCommand>>>> = Lazy::new(|| Mutex::new(None));
static UI_SENDER: Lazy<Mutex<Option<Sender<UiAction>>>> = Lazy::new(|| Mutex::new(None));
static BOUND_WINDOW: Lazy<Mutex<Option<SharedBoundWindow>>> = Lazy::new(|| Mutex::new(None));
static STATUS: Lazy<Mutex<Option<Arc<RwLock<String>>>>> = Lazy::new(|| Mutex::new(None));
static BINDINGS: Lazy<RwLock<HookBindings>> = Lazy::new(|| RwLock::new(HookBindings::default()));
static PRESSED_KEYS: Lazy<Mutex<BTreeSet<u16>>> = Lazy::new(|| Mutex::new(BTreeSet::new()));
static SUPPRESSED_KEYS: Lazy<Mutex<BTreeSet<u16>>> = Lazy::new(|| Mutex::new(BTreeSet::new()));
static HOTKEY_FIRED: AtomicBool = AtomicBool::new(false);
static LEFT_ALT_DOWN: AtomicBool = AtomicBool::new(false);
static LEFT_ALT_SOLO: AtomicBool = AtomicBool::new(false);
static RIGHT_DRAGGING: AtomicBool = AtomicBool::new(false);
static RIGHT_DRAG_START: Lazy<Mutex<(i32, i32)>> = Lazy::new(|| Mutex::new((0, 0)));
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
            .name(crate::obfuscate::random_thread_name())
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
                        // Register the cycle hotkey via RegisterHotKey (more reliable than hook-based detection)
                        register_cycle_hotkey_on_thread();

                        let mut message = MSG::default();
                        loop {
                            let result = GetMessageW(&mut message, None, 0, 0).0;
                            if result <= 0 {
                                break;
                            }
                            // Handle WM_HOTKEY for the cycle config hotkey
                            if message.message == WM_HOTKEY
                                && message.wParam.0 as i32 == CYCLE_HOTKEY_ID
                            {
                                send_ui_action(UiAction::NextConfig);
                                continue;
                            }
                            // Handle request to re-register the cycle hotkey
                            if message.message == WM_REFRESH_CYCLE_HOTKEY {
                                unregister_cycle_hotkey_on_thread();
                                register_cycle_hotkey_on_thread();
                                continue;
                            }
                            let _ = TranslateMessage(&message);
                            DispatchMessageW(&message);
                        }
                        unregister_cycle_hotkey_on_thread();
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

    pub fn update_hotkeys(cycle: &str) {
        let cycle_parsed = Hotkey::parse(cycle).ok();
        *BINDINGS.write() = HookBindings {
            cycle: cycle_parsed,
        };

        // Store the cycle hotkey string for RegisterHotKey on the hook thread
        *CYCLE_HOTKEY_STR.lock() = cycle.to_owned();

        // Wake up the hook thread to re-register the hotkey
        let thread_id = HOOK_THREAD_ID.load(Ordering::Acquire);
        if thread_id != 0 {
            unsafe {
                let _ =
                    PostThreadMessageW(thread_id, WM_REFRESH_CYCLE_HOTKEY, WPARAM(0), LPARAM(0));
            }
        }
    }

    pub fn begin_key_capture() {
        PRESSED_KEYS.lock().clear();
        SUPPRESSED_KEYS.lock().clear();
        CAPTURE_MODE.store(1, Ordering::Release);
    }

    pub fn begin_hotkey_capture() {
        PRESSED_KEYS.lock().clear();
        SUPPRESSED_KEYS.lock().clear();
        CAPTURE_MODE.store(2, Ordering::Release);
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
    *BINDINGS.write() = HookBindings::default();
    PRESSED_KEYS.lock().clear();
    SUPPRESSED_KEYS.lock().clear();
    HOTKEY_FIRED.store(false, Ordering::Relaxed);
    LEFT_ALT_DOWN.store(false, Ordering::Relaxed);
    LEFT_ALT_SOLO.store(false, Ordering::Relaxed);
    RIGHT_DRAGGING.store(false, Ordering::Relaxed);
    CAPTURE_MODE.store(0, Ordering::Relaxed);
}

fn install_hooks() -> Result<(u32, HHOOK, HHOOK)> {
    // SAFETY: Both callbacks have the system ABI and remain valid for the hook lifetime.
    unsafe {
        let thread_id = GetCurrentThreadId();
        HOOK_THREAD_ID.store(thread_id, Ordering::Release);
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
            }
        } else if inserted && LEFT_ALT_DOWN.load(Ordering::Relaxed) {
            // Any other key while Alt is held cancels the solo gesture.
            LEFT_ALT_SOLO.store(false, Ordering::Relaxed);
        }

        let capture_mode = CAPTURE_MODE.load(Ordering::Acquire);
        if inserted && capture_mode != 0 {
            SUPPRESSED_KEYS.lock().insert(vk);
            LEFT_ALT_SOLO.store(false, Ordering::Relaxed);
            if raw_vk == 0x1B {
                CAPTURE_MODE.store(0, Ordering::Release);
                send_ui_action(if capture_mode == 1 {
                    UiAction::CapturedKey(0)
                } else {
                    UiAction::CapturedHotkey(String::new())
                });
            } else if capture_mode == 1 && !crate::hotkey::is_modifier(vk) {
                CAPTURE_MODE.store(0, Ordering::Release);
                send_ui_action(UiAction::CapturedKey(vk));
            } else if capture_mode == 2 && !crate::hotkey::is_modifier(vk) {
                let pressed = PRESSED_KEYS.lock().clone();
                if let Ok(hotkey) = Hotkey::from_keys(pressed) {
                    CAPTURE_MODE.store(0, Ordering::Release);
                    send_ui_action(UiAction::CapturedHotkey(hotkey.display()));
                }
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

        if is_left_alt {
            LEFT_ALT_DOWN.store(false, Ordering::Relaxed);
            // Edge-triggered toggle: firing on key-up keeps a clean 1:1 mapping
            // between physical Alt taps and toggles, regardless of press speed.
            if LEFT_ALT_SOLO.swap(false, Ordering::Relaxed) {
                if let Some(sender) = COMMAND_SENDER.try_lock().and_then(|guard| guard.clone()) {
                    let _ = sender.send(AppCommand::ToggleRunning);
                }
                // Let Alt combos keep working, but suppress the solo Alt key-up
                // so it does not focus menu bars in the foreground app.
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

fn handle_registered_hotkey(pressed: &BTreeSet<u16>) -> bool {
    let bind_window = BTreeSet::from([VK_CONTROL, VK_ALT, 0x20]);
    if pressed == &bind_window {
        bind_window_under_cursor();
        return true;
    }

    let bindings = BINDINGS.read();
    if bindings
        .cycle
        .as_ref()
        .is_some_and(|hotkey| hotkey.matches(pressed))
    {
        send_ui_action(UiAction::NextConfig);
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

/// Register the cycle hotkey using RegisterHotKey on the current thread.
/// Must be called from the hook thread (which owns the message loop).
unsafe fn register_cycle_hotkey_on_thread() {
    let hotkey_str = CYCLE_HOTKEY_STR.lock().clone();
    if hotkey_str.is_empty() {
        return;
    }
    let Ok(hotkey) = Hotkey::parse(&hotkey_str) else {
        return;
    };

    let mut mod_flags: u32 = 0;
    let mut vk_code: u32 = 0;
    for &key in &hotkey.keys {
        match key {
            VK_CONTROL => mod_flags |= 0x0002, // MOD_CONTROL
            VK_ALT => mod_flags |= 0x0001,     // MOD_ALT
            VK_SHIFT => mod_flags |= 0x0004,   // MOD_SHIFT
            VK_WIN => mod_flags |= 0x0008,     // MOD_WIN
            k => vk_code = k as u32,
        }
    }
    if vk_code == 0 {
        return;
    }
    // MOD_NOREPEAT = 0x4000 — prevents repeated WM_HOTKEY when key is held
    mod_flags |= 0x4000;

    #[link(name = "user32")]
    extern "system" {
        fn RegisterHotKey(hwnd: isize, id: i32, fsModifiers: u32, vk: u32) -> i32;
    }
    RegisterHotKey(0, CYCLE_HOTKEY_ID, mod_flags, vk_code);
}

/// Unregister the cycle hotkey. Must be called from the hook thread.
unsafe fn unregister_cycle_hotkey_on_thread() {
    #[link(name = "user32")]
    extern "system" {
        fn UnregisterHotKey(hwnd: isize, id: i32) -> i32;
    }
    UnregisterHotKey(0, CYCLE_HOTKEY_ID);
}
