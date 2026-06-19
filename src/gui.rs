use crate::config::{
    delete_named_config, list_config_names, load_into, save_named_config, AppPreferences, Config,
    DEFAULT_CONFIG_NAME, KEY_SLOT_COUNT, MAX_DELAY_MS, MIN_DELAY_MS,
};
use crate::hook::{GlobalHooks, ALT_TOGGLE_HANDLED_BY_HOOK, NEXT_CONFIG_REQUESTED};
use crate::hotkey::key_display_name;
use crate::single_instance::SingleInstance;
use crate::tray::TrayController;
use crate::window::{enumerate_windows, get_window_title, is_window_valid, WindowInfo};
use crate::{AppCommand, UiAction};
use eframe::egui;
use parking_lot::RwLock;
use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use windows::core::PROPVARIANT;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Storage::EnhancedStorage::PKEY_AppUserModel_RelaunchIconResource;
use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, PSCoerceToCanonicalValue, SHGetPropertyStoreForWindow};
use windows::Win32::UI::WindowsAndMessaging::*;

const AUTOSAVE_INTERVAL: Duration = Duration::from_millis(750);

const CHINESE_FONT_CANDIDATES: &[&str] = &[
    r"C:\Windows\Fonts\NotoSansSC-VF.ttf",
    r"C:\Windows\Fonts\MiSans-Normal.ttf",
    r"C:\Windows\Fonts\msyh.ttf",
    r"C:\Windows\Fonts\msyh.ttc",
    r"C:\Windows\Fonts\simhei.ttf",
    r"C:\Windows\Fonts\simsun.ttc",
];

// Sky blue color constants
const SKY_BLUE_PRIMARY: egui::Color32 = egui::Color32::from_rgb(30, 136, 229); // #1E88E5
const SKY_BLUE_DARK: egui::Color32 = egui::Color32::from_rgb(21, 101, 192); // #1565C0
const SKY_BLUE_LIGHT: egui::Color32 = egui::Color32::from_rgb(227, 242, 253); // #E3F2FD
const SKY_BLUE_VERY_LIGHT: egui::Color32 = egui::Color32::from_rgb(240, 248, 255); // #F0F8FF
const SKY_BLUE_BG: egui::Color32 = egui::Color32::from_rgb(232, 245, 253); // #E8F5FD

fn create_window_icon(is_running: bool, config_name: &str) -> egui::IconData {
    let rgba = crate::icon::render_icon_rgba(is_running, config_name);
    egui::IconData {
        width: crate::icon::ICON_SIZE as u32,
        height: crate::icon::ICON_SIZE as u32,
        rgba,
    }
}

/// Create a Windows HICON from RGBA data for the taskbar icon.
fn create_hicon(is_running: bool, config_name: &str) -> Option<isize> {
    const SIZE: u32 = crate::icon::ICON_SIZE as u32;
    let rgba = crate::icon::render_icon_rgba(is_running, config_name);

    unsafe {
        let hdc = GetDC(None);
        if hdc.is_invalid() {
            return None;
        }

        let mut bmi: BITMAPINFO = std::mem::zeroed();
        bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = SIZE as i32;
        bmi.bmiHeader.biHeight = -(SIZE as i32); // top-down DIB
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = 0; // BI_RGB

        let mut ppv_bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let h_color = match CreateDIBSection(hdc, &bmi, DIB_RGB_COLORS, &mut ppv_bits, None, 0) {
            Ok(h) => h,
            Err(_) => {
                let _ = ReleaseDC(None, hdc);
                return None;
            }
        };
        let _ = ReleaseDC(None, hdc);

        if ppv_bits.is_null() {
            let _ = DeleteObject(h_color);
            return None;
        }

        // Copy RGBA with pre-multiplied alpha and BGRA byte order
        let bits = ppv_bits as *mut u8;
        for i in 0..(SIZE * SIZE) as usize {
            let r = rgba[i * 4];
            let g = rgba[i * 4 + 1];
            let b = rgba[i * 4 + 2];
            let a = rgba[i * 4 + 3];
            *bits.add(i * 4) = (b as u16 * a as u16 / 255) as u8;
            *bits.add(i * 4 + 1) = (g as u16 * a as u16 / 255) as u8;
            *bits.add(i * 4 + 2) = (r as u16 * a as u16 / 255) as u8;
            *bits.add(i * 4 + 3) = a;
        }

        // Create mask bitmap (1bpp, all zeros = fully opaque)
        let mask_row_bytes = (SIZE.div_ceil(32) * 4) as usize;
        let mask_data = vec![0u8; mask_row_bytes * SIZE as usize];
        let h_mask = CreateBitmap(
            SIZE as i32,
            SIZE as i32,
            1,
            1,
            Some(mask_data.as_ptr() as *const _),
        );

        let icon_info = ICONINFO {
            fIcon: TRUE,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: h_mask,
            hbmColor: h_color,
        };

        let hicon = match CreateIconIndirect(&icon_info) {
            Ok(h) => h,
            Err(_) => {
                let _ = DeleteObject(h_color);
                let _ = DeleteObject(h_mask);
                return None;
            }
        };

        let _ = DeleteObject(h_color);
        let _ = DeleteObject(h_mask);

        if hicon.is_invalid() {
            None
        } else {
            Some(hicon.0 as isize)
        }
    }
}

/// Point pinned taskbar shortcuts at the badge-bearing icon resource.
fn set_taskbar_relaunch_icon_resource() -> bool {
    let Some(hwnd) = crate::window::find_own_hwnd() else {
        return false;
    };
    let Ok(exe_path) = std::env::current_exe() else {
        return false;
    };
    let icon_resource = format!("{},-2", exe_path.display());

    unsafe {
        let hwnd = HWND(hwnd as *mut _);
        let Ok(store) = SHGetPropertyStoreForWindow::<_, IPropertyStore>(hwnd) else {
            return false;
        };
        let mut prop = PROPVARIANT::from(icon_resource.as_str());
        if PSCoerceToCanonicalValue(&PKEY_AppUserModel_RelaunchIconResource, &mut prop).is_err() {
            return false;
        }
        store
            .SetValue(&PKEY_AppUserModel_RelaunchIconResource, &prop)
            .and_then(|_| store.Commit())
            .is_ok()
    }
}

/// Update the taskbar icon by sending WM_SETICON to the main window.
fn update_taskbar_icon(is_running: bool, config_name: &str, old_hicon: &mut Option<isize>) {
    let Some(hicon) = create_hicon(is_running, config_name) else {
        return;
    };

    // Destroy old icon if any
    if let Some(old) = old_hicon.take() {
        unsafe {
            let _ = DestroyIcon(HICON(old as *mut _));
        }
    }
    *old_hicon = Some(hicon);

    // Find the main window and set the icon
    if let Some(hwnd) = crate::window::find_own_hwnd() {
        unsafe {
            let hwnd = HWND(hwnd as *mut _);
            let _ = SendMessageW(hwnd, WM_SETICON, WPARAM(0), LPARAM(hicon)); // ICON_SMALL
            let _ = SendMessageW(hwnd, WM_SETICON, WPARAM(1), LPARAM(hicon)); // ICON_BIG
        }
    }
}

fn install_chinese_font_fallback(ctx: &egui::Context) {
    let Some(bytes) = CHINESE_FONT_CANDIDATES
        .iter()
        .find_map(|path| fs::read(path).ok())
    else {
        crate::logging::log_error(
            "font",
            &anyhow::anyhow!("未找到可用中文字体，界面中文可能显示异常"),
        );
        return;
    };

    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "chinese_fallback".to_owned(),
        egui::FontData::from_owned(bytes),
    );

    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        if let Some(family_fonts) = fonts.families.get_mut(&family) {
            family_fonts.push("chinese_fallback".to_owned());
        }
    }

    ctx.set_fonts(fonts);
}

/// Configure a sky blue visual theme matching the original C# version.
fn setup_visuals(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::light();

    visuals.override_text_color = Some(egui::Color32::from_rgb(51, 51, 51));

    visuals.widgets.noninteractive.bg_fill = SKY_BLUE_VERY_LIGHT;
    visuals.widgets.noninteractive.weak_bg_fill = SKY_BLUE_VERY_LIGHT;
    visuals.widgets.noninteractive.fg_stroke =
        egui::Stroke::new(1.0, egui::Color32::from_rgb(170, 195, 220));

    visuals.widgets.inactive.bg_fill = egui::Color32::WHITE;
    visuals.widgets.inactive.weak_bg_fill = SKY_BLUE_LIGHT;
    visuals.widgets.inactive.fg_stroke =
        egui::Stroke::new(1.0, egui::Color32::from_rgb(60, 60, 60));
    visuals.widgets.inactive.rounding = egui::Rounding::same(4.0);

    visuals.widgets.hovered.bg_fill = SKY_BLUE_LIGHT;
    visuals.widgets.hovered.weak_bg_fill = SKY_BLUE_LIGHT;
    visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, SKY_BLUE_PRIMARY);
    visuals.widgets.hovered.rounding = egui::Rounding::same(4.0);

    visuals.widgets.active.bg_fill = SKY_BLUE_PRIMARY;
    visuals.widgets.active.weak_bg_fill = SKY_BLUE_PRIMARY;
    visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0, egui::Color32::WHITE);
    visuals.widgets.active.rounding = egui::Rounding::same(4.0);

    visuals.selection.bg_fill = SKY_BLUE_PRIMARY;
    visuals.selection.stroke = egui::Stroke::new(1.0, SKY_BLUE_PRIMARY);

    visuals.panel_fill = SKY_BLUE_VERY_LIGHT;
    visuals.window_fill = egui::Color32::WHITE;
    visuals.faint_bg_color = SKY_BLUE_BG;

    ctx.set_visuals(visuals);

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(10.0, 6.0);
    style.spacing.interact_size = egui::vec2(40.0, 26.0);
    ctx.set_style(style);
}

struct ActivationWake {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl ActivationWake {
    fn spawn(handle: isize, context: egui::Context) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker = thread::Builder::new()
            .name(crate::stealth::random_thread_name())
            .spawn(move || {
                while !worker_stop.load(Ordering::Acquire) {
                    if SingleInstance::wait_for_activation(handle, 100) {
                        crate::window::restore_own_main_window();
                        context.request_repaint();
                        thread::sleep(Duration::from_millis(20));
                    }
                }
            })
            .ok();
        Self { stop, worker }
    }
}

impl Drop for ActivationWake {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Background thread that directly polls tray menu events.
/// When the window is hidden via ShowWindow(SW_HIDE), the egui event loop stops,
/// so tray.poll() in update() never runs. This thread ensures tray events are
/// always processed regardless of window visibility.
struct TrayExitWatcher {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl TrayExitWatcher {
    fn spawn(egui_ctx: egui::Context, exit_requested: Arc<AtomicBool>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker = thread::Builder::new()
            .name(crate::stealth::random_thread_name())
            .spawn(move || {
                use tray_icon::menu::MenuEvent;
                use tray_icon::TrayIconEvent;

                while !worker_stop.load(Ordering::Acquire) {
                    thread::sleep(Duration::from_millis(50));

                    // Poll tray icon events (double-click to show)
                    while let Ok(event) = TrayIconEvent::receiver().try_recv() {
                        if matches!(event, TrayIconEvent::DoubleClick { .. }) {
                            crate::window::restore_own_main_window();
                            egui_ctx.request_repaint();
                        }
                    }

                    // Poll menu events — handle all events directly
                    while let Ok(event) = MenuEvent::receiver().try_recv() {
                        let id_str = event.id.as_ref();
                        if id_str == "exit" {
                            exit_requested.store(true, Ordering::Release);
                            // Must restore window first — when hidden (SW_HIDE),
                            // eframe stops its event loop and update() never runs,
                            // so close_requested() is never checked.
                            crate::window::restore_own_main_window();
                            let _ = crate::window::request_own_main_window_close();
                            egui_ctx.request_repaint();
                        } else if id_str == "show" {
                            crate::window::restore_own_main_window();
                            egui_ctx.request_repaint();
                        } else if id_str == "autostart" {
                            // Toggle autostart
                            let enabled = !crate::tray::is_autostart_enabled();
                            let _ = crate::tray::set_autostart(enabled);
                        }
                    }
                }
            })
            .ok();
        Self { stop, worker }
    }
}

impl Drop for TrayExitWatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub struct AutoKeyApp {
    config: Arc<RwLock<Config>>,
    preferences: Arc<RwLock<AppPreferences>>,
    command_tx: Sender<AppCommand>,
    ui_rx: Receiver<UiAction>,
    is_running: Arc<AtomicBool>,
    key_running: Arc<RwLock<Vec<bool>>>,
    bound_window: Arc<RwLock<Option<isize>>>,
    status: Arc<RwLock<String>>,
    _activation_wake: ActivationWake,
    hooks_available: bool,
    profile_names: Vec<String>,
    profile_name_edit: String,
    show_window_selector: bool,
    available_windows: Vec<WindowInfo>,
    capturing_key: Option<usize>,
    tray: Option<TrayController>,
    last_icon_state: Option<(bool, String)>,
    last_saved_config: Config,
    last_saved_preferences: AppPreferences,
    last_autosave: Instant,
    last_config_switch: Instant,
    last_hook_alt_toggle: Instant,
    alt_fallback_down: bool,
    alt_fallback_solo: bool,
    taskbar_hicon: Option<isize>,
    taskbar_relaunch_icon_set: bool,
    exit_requested: Arc<AtomicBool>,
    _tray_exit_watcher: TrayExitWatcher,
}

impl AutoKeyApp {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: Arc<RwLock<Config>>,
        preferences: Arc<RwLock<AppPreferences>>,
        command_tx: Sender<AppCommand>,
        ui_rx: Receiver<UiAction>,
        is_running: Arc<AtomicBool>,
        key_running: Arc<RwLock<Vec<bool>>>,
        bound_window: Arc<RwLock<Option<isize>>>,
        status: Arc<RwLock<String>>,
        activation_handle: isize,
        hooks_available: bool,
        egui_context: egui::Context,
    ) -> Self {
        let last_saved_config = config.read().clone();
        let last_saved_preferences = preferences.read().clone();
        let profile_name_edit = last_saved_preferences.selected_config.clone();
        let tray = if std::env::var_os(crate::obfstr!("SYSUTIL_DISABLE_TRAY")).is_some() {
            None
        } else {
            match TrayController::new() {
                Ok(tray) => Some(tray),
                Err(error) => {
                    crate::logging::log_error("tray", &error);
                    *status.write() = format!(
                        "\u{7cfb}\u{7edf}\u{6258}\u{76d8}\u{4e0d}\u{53ef}\u{7528}: {error}"
                    );
                    None
                }
            }
        };

        let exit_requested = Arc::new(AtomicBool::new(false));
        let tray_exit_watcher =
            TrayExitWatcher::spawn(egui_context.clone(), exit_requested.clone());

        let mut app = Self {
            config,
            preferences,
            command_tx,
            ui_rx,
            is_running,
            key_running,
            bound_window,
            status,
            _activation_wake: ActivationWake::spawn(activation_handle, egui_context),
            hooks_available,
            profile_names: Vec::new(),
            profile_name_edit,
            show_window_selector: false,
            available_windows: Vec::new(),
            capturing_key: None,
            tray,
            last_icon_state: None,
            last_saved_config,
            last_saved_preferences,
            last_autosave: Instant::now(),
            last_config_switch: Instant::now() - Duration::from_secs(10),
            last_hook_alt_toggle: Instant::now() - Duration::from_secs(10),
            alt_fallback_down: false,
            alt_fallback_solo: false,
            taskbar_hicon: None,
            taskbar_relaunch_icon_set: false,
            exit_requested,
            _tray_exit_watcher: tray_exit_watcher,
        };
        app.refresh_profiles_and_hotkeys();
        app
    }

    fn refresh_profiles_and_hotkeys(&mut self) {
        self.profile_names = list_config_names().unwrap_or_default();
    }

    fn switch_to_next_config(&mut self) {
        if self.last_config_switch.elapsed() < Duration::from_millis(300) {
            return; // debounce
        }
        self.last_config_switch = Instant::now();

        if let Some(pos) = self
            .profile_names
            .iter()
            .position(|n| *n == self.preferences.read().selected_config)
        {
            let next = (pos + 1) % self.profile_names.len();
            let name = self.profile_names[next].clone();
            self.save_now();
            if let Err(error) = load_into(&name, &self.config) {
                *self.status.write() = format!("切换配置失败: {error}");
            } else {
                self.preferences.write().selected_config = name.clone();
                self.profile_name_edit = name.clone();
                *self.status.write() = format!("已切换到配置 [{name}]");
                self.refresh_profiles_and_hotkeys();
            }
        }
    }

    fn autosave_if_changed(&mut self) {
        if self.last_autosave.elapsed() < AUTOSAVE_INTERVAL {
            return;
        }
        self.last_autosave = Instant::now();

        let current_config = self.config.read().clone();
        if current_config != self.last_saved_config {
            let name = self.preferences.read().selected_config.clone();
            if let Err(error) = save_named_config(&name, &self.config) {
                *self.status.write() =
                    format!("\u{81ea}\u{52a8}\u{4fdd}\u{5b58}\u{5931}\u{8d25}: {error}");
            } else {
                self.last_saved_config = current_config;
            }
        }

        let current_preferences = self.preferences.read().clone();
        if current_preferences != self.last_saved_preferences {
            if let Err(error) = crate::config::save_preferences(&current_preferences) {
                *self.status.write() =
                    format!("\u{4fdd}\u{5b58}\u{504f}\u{597d}\u{5931}\u{8d25}: {error}");
            } else {
                self.last_saved_preferences = current_preferences;
            }
        }
    }

    fn save_now(&mut self) {
        self.last_autosave = Instant::now();

        let current_config = self.config.read().clone();
        if current_config != self.last_saved_config {
            let name = self.preferences.read().selected_config.clone();
            if let Err(error) = save_named_config(&name, &self.config) {
                crate::logging::log_error("save_config", &error);
                *self.status.write() = format!("\u{4fdd}\u{5b58}\u{5931}\u{8d25}: {error}");
            } else {
                self.last_saved_config = current_config;
            }
        }

        let current_preferences = self.preferences.read().clone();
        if current_preferences != self.last_saved_preferences {
            if let Err(error) = crate::config::save_preferences(&current_preferences) {
                crate::logging::log_error("save_preferences", &error);
                *self.status.write() =
                    format!("\u{4fdd}\u{5b58}\u{504f}\u{597d}\u{5931}\u{8d25}: {error}");
            } else {
                self.last_saved_preferences = current_preferences;
            }
        }
    }

    fn persist_window_state(&mut self, ctx: &egui::Context) {
        let Some(rect) = ctx.input(|input| input.viewport().outer_rect) else {
            return;
        };
        let pos = rect.min;
        let size = rect.size();
        if !pos.x.is_finite() || !pos.y.is_finite() || !size.x.is_finite() || !size.y.is_finite() {
            return;
        }
        if size.x <= 0.0 || size.y <= 0.0 {
            return;
        }

        let preferences = {
            let mut preferences = self.preferences.write();
            preferences.window_x = pos.x;
            preferences.window_y = pos.y;
            preferences.window_width = size.x;
            preferences.window_height = size.y;
            preferences.sanitize();
            preferences.clone()
        };

        if let Err(error) = crate::config::save_preferences(&preferences) {
            crate::logging::log_error("save_window_state", &error);
            *self.status.write() = format!("保存窗口状态失败: {error}");
        } else {
            self.last_saved_preferences = preferences;
        }
    }

    fn process_ui_actions(&mut self) {
        while let Ok(action) = self.ui_rx.try_recv() {
            match action {
                UiAction::CapturedKey(vk) => {
                    if let Some(index) = self.capturing_key.take() {
                        let mut config = self.config.write();
                        if let Some(key) = config.keys.get_mut(index) {
                            if vk == 0 {
                                key.vk_code = 0;
                                key.key_name = "\u{672a}\u{8bbe}\u{7f6e}".to_owned();
                            } else {
                                key.vk_code = vk;
                                key.key_name = key_display_name(vk);
                            }
                        }
                    }
                    GlobalHooks::cancel_capture();
                }
            }
        }
    }

    fn process_focused_window_hotkeys(&mut self, ctx: &egui::Context) {
        if ALT_TOGGLE_HANDLED_BY_HOOK.swap(false, Ordering::Acquire) {
            self.last_hook_alt_toggle = Instant::now();
        }

        let ctrl_z_pressed = ctx.input_mut(|input| {
            let mut pressed = false;
            input.events.retain(|event| {
                let is_match = matches!(
                    event,
                    egui::Event::Key {
                        key: egui::Key::Z,
                        modifiers,
                        pressed: true,
                        ..
                    } if modifiers.ctrl && !modifiers.alt && !modifiers.shift
                );
                pressed |= is_match;
                !is_match
            });
            pressed
        });
        if ctrl_z_pressed {
            self.switch_to_next_config();
        }

        let (alt_down, cancel_solo) = ctx.input(|input| {
            let cancel_solo = input.events.iter().any(|event| {
                matches!(
                    event,
                    egui::Event::Key { pressed: true, .. }
                        | egui::Event::PointerButton { pressed: true, .. }
                        | egui::Event::MouseWheel { .. }
                        | egui::Event::Text(_)
                )
            });
            (input.modifiers.alt, cancel_solo)
        });

        if alt_down {
            if !self.alt_fallback_down {
                self.alt_fallback_down = true;
                self.alt_fallback_solo = !cancel_solo;
            } else if cancel_solo {
                self.alt_fallback_solo = false;
            }
        } else if self.alt_fallback_down {
            let was_solo = self.alt_fallback_solo;
            self.alt_fallback_down = false;
            self.alt_fallback_solo = false;

            if was_solo && self.last_hook_alt_toggle.elapsed() > Duration::from_millis(300) {
                let _ = self.command_tx.send(AppCommand::ToggleRunning);
            }
        }
    }

    fn render_header(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("header")
            .exact_height(52.0)
            .frame(
                egui::Frame::none()
                    .fill(SKY_BLUE_PRIMARY)
                    .inner_margin(egui::Margin::symmetric(12.0, 6.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(
                            egui::RichText::new(crate::obfstr!("调度器"))
                                .size(18.0)
                                .strong()
                                .color(egui::Color32::WHITE),
                        );
                        ui.label(
                            egui::RichText::new(crate::obfstr!("Windows 按键调度器"))
                                .size(10.0)
                                .color(egui::Color32::from_rgba_premultiplied(255, 255, 255, 180)),
                        );
                    });

                    let right_size = egui::vec2(ui.available_width(), 36.0);
                    ui.allocate_ui_with_layout(
                        right_size,
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            let running = self.is_running.load(Ordering::Acquire);
                            let button =
                                egui::Button::new(if running { "停止" } else { "启动" })
                                    .min_size(egui::vec2(70.0, 30.0))
                                    .rounding(egui::Rounding::same(4.0))
                                    .fill(if running {
                                        egui::Color32::from_rgb(244, 67, 54)
                                    } else {
                                        egui::Color32::from_rgb(76, 175, 80)
                                    })
                                    .stroke(egui::Stroke::new(
                                        1.0,
                                        if running {
                                            egui::Color32::from_rgb(211, 47, 47)
                                        } else {
                                            egui::Color32::from_rgb(56, 142, 60)
                                        },
                                    ));
                            if ui
                                .add(button)
                                .on_hover_text("也可以单独按下左 Alt 切换运行状态")
                                .clicked()
                            {
                                let command = if running {
                                    AppCommand::Stop
                                } else {
                                    AppCommand::Start
                                };
                                let _ = self.command_tx.send(command);
                            }
                            ui.add_space(8.0);
                            ui.label(
                                egui::RichText::new(if running {
                                    "运行中"
                                } else {
                                    "已停止"
                                })
                                .size(12.0)
                                .color(egui::Color32::from_rgba_premultiplied(255, 255, 255, 200)),
                            );
                        },
                    );
                });
            });
    }

    fn render_settings(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("settings")
            .resizable(false)
            .exact_width(280.0)
            .frame(
                egui::Frame::none()
                    .fill(SKY_BLUE_LIGHT)
                    .inner_margin(egui::Margin::same(10.0)),
            )
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.label(
                        egui::RichText::new("运行设置")
                            .size(13.0)
                            .strong()
                            .color(SKY_BLUE_DARK),
                    );
                    ui.add_space(4.0);
                    self.render_run_settings(ui);

                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new("发送目标")
                            .size(13.0)
                            .strong()
                            .color(SKY_BLUE_DARK),
                    );
                    ui.add_space(4.0);
                    self.render_target_settings(ui);

                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new("配置档")
                            .size(13.0)
                            .strong()
                            .color(SKY_BLUE_DARK),
                    );
                    ui.add_space(4.0);
                    self.render_profile_settings(ui);
                });
            });
    }

    fn render_run_settings(&mut self, ui: &mut egui::Ui) {
        let mut config = self.config.write();

        ui.checkbox(&mut config.independent_loop, "\u{72ec}\u{7acb}\u{5faa}\u{73af}\u{6a21}\u{5f0f}")
            .on_hover_text("\u{5f00}\u{542f}\u{540e}\u{6bcf}\u{4e2a}\u{6309}\u{952e}\u{72ec}\u{7acb}\u{5faa}\u{73af}\u{ff0c}\u{5173}\u{95ed}\u{540e}\u{6309}\u{987a}\u{5e8f}\u{9010}\u{952e}\u{6267}\u{884c}");

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label("\u{6700}\u{5927}\u{5faa}\u{73af}\u{6b21}\u{6570}:");
            ui.add(
                egui::DragValue::new(&mut config.max_loops)
                    .range(-1..=1_000_000)
                    .speed(1),
            );
            ui.label("(-1 \u{4e3a}\u{65e0}\u{9650})");
        });

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label("\u{5168}\u{5c40}\u{968f}\u{673a}\u{5ef6}\u{8fdf}:");
            ui.add(egui::Slider::new(&mut config.global_random_delay, 0..=5000).text("ms"));
        });

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label("\u{65f6}\u{5e8f}\u{53d8}\u{5316}\u{7ea7}\u{522b}:");
            egui::ComboBox::from_id_salt("timing_level")
                .selected_text(match config.timing_variation_level {
                    0 => "0 - \u{57fa}\u{7840}",
                    1 => "1 - \u{6807}\u{51c6}",
                    _ => "2 - \u{62df}\u{4eba}",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut config.timing_variation_level,
                        0,
                        "0 - \u{57fa}\u{7840}",
                    );
                    ui.selectable_value(
                        &mut config.timing_variation_level,
                        1,
                        "1 - \u{6807}\u{51c6}",
                    );
                    ui.selectable_value(
                        &mut config.timing_variation_level,
                        2,
                        "2 - \u{62df}\u{4eba}",
                    );
                });
        });
    }

    fn render_target_settings(&mut self, ui: &mut egui::Ui) {
        let bound = *self.bound_window.read();
        match bound {
            Some(hwnd) => {
                let title = get_window_title(hwnd);
                let valid = is_window_valid(hwnd);
                ui.label(format!(
                    "\u{5df2}\u{7ed1}\u{5b9a}: {}",
                    if title.is_empty() {
                        format!("{hwnd:#x}")
                    } else {
                        title
                    }
                ));
                if !valid {
                    ui.colored_label(
                        egui::Color32::from_rgb(220, 80, 80),
                        "\u{7a97}\u{53e3}\u{5df2}\u{5931}\u{6548}",
                    );
                }
                if ui.button("\u{89e3}\u{9664}\u{7ed1}\u{5b9a}").clicked() {
                    *self.bound_window.write() = None;
                    *self.status.write() =
                        "\u{5df2}\u{89e3}\u{9664}\u{7a97}\u{53e3}\u{7ed1}\u{5b9a}".to_owned();
                }
            }
            None => {
                ui.label("\u{672a}\u{7ed1}\u{5b9a}\u{7a97}\u{53e3} (\u{53d1}\u{9001}\u{5230}\u{524d}\u{53f0})");
                if ui.button("\u{9009}\u{62e9}\u{7a97}\u{53e3}...").clicked() {
                    self.show_window_selector = true;
                    self.available_windows = enumerate_windows();
                }
            }
        }

        if self.show_window_selector {
            ui.add_space(6.0);
            egui::Window::new("\u{9009}\u{62e9}\u{76ee}\u{6807}\u{7a97}\u{53e3}")
                .collapsible(false)
                .resizable(false)
                .show(ui.ctx(), |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(300.0)
                        .show(ui, |ui| {
                            for window in &self.available_windows {
                                if ui.button(&window.title).clicked() {
                                    *self.bound_window.write() = Some(window.hwnd);
                                    *self.status.write() =
                                        format!("\u{5df2}\u{7ed1}\u{5b9a}: {}", window.title);
                                    self.show_window_selector = false;
                                }
                            }
                        });
                    if ui.button("\u{53d6}\u{6d88}").clicked() {
                        self.show_window_selector = false;
                    }
                });
        }

        ui.add_space(4.0);
    }

    fn render_profile_settings(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("\u{540d}\u{79f0}:");
            ui.text_edit_singleline(&mut self.profile_name_edit);
        });

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui.button("\u{4fdd}\u{5b58}").clicked() {
                let name = self.profile_name_edit.clone();
                match save_named_config(&name, &self.config) {
                    Ok(saved_name) => {
                        self.preferences.write().selected_config = saved_name.clone();
                        self.profile_name_edit = saved_name;
                        *self.status.write() =
                            "\u{914d}\u{7f6e}\u{5df2}\u{4fdd}\u{5b58}".to_owned();
                        self.refresh_profiles_and_hotkeys();
                    }
                    Err(error) => {
                        *self.status.write() = format!("\u{4fdd}\u{5b58}\u{5931}\u{8d25}: {error}");
                    }
                }
            }
            if ui.button("\u{52a0}\u{8f7d}").clicked() {
                let name = self.profile_name_edit.clone();
                self.save_now();
                match load_into(&name, &self.config) {
                    Ok(()) => {
                        self.preferences.write().selected_config = name.clone();
                        *self.status.write() =
                            format!("\u{5df2}\u{52a0}\u{8f7d}\u{914d}\u{7f6e} [{name}]");
                        self.refresh_profiles_and_hotkeys();
                    }
                    Err(error) => {
                        *self.status.write() = format!("\u{52a0}\u{8f7d}\u{5931}\u{8d25}: {error}");
                    }
                }
            }
            if ui.button("\u{5220}\u{9664}").clicked() {
                let name = self.profile_name_edit.clone();
                self.save_now();
                match delete_named_config(&name) {
                    Ok(()) => {
                        if self.preferences.read().selected_config == name {
                            self.preferences.write().selected_config =
                                DEFAULT_CONFIG_NAME.to_owned();
                            self.profile_name_edit = DEFAULT_CONFIG_NAME.to_owned();
                            let _ = load_into(DEFAULT_CONFIG_NAME, &self.config);
                        }
                        *self.status.write() =
                            format!("\u{5df2}\u{5220}\u{9664}\u{914d}\u{7f6e} [{name}]");
                        self.refresh_profiles_and_hotkeys();
                    }
                    Err(error) => {
                        *self.status.write() = format!("\u{5220}\u{9664}\u{5931}\u{8d25}: {error}");
                    }
                }
            }
        });

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label("\u{5207}\u{6362}\u{914d}\u{7f6e}:");
            let mut selected = self.preferences.read().selected_config.clone();
            egui::ComboBox::from_id_salt("config_selector")
                .width(160.0)
                .selected_text(&selected)
                .show_ui(ui, |ui| {
                    for name in &self.profile_names {
                        ui.selectable_value(&mut selected, name.clone(), name);
                    }
                });
            if selected != self.preferences.read().selected_config {
                self.save_now();
                if let Err(error) = load_into(&selected, &self.config) {
                    *self.status.write() = format!("\u{5207}\u{6362}\u{5931}\u{8d25}: {error}");
                } else {
                    self.preferences.write().selected_config = selected.clone();
                    self.profile_name_edit = selected;
                    self.refresh_profiles_and_hotkeys();
                }
            }
        });

        ui.add_space(8.0);
    }

    fn render_key_table(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(SKY_BLUE_VERY_LIGHT)
                    .inner_margin(egui::Margin::same(10.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("全选").clicked() {
                        for key in &mut self.config.write().keys {
                            key.enabled = true;
                        }
                    }
                    if ui.button("反选").clicked() {
                        for key in &mut self.config.write().keys {
                            key.enabled = !key.enabled;
                        }
                    }
                });
                ui.add_space(6.0);

                egui::ScrollArea::both().show(ui, |ui| {
                    // Calculate column widths to fill available space
                    let available = ui.available_width();
                    let col_num = 36.0; // # column
                    let col_key = 120.0; // 按键 column
                    let col_enabled = 50.0; // 启用 column
                    let col_status = 40.0; // 状态 column
                    let spacing = 12.0 * 5.0; // 5 gaps between 6 columns
                    let remaining =
                        (available - col_num - col_key - col_enabled - col_status - spacing)
                            .max(80.0);
                    let col_delay = remaining / 2.0; // 基础延迟 — auto-expand
                    let col_range = remaining / 2.0; // 随机范围 — auto-expand

                    egui::Grid::new("key_table")
                        .spacing(egui::vec2(12.0, 8.0))
                        .show(ui, |ui| {
                            // Header row
                            ui.label(
                                egui::RichText::new("#")
                                    .strong()
                                    .size(13.0)
                                    .color(SKY_BLUE_DARK),
                            );
                            ui.label(
                                egui::RichText::new("按键")
                                    .strong()
                                    .size(13.0)
                                    .color(SKY_BLUE_DARK),
                            );
                            ui.label(
                                egui::RichText::new("基础延迟(ms)")
                                    .strong()
                                    .size(13.0)
                                    .color(SKY_BLUE_DARK),
                            );
                            ui.label(
                                egui::RichText::new("随机范围(ms)")
                                    .strong()
                                    .size(13.0)
                                    .color(SKY_BLUE_DARK),
                            );
                            ui.label(
                                egui::RichText::new("启用")
                                    .strong()
                                    .size(13.0)
                                    .color(SKY_BLUE_DARK),
                            );
                            ui.label(
                                egui::RichText::new("状态")
                                    .strong()
                                    .size(13.0)
                                    .color(SKY_BLUE_DARK),
                            );
                            ui.end_row();

                            #[derive(Default)]
                            struct RowEdit {
                                base_delay: Option<u32>,
                                random_range: Option<u32>,
                                enabled: Option<bool>,
                            }

                            let mut edits: Vec<RowEdit> =
                                (0..KEY_SLOT_COUNT).map(|_| RowEdit::default()).collect();

                            {
                                let config = self.config.read();
                                let key_running = self.key_running.read();

                                for (index, key) in config.keys.iter().enumerate() {
                                    let is_capturing = self.capturing_key == Some(index);
                                    let is_active =
                                        key_running.get(index).copied().unwrap_or(false);

                                    ui.label(
                                        egui::RichText::new(format!("{}", index + 1))
                                            .size(13.0)
                                            .color(egui::Color32::from_rgb(100, 100, 100)),
                                    );

                                    let button_text = if is_capturing {
                                        "按任意键..."
                                    } else {
                                        &key.key_name
                                    };
                                    let button = egui::Button::new(
                                        egui::RichText::new(button_text).size(13.0),
                                    )
                                    .min_size(egui::vec2(col_key, 28.0))
                                    .rounding(egui::Rounding::same(3.0));
                                    if ui.add(button).clicked() {
                                        if is_capturing {
                                            self.capturing_key = None;
                                            GlobalHooks::cancel_capture();
                                        } else {
                                            self.capturing_key = Some(index);
                                            GlobalHooks::begin_key_capture();
                                        }
                                    }

                                    let mut base_delay = key.base_delay;
                                    ui.add_sized(
                                        [col_delay, 28.0],
                                        egui::DragValue::new(&mut base_delay)
                                            .range(MIN_DELAY_MS..=MAX_DELAY_MS)
                                            .speed(10),
                                    );
                                    if base_delay != key.base_delay {
                                        edits[index].base_delay = Some(base_delay);
                                    }

                                    let mut random_range = key.random_range;
                                    ui.add_sized(
                                        [col_range, 28.0],
                                        egui::DragValue::new(&mut random_range)
                                            .range(0..=MAX_DELAY_MS)
                                            .speed(10),
                                    );
                                    if random_range != key.random_range {
                                        edits[index].random_range = Some(random_range);
                                    }

                                    let mut enabled = key.enabled;
                                    ui.checkbox(&mut enabled, "");
                                    if enabled != key.enabled {
                                        edits[index].enabled = Some(enabled);
                                    }

                                    if is_active {
                                        ui.label(
                                            egui::RichText::new("\u{25cf}")
                                                .size(14.0)
                                                .color(egui::Color32::from_rgb(76, 175, 80)),
                                        );
                                    } else {
                                        ui.label(
                                            egui::RichText::new("\u{25cb}")
                                                .size(14.0)
                                                .color(egui::Color32::from_rgb(189, 189, 189)),
                                        );
                                    }

                                    ui.end_row();
                                }
                            }

                            if edits.iter().any(|e| {
                                e.base_delay.is_some()
                                    || e.random_range.is_some()
                                    || e.enabled.is_some()
                            }) {
                                let mut config = self.config.write();
                                for (index, edit) in edits.into_iter().enumerate() {
                                    if let Some(key) = config.keys.get_mut(index) {
                                        if let Some(d) = edit.base_delay {
                                            key.base_delay = d;
                                        }
                                        if let Some(r) = edit.random_range {
                                            key.random_range = r;
                                        }
                                        if let Some(e) = edit.enabled {
                                            key.enabled = e;
                                        }
                                    }
                                }
                            }
                        });
                });
            });
    }

    fn render_status_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("status_bar")
            .exact_height(24.0)
            .frame(
                egui::Frame::none()
                    .fill(SKY_BLUE_LIGHT)
                    .inner_margin(egui::Margin::symmetric(10.0, 3.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let status = self.status.read().clone();
                    ui.label(
                        egui::RichText::new(status)
                            .size(11.0)
                            .color(egui::Color32::from_rgb(80, 120, 160)),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if !self.hooks_available {
                            ui.colored_label(
                                egui::Color32::from_rgb(230, 81, 0),
                                "全局快捷键不可用",
                            );
                        }
                    });
                });
            });
    }
}

impl Drop for AutoKeyApp {
    fn drop(&mut self) {
        if let Some(hicon) = self.taskbar_hicon.take() {
            unsafe {
                let _ = DestroyIcon(HICON(hicon as *mut _));
            }
        }
    }
}

impl eframe::App for AutoKeyApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        setup_visuals(ctx);

        self.process_ui_actions();
        if NEXT_CONFIG_REQUESTED.swap(false, Ordering::Acquire) {
            self.switch_to_next_config();
        }
        self.process_focused_window_hotkeys(ctx);

        // The low-level hook handles global hotkeys. This focused-window pass is
        // a local fallback for egui/winit edge cases while the app itself is active.

        // Poll tray events for autostart toggle only.
        // Exit and Show events are handled by the TrayExitWatcher background thread,
        // which works even when the window is hidden and update() stops being called.
        if let Some(tray) = &self.tray {
            tray.poll(); // handles autostart toggle
        }

        if let Some(tray) = &mut self.tray {
            let running = self.is_running.load(Ordering::Acquire);
            let name = self.preferences.read().selected_config.clone();
            tray.update(running, &name);
        }

        if !self.taskbar_relaunch_icon_set {
            self.taskbar_relaunch_icon_set = set_taskbar_relaunch_icon_resource();
        }

        // Update window and taskbar icons when either running state or config changes.
        {
            let running = self.is_running.load(Ordering::Acquire);
            let name = self.preferences.read().selected_config.clone();
            let state = (running, name.clone());
            if self.last_icon_state.as_ref() != Some(&state) {
                self.last_icon_state = Some(state);
                let icon = create_window_icon(running, &name);
                ctx.send_viewport_cmd(egui::ViewportCommand::Icon(Some(Arc::new(icon))));
                update_taskbar_icon(running, &name, &mut self.taskbar_hicon);
            }
        }

        self.autosave_if_changed();

        // Clicking X → cancel close and hide window to tray
        if ctx.input(|i| i.viewport().close_requested()) {
            if self.tray.is_some() && !self.exit_requested.load(Ordering::Acquire) {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.persist_window_state(ctx);
                self.save_now();
                self.hide_window();
            } else {
                self.persist_window_state(ctx);
                self.save_now();
                if let Some(hicon) = self.taskbar_hicon.take() {
                    unsafe {
                        let _ = DestroyIcon(HICON(hicon as *mut _));
                    }
                }
            }
        }

        self.render_header(ctx);
        self.render_settings(ctx);
        self.render_key_table(ctx);
        self.render_status_bar(ctx);

        // Keep the event loop alive so tray.poll() keeps being called
        ctx.request_repaint_after(Duration::from_millis(200));
    }
}

impl AutoKeyApp {
    fn hide_window(&mut self) {
        if let Some(hwnd) = crate::window::find_own_hwnd() {
            unsafe {
                let _ = ShowWindowAsync(HWND(hwnd as *mut _), SW_HIDE);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn run_gui(
    config: Arc<RwLock<Config>>,
    preferences: Arc<RwLock<AppPreferences>>,
    command_tx: Sender<AppCommand>,
    ui_rx: Receiver<UiAction>,
    is_running: Arc<AtomicBool>,
    key_running: Arc<RwLock<Vec<bool>>>,
    bound_window: Arc<RwLock<Option<isize>>>,
    status: Arc<RwLock<String>>,
    activation_handle: isize,
    hooks_available: bool,
) -> Result<(), eframe::Error> {
    let title = format!("{} - {}", crate::obfstr!("调度器"), preferences.read().selected_config);
    let (width, height, pos_x, pos_y) = {
        let prefs = preferences.read();
        (
            prefs.window_width,
            prefs.window_height,
            prefs.window_x,
            prefs.window_y,
        )
    };

    let icon = Arc::new(create_window_icon(
        false,
        &preferences.read().selected_config,
    ));

    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([width, height])
        .with_title(title)
        .with_app_id(crate::APP_USER_MODEL_ID)
        .with_icon(icon);

    // Restore window position if previously saved
    if pos_x.is_finite() && pos_y.is_finite() {
        viewport = viewport.with_position([pos_x, pos_y]);
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        &crate::obfstr!("调度器"),
        options,
        Box::new(move |cc| {
            install_chinese_font_fallback(&cc.egui_ctx);
            let app = AutoKeyApp::new(
                config,
                preferences,
                command_tx,
                ui_rx,
                is_running,
                key_running,
                bound_window,
                status,
                activation_handle,
                hooks_available,
                cc.egui_ctx.clone(),
            );
            Ok(Box::new(app))
        }),
    )
}
