use crate::hotkey::{is_modifier, normalize_vk, Hotkey, VK_ALT, VK_CONTROL};
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
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_LMENU, VK_MENU};
use windows::Win32::UI::WindowsAndMessaging::*;

const RIGHT_DRAG_THRESHOLD_SQUARED: i64 = 64;

type SharedBoundWindow = Arc<RwLock<Option<isize>>>;

#[derive(Clone, Default)]
struct HookBindings {
    cycle: Option<Hotkey>,
    profiles: Vec<(String, Hotkey)>,
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

    pub fn update_hotkeys(cycle: &str, profiles: &[(String, String)]) {
        let cycle = Hotkey::parse(cycle).ok();
        let profiles = profiles
            .iter()
            .filter_map(|(name, value)| {
                Hotkey::parse(value)
                    .ok()
                    .map(|hotkey| (name.clone(), hotkey))
            })
            .collect();
        *BINDINGS.write() = HookBindings { cycle, profiles };
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
    BINDINGS.write().profiles.clear();
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
    let was_suppressed = is_key_up && SUPPRESSED_KEYS.lock().remove(&raw_vk);
    let is_left_alt = raw_vk == VK_LMENU.0
        || (raw_vk == VK_MENU.0 && event.scanCode == 0x38 && event.flags.0 & LLKHF_EXTENDED.0 == 0);

    if is_key_down {
        let inserted = PRESSED_KEYS.lock().insert(raw_vk);
        if inserted {
            if is_left_alt && !LEFT_ALT_DOWN.swap(true, Ordering::Relaxed) {
                LEFT_ALT_SOLO.store(true, Ordering::Relaxed);
            } else if LEFT_ALT_DOWN.load(Ordering::Relaxed) {
                LEFT_ALT_SOLO.store(false, Ordering::Relaxed);
            }
        }

        let capture_mode = CAPTURE_MODE.load(Ordering::Acquire);
        if inserted && capture_mode != 0 {
            SUPPRESSED_KEYS.lock().insert(raw_vk);
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
                send_ui_action(UiAction::CapturedKey(raw_vk));
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
            }
        }
    } else if is_key_up {
        PRESSED_KEYS.lock().remove(&raw_vk);
        // Reset HOTKEY_FIRED when a non-modifier key is released (not when ALL keys are released).
        // This allows re-firing the hotkey when the user holds modifiers and presses
        // the trigger key again (e.g. Ctrl+Alt held, Space pressed multiple times),
        // matching the C# version's MOD_NOREPEAT behavior.
        if !is_modifier(vk) || PRESSED_KEYS.lock().is_empty() {
            HOTKEY_FIRED.store(false, Ordering::Relaxed);
        }

        if is_left_alt
            && LEFT_ALT_DOWN.swap(false, Ordering::Relaxed)
            && LEFT_ALT_SOLO.swap(false, Ordering::Relaxed)
        {
            if let Some(sender) = COMMAND_SENDER.try_lock().and_then(|guard| guard.clone()) {
                let _ = sender.send(AppCommand::ToggleRunning);
            }
        }
    }
    was_suppressed || CAPTURE_MODE.load(Ordering::Acquire) != 0
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
    if let Some((name, _)) = bindings
        .profiles
        .iter()
        .find(|(_, hotkey)| hotkey.matches(pressed))
    {
        send_ui_action(UiAction::LoadConfig(name.clone()));
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
        }
    }
}

fn bind_window_at(point: POINT) {
    // SAFETY: WindowFromPoint/GetAncestor return opaque handles that are validated below.
    unsafe {
        let child = WindowFromPoint(point);
        if child.0.is_null() {
            return;
        }
        let root = GetAncestor(child, GA_ROOT);
        let target = if root.0.is_null() { child } else { root };
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
