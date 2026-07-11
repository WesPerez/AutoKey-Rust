use crate::config::{
    delete_named_config, list_config_names, load_into, save_named_config, AppPreferences, Config,
    DEFAULT_CONFIG_NAME, KEY_SLOT_COUNT, MAX_DELAY_MS, MIN_DELAY_MS,
};
use crate::hook::{GlobalHooks, ALT_TOGGLE_HANDLED_BY_HOOK, NEXT_CONFIG_REQUESTED};
use crate::hotkey::key_display_name;
use crate::single_instance::SingleInstance;
use crate::taskbar::TaskbarDecoration;
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
use windows::Win32::Foundation::*;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_TIP, NIM_MODIFY, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::*;

const AUTOSAVE_INTERVAL: Duration = Duration::from_millis(750);
const TASKBAR_ICON_RETRY_INTERVAL: Duration = Duration::from_millis(500);
const TRAY_ICON_SYNC_INTERVAL: Duration = Duration::from_secs(2);
const TRAY_NOTIFY_ID_SEARCH_LIMIT: u32 = 16;

const CHINESE_FONT_CANDIDATES: &[&str] = &[
    r"C:\Windows\Fonts\NotoSansSC-VF.ttf",
    r"C:\Windows\Fonts\MiSans-Normal.ttf",
    r"C:\Windows\Fonts\msyh.ttf",
    r"C:\Windows\Fonts\msyh.ttc",
    r"C:\Windows\Fonts\simhei.ttf",
    r"C:\Windows\Fonts\simsun.ttc",
];

// Palette translated from screen-watch-ocr-tauri's compact workbench UI.
const ACCENT_BLUE: egui::Color32 = egui::Color32::from_rgb(35, 106, 165);
const ACCENT_BLUE_DARK: egui::Color32 = egui::Color32::from_rgb(29, 95, 153);
const ACCENT_BLUE_SOFT: egui::Color32 = egui::Color32::from_rgb(214, 232, 248);
const APP_BG: egui::Color32 = egui::Color32::from_rgb(238, 241, 245);
const PANEL_BG: egui::Color32 = egui::Color32::from_rgb(248, 249, 251);
const CARD_BG: egui::Color32 = egui::Color32::from_rgb(255, 255, 255);
const CONTROL_BG: egui::Color32 = egui::Color32::from_rgb(243, 245, 248);
const HEADER_BG: egui::Color32 = egui::Color32::from_rgb(238, 241, 245);
const BORDER: egui::Color32 = egui::Color32::from_rgb(200, 206, 216);
const BORDER_STRONG: egui::Color32 = egui::Color32::from_rgb(185, 192, 202);
const TEXT_PRIMARY: egui::Color32 = egui::Color32::from_rgb(20, 23, 31);
const TEXT_SECONDARY: egui::Color32 = egui::Color32::from_rgb(91, 101, 116);
const TEXT_MUTED: egui::Color32 = egui::Color32::from_rgb(141, 150, 165);
const START_GREEN: egui::Color32 = egui::Color32::from_rgb(60, 146, 87);
const START_GREEN_DARK: egui::Color32 = egui::Color32::from_rgb(42, 118, 71);
const STOP_RED: egui::Color32 = egui::Color32::from_rgb(173, 73, 56);
const STOP_RED_DARK: egui::Color32 = egui::Color32::from_rgb(138, 58, 45);
const STOP_RED_SOFT: egui::Color32 = egui::Color32::from_rgb(249, 232, 228);
const WARNING_ORANGE: egui::Color32 = egui::Color32::from_rgb(191, 98, 32);

fn create_window_icon(is_running: bool) -> egui::IconData {
    let rgba = crate::icon::render_icon_rgba_unbadged(is_running);
    egui::IconData {
        width: crate::icon::ICON_SIZE as u32,
        height: crate::icon::ICON_SIZE as u32,
        rgba,
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

/// Configure a clean light visual theme for the desktop tool.
fn setup_visuals(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::light();

    visuals.override_text_color = Some(TEXT_PRIMARY);

    visuals.widgets.noninteractive.bg_fill = PANEL_BG;
    visuals.widgets.noninteractive.weak_bg_fill = PANEL_BG;
    visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, TEXT_SECONDARY);

    visuals.widgets.inactive.bg_fill = CARD_BG;
    visuals.widgets.inactive.weak_bg_fill = CONTROL_BG;
    visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, TEXT_PRIMARY);
    visuals.widgets.inactive.rounding = egui::Rounding::same(3.0);

    visuals.widgets.hovered.bg_fill = ACCENT_BLUE_SOFT;
    visuals.widgets.hovered.weak_bg_fill = egui::Color32::from_rgb(231, 239, 248);
    visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, ACCENT_BLUE);
    visuals.widgets.hovered.rounding = egui::Rounding::same(3.0);

    visuals.widgets.active.bg_fill = ACCENT_BLUE;
    visuals.widgets.active.weak_bg_fill = ACCENT_BLUE;
    visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0, CARD_BG);
    visuals.widgets.active.rounding = egui::Rounding::same(3.0);

    visuals.selection.bg_fill = ACCENT_BLUE;
    visuals.selection.stroke = egui::Stroke::new(1.0, ACCENT_BLUE);

    visuals.panel_fill = APP_BG;
    visuals.window_fill = CARD_BG;
    visuals.faint_bg_color = CONTROL_BG;
    visuals.extreme_bg_color = CARD_BG;

    ctx.set_visuals(visuals);

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(12.0, 5.0);
    style.spacing.interact_size = egui::vec2(42.0, 28.0);
    ctx.set_style(style);
}

fn settings_panel_frame() -> egui::Frame {
    egui::Frame::none()
        .fill(CARD_BG)
        .stroke(egui::Stroke::new(
            1.0,
            egui::Color32::from_rgb(216, 221, 229),
        ))
        .rounding(egui::Rounding::same(3.0))
        .inner_margin(egui::Margin::symmetric(10.0, 9.0))
}

fn subtle_button(text: &str) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(text).color(TEXT_PRIMARY))
        .min_size(egui::vec2(0.0, 30.0))
        .rounding(egui::Rounding::same(4.0))
        .fill(CONTROL_BG)
        .stroke(egui::Stroke::new(1.0, BORDER_STRONG))
}

fn primary_button(text: &str) -> egui::Button<'static> {
    egui::Button::new(
        egui::RichText::new(text)
            .strong()
            .color(egui::Color32::WHITE),
    )
    .min_size(egui::vec2(0.0, 32.0))
    .rounding(egui::Rounding::same(4.0))
    .fill(ACCENT_BLUE)
    .stroke(egui::Stroke::new(1.0, ACCENT_BLUE_DARK))
}

fn danger_button(text: &str) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(text).color(STOP_RED_DARK))
        .min_size(egui::vec2(0.0, 30.0))
        .rounding(egui::Rounding::same(4.0))
        .fill(STOP_RED_SOFT)
        .stroke(egui::Stroke::new(1.0, STOP_RED_DARK))
}

fn section_title(ui: &mut egui::Ui, title: &str) {
    ui.horizontal(|ui| {
        ui.painter().rect_filled(
            egui::Rect::from_min_size(ui.cursor().min, egui::vec2(3.0, 16.0)),
            egui::Rounding::same(2.0),
            ACCENT_BLUE,
        );
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(title)
                .size(14.0)
                .strong()
                .color(TEXT_PRIMARY),
        );
    });
}

fn status_chip(ui: &mut egui::Ui, text: &str, text_color: egui::Color32, fill: egui::Color32) {
    egui::Frame::none()
        .fill(fill)
        .stroke(egui::Stroke::new(1.0, BORDER))
        .rounding(egui::Rounding::same(3.0))
        .inner_margin(egui::Margin::symmetric(8.0, 3.0))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(text)
                    .size(12.0)
                    .strong()
                    .color(text_color),
            );
        });
}

fn soft_section_break(ui: &mut egui::Ui) {
    ui.add_space(8.0);
    ui.separator();
    ui.add_space(7.0);
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
                        crate::logging::log_event(
                            "single_instance",
                            "activation received; showing main viewport",
                        );
                        show_main_window(&context);
                        thread::sleep(Duration::from_millis(20));
                    }
                }
            })
            .ok();
        Self { stop, worker }
    }
}

fn show_main_window(context: &egui::Context) {
    crate::window::restore_own_main_window();
    context.send_viewport_cmd(egui::ViewportCommand::Visible(true));
    context.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
    context.send_viewport_cmd(egui::ViewportCommand::Focus);
    context.request_repaint();
}

impl Drop for ActivationWake {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct TrayIconStateSync {
    hwnd: isize,
    notify_id: Option<u32>,
    hicon: Option<isize>,
    last_state: Option<(bool, String)>,
    last_refresh: Instant,
}

impl TrayIconStateSync {
    fn new(hwnd: isize) -> Self {
        Self {
            hwnd,
            notify_id: None,
            hicon: None,
            last_state: None,
            last_refresh: Instant::now() - TRAY_ICON_SYNC_INTERVAL,
        }
    }

    fn refresh(&mut self, is_running: bool, config_name: &str) {
        let state = (is_running, config_name.to_owned());
        let state_changed = self.last_state.as_ref() != Some(&state);
        if !state_changed && self.last_refresh.elapsed() < TRAY_ICON_SYNC_INTERVAL {
            return;
        }

        if state_changed || self.hicon.is_none() {
            let rgba = crate::icon::render_icon_rgba(is_running, config_name);
            let Some(hicon) =
                crate::taskbar::create_hicon_from_rgba(crate::icon::ICON_SIZE as u32, &rgba)
            else {
                return;
            };
            if let Some(old) = self.hicon.replace(hicon) {
                unsafe {
                    let _ = DestroyIcon(HICON(old as *mut _));
                }
            }
        }

        let Some(hicon) = self.hicon else {
            return;
        };
        let status = if is_running { "运行中" } else { "已停止" };
        let tooltip = format!("{} - {config_name} - {status}", crate::obfstr!("调度器"));
        if let Some(id) = update_shell_tray_icon(self.hwnd, self.notify_id, hicon, &tooltip) {
            self.notify_id = Some(id);
        }
        self.last_state = Some(state);
        self.last_refresh = Instant::now();
    }
}

impl Drop for TrayIconStateSync {
    fn drop(&mut self) {
        if let Some(hicon) = self.hicon.take() {
            unsafe {
                let _ = DestroyIcon(HICON(hicon as *mut _));
            }
        }
    }
}

fn update_shell_tray_icon(
    hwnd: isize,
    cached_id: Option<u32>,
    hicon: isize,
    tooltip: &str,
) -> Option<u32> {
    if let Some(id) = cached_id {
        if update_shell_tray_icon_with_id(hwnd, id, hicon, tooltip) {
            return Some(id);
        }
    }

    (1..=TRAY_NOTIFY_ID_SEARCH_LIMIT)
        .filter(|id| Some(*id) != cached_id)
        .find(|id| update_shell_tray_icon_with_id(hwnd, *id, hicon, tooltip))
}

fn update_shell_tray_icon_with_id(
    hwnd: isize,
    notify_id: u32,
    hicon: isize,
    tooltip: &str,
) -> bool {
    let mut data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: HWND(hwnd as *mut _),
        uID: notify_id,
        uFlags: NIF_ICON | NIF_TIP,
        hIcon: HICON(hicon as *mut _),
        ..Default::default()
    };

    let tooltip: Vec<u16> = tooltip.encode_utf16().chain(Some(0)).collect();
    for (target, source) in data.szTip.iter_mut().zip(tooltip) {
        *target = source;
    }

    unsafe { Shell_NotifyIconW(NIM_MODIFY, &data).as_bool() }
}

/// Background thread that polls tray events while the main viewport is hidden.
struct TrayExitWatcher {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl TrayExitWatcher {
    fn spawn(
        egui_ctx: egui::Context,
        exit_requested: Arc<AtomicBool>,
        tray_window: Option<isize>,
        is_running: Arc<AtomicBool>,
        preferences: Arc<RwLock<AppPreferences>>,
        status: Arc<RwLock<String>>,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker = thread::Builder::new()
            .name(crate::stealth::random_thread_name())
            .spawn(move || {
                use tray_icon::menu::MenuEvent;
                use tray_icon::TrayIconEvent;

                let mut tray_icon_sync = tray_window.map(TrayIconStateSync::new);
                while !worker_stop.load(Ordering::Acquire) {
                    thread::sleep(Duration::from_millis(50));

                    if let Some(sync) = &mut tray_icon_sync {
                        let running = is_running.load(Ordering::Acquire);
                        let name = preferences.read().selected_config.clone();
                        sync.refresh(running, &name);
                    }

                    // Poll tray icon events (double-click to show)
                    while let Ok(event) = TrayIconEvent::receiver().try_recv() {
                        if matches!(event, TrayIconEvent::DoubleClick { .. }) {
                            show_main_window(&egui_ctx);
                        }
                    }

                    // Poll menu events — handle all events directly
                    while let Ok(event) = MenuEvent::receiver().try_recv() {
                        let id_str = event.id.as_ref();
                        if id_str == "exit" {
                            exit_requested.store(true, Ordering::Release);
                            egui_ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            egui_ctx.request_repaint();
                        } else if id_str == "show" {
                            show_main_window(&egui_ctx);
                        } else if id_str == "autostart" {
                            let enabled = !crate::autostart::is_enabled();
                            match crate::autostart::set_enabled(enabled) {
                                Ok(verified) => {
                                    *status.write() = if verified.enabled {
                                        format!(
                                            "开机自启已启用 ({:?}): {}",
                                            verified.backend,
                                            verified.link_path.display()
                                        )
                                    } else {
                                        "开机自启已关闭并验证".to_owned()
                                    };
                                    crate::logging::log_event(
                                        "autostart",
                                        &format!(
                                            "enabled={} backend={:?} link={} target={} args={} working_dir={}",
                                            verified.enabled,
                                            verified.backend,
                                            verified.link_path.display(),
                                            verified.target_path.display(),
                                            verified.arguments,
                                            verified.working_dir.display()
                                        ),
                                    );
                                }
                                Err(error) => {
                                    crate::logging::log_error("set_autostart", &error);
                                    *status.write() = format!("开机自启设置失败: {error}");
                                }
                            }
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
    last_icon_attempt: Option<((bool, String), Instant)>,
    last_saved_config: Config,
    last_saved_preferences: AppPreferences,
    last_autosave: Instant,
    last_config_switch: Instant,
    last_hook_alt_toggle: Instant,
    alt_fallback_down: bool,
    alt_fallback_solo: bool,
    taskbar: TaskbarDecoration,
    autostart_hide_at: Option<Instant>,
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
        started_by_autostart: bool,
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
        let tray_window = tray.as_ref().map(TrayController::window_handle);
        let tray_exit_watcher = TrayExitWatcher::spawn(
            egui_context.clone(),
            exit_requested.clone(),
            tray_window,
            is_running.clone(),
            preferences.clone(),
            status.clone(),
        );
        let autostart_hide_at = (started_by_autostart && tray.is_some())
            .then(|| Instant::now() + Duration::from_millis(750));
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
            last_icon_attempt: None,
            last_saved_config,
            last_saved_preferences,
            last_autosave: Instant::now(),
            last_config_switch: Instant::now() - Duration::from_secs(10),
            last_hook_alt_toggle: Instant::now() - Duration::from_secs(10),
            alt_fallback_down: false,
            alt_fallback_solo: false,
            taskbar: TaskbarDecoration::new(),
            autostart_hide_at,
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
                self.last_saved_config = self.config.read().clone();
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
            .exact_height(58.0)
            .frame(
                egui::Frame::none()
                    .fill(HEADER_BG)
                    .inner_margin(egui::Margin::symmetric(12.0, 10.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(
                            egui::RichText::new(crate::obfstr!("调度器"))
                                .size(18.0)
                                .strong()
                                .color(TEXT_PRIMARY),
                        );
                        ui.label(
                            egui::RichText::new(crate::obfstr!("Windows 按键调度器"))
                                .size(11.0)
                                .color(TEXT_SECONDARY),
                        );
                    });

                    let right_size = egui::vec2(ui.available_width(), 36.0);
                    ui.allocate_ui_with_layout(
                        right_size,
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            let running = self.is_running.load(Ordering::Acquire);
                            let button = if running {
                                egui::Button::new(
                                    egui::RichText::new("停止")
                                        .strong()
                                        .color(egui::Color32::WHITE),
                                )
                                .min_size(egui::vec2(82.0, 32.0))
                                .rounding(egui::Rounding::same(4.0))
                                .fill(STOP_RED)
                                .stroke(egui::Stroke::new(1.0, STOP_RED_DARK))
                            } else {
                                primary_button("启动").min_size(egui::vec2(82.0, 32.0))
                            };
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
                            status_chip(
                                ui,
                                if running { "运行中" } else { "已停止" },
                                if running {
                                    START_GREEN_DARK
                                } else {
                                    TEXT_SECONDARY
                                },
                                if running {
                                    egui::Color32::from_rgb(230, 244, 236)
                                } else {
                                    CONTROL_BG
                                },
                            );
                        },
                    );
                });
            });
    }

    fn render_settings(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("settings")
            .resizable(false)
            .show_separator_line(false)
            .exact_width(286.0)
            .frame(
                egui::Frame::none()
                    .fill(APP_BG)
                    .inner_margin(egui::Margin::symmetric(10.0, 0.0)),
            )
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                    .show(ui, |ui| {
                        ui.add_space(10.0);
                        settings_panel_frame().show(ui, |ui| {
                            section_title(ui, "运行设置");
                            ui.add_space(6.0);
                            self.render_run_settings(ui);

                            soft_section_break(ui);
                            section_title(ui, "发送目标");
                            ui.add_space(6.0);
                            self.render_target_settings(ui);

                            soft_section_break(ui);
                            section_title(ui, "配置档");
                            ui.add_space(6.0);
                            self.render_profile_settings(ui);
                        });
                        ui.add_space(10.0);
                    });
            });
    }

    fn render_run_settings(&mut self, ui: &mut egui::Ui) {
        let mut config = self.config.write();

        ui.checkbox(&mut config.independent_loop, "\u{72ec}\u{7acb}\u{5faa}\u{73af}\u{6a21}\u{5f0f}")
            .on_hover_text("\u{5f00}\u{542f}\u{540e}\u{6bcf}\u{4e2a}\u{6309}\u{952e}\u{72ec}\u{7acb}\u{5faa}\u{73af}\u{ff0c}\u{5173}\u{95ed}\u{540e}\u{6309}\u{987a}\u{5e8f}\u{9010}\u{952e}\u{6267}\u{884c}");

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("\u{6700}\u{5927}\u{5faa}\u{73af}\u{6b21}\u{6570}:")
                    .color(TEXT_SECONDARY),
            );
            ui.add(
                egui::DragValue::new(&mut config.max_loops)
                    .range(-1..=1_000_000)
                    .speed(1),
            )
            .on_hover_cursor(egui::CursorIcon::Default);
            ui.label(egui::RichText::new("(-1 \u{4e3a}\u{65e0}\u{9650})").color(TEXT_MUTED));
        });

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("\u{5168}\u{5c40}\u{968f}\u{673a}\u{5ef6}\u{8fdf}:")
                    .color(TEXT_SECONDARY),
            );
            ui.add_sized(
                [50.0, 28.0],
                egui::DragValue::new(&mut config.global_random_delay)
                    .range(0..=5000)
                    .speed(10),
            )
            .on_hover_cursor(egui::CursorIcon::Default);
            ui.label(egui::RichText::new("ms").color(TEXT_MUTED));
        });

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("\u{65f6}\u{5e8f}\u{53d8}\u{5316}\u{7ea7}\u{522b}:")
                    .color(TEXT_SECONDARY),
            );
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
                egui::Frame::none()
                    .fill(CONTROL_BG)
                    .stroke(egui::Stroke::new(1.0, BORDER))
                    .rounding(egui::Rounding::same(3.0))
                    .inner_margin(egui::Margin::same(7.0))
                    .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new(format!(
                                "\u{5df2}\u{7ed1}\u{5b9a}: {}",
                                if title.is_empty() {
                                    format!("{hwnd:#x}")
                                } else {
                                    title
                                }
                            ))
                            .color(TEXT_PRIMARY),
                        );
                    });
                if !valid {
                    ui.colored_label(STOP_RED, "\u{7a97}\u{53e3}\u{5df2}\u{5931}\u{6548}");
                }
                if ui
                    .add(subtle_button("\u{89e3}\u{9664}\u{7ed1}\u{5b9a}"))
                    .clicked()
                {
                    *self.bound_window.write() = None;
                    *self.status.write() =
                        "\u{5df2}\u{89e3}\u{9664}\u{7a97}\u{53e3}\u{7ed1}\u{5b9a}".to_owned();
                }
            }
            None => {
                ui.label(
                    egui::RichText::new(
                        "\u{672a}\u{7ed1}\u{5b9a}\u{7a97}\u{53e3} (\u{4e0d}\u{4f1a}\u{53d1}\u{9001}\u{6309}\u{952e})",
                    )
                    .color(TEXT_SECONDARY),
                );
                if ui
                    .add(subtle_button("\u{9009}\u{62e9}\u{7a97}\u{53e3}..."))
                    .clicked()
                {
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
                                if ui.add(subtle_button(&window.title)).clicked() {
                                    *self.bound_window.write() = Some(window.hwnd);
                                    *self.status.write() =
                                        format!("\u{5df2}\u{7ed1}\u{5b9a}: {}", window.title);
                                    self.show_window_selector = false;
                                }
                            }
                        });
                    if ui.add(subtle_button("\u{53d6}\u{6d88}")).clicked() {
                        self.show_window_selector = false;
                    }
                });
        }

        ui.add_space(4.0);
    }

    fn render_profile_settings(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("\u{540d}\u{79f0}:").color(TEXT_SECONDARY));
            ui.text_edit_singleline(&mut self.profile_name_edit);
        });

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui.add(subtle_button("\u{4fdd}\u{5b58}")).clicked() {
                let name = self.profile_name_edit.clone();
                match save_named_config(&name, &self.config) {
                    Ok(saved_name) => {
                        self.preferences.write().selected_config = saved_name.clone();
                        self.profile_name_edit = saved_name;
                        self.last_saved_config = self.config.read().clone();
                        *self.status.write() =
                            "\u{914d}\u{7f6e}\u{5df2}\u{4fdd}\u{5b58}".to_owned();
                        self.refresh_profiles_and_hotkeys();
                    }
                    Err(error) => {
                        *self.status.write() = format!("\u{4fdd}\u{5b58}\u{5931}\u{8d25}: {error}");
                    }
                }
            }
            if ui.add(subtle_button("\u{52a0}\u{8f7d}")).clicked() {
                let name = self.profile_name_edit.clone();
                self.save_now();
                match load_into(&name, &self.config) {
                    Ok(()) => {
                        self.preferences.write().selected_config = name.clone();
                        self.last_saved_config = self.config.read().clone();
                        *self.status.write() =
                            format!("\u{5df2}\u{52a0}\u{8f7d}\u{914d}\u{7f6e} [{name}]");
                        self.refresh_profiles_and_hotkeys();
                    }
                    Err(error) => {
                        *self.status.write() = format!("\u{52a0}\u{8f7d}\u{5931}\u{8d25}: {error}");
                    }
                }
            }
            if ui.add(danger_button("\u{5220}\u{9664}")).clicked() {
                let name = self.profile_name_edit.clone();
                self.save_now();
                match delete_named_config(&name) {
                    Ok(()) => {
                        if self.preferences.read().selected_config == name {
                            self.preferences.write().selected_config =
                                DEFAULT_CONFIG_NAME.to_owned();
                            self.profile_name_edit = DEFAULT_CONFIG_NAME.to_owned();
                            if load_into(DEFAULT_CONFIG_NAME, &self.config).is_ok() {
                                self.last_saved_config = self.config.read().clone();
                            }
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
            ui.label(
                egui::RichText::new("\u{5207}\u{6362}\u{914d}\u{7f6e}:").color(TEXT_SECONDARY),
            );
            let mut selected = self.preferences.read().selected_config.clone();
            egui::ComboBox::from_id_salt("config_selector")
                .width(150.0)
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
                    self.last_saved_config = self.config.read().clone();
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
                    .fill(APP_BG)
                    .inner_margin(egui::Margin::symmetric(10.0, 0.0)),
            )
            .show(ctx, |ui| {
                ui.add_space(10.0);
                let panel_min_height = ui.available_height().max(0.0);
                ui.scope(|ui| {
                    ui.set_min_height((panel_min_height - 20.0).max(0.0));
                    let enabled_count = self
                        .config
                        .read()
                        .keys
                        .iter()
                        .filter(|key| key.enabled)
                        .count();

                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label(
                                egui::RichText::new("按键调度")
                                    .size(16.0)
                                    .strong()
                                    .color(TEXT_PRIMARY),
                            );
                            ui.label(
                                egui::RichText::new(format!(
                                    "{KEY_SLOT_COUNT} 个按键槽 / {enabled_count} 个已启用"
                                ))
                                .size(12.0)
                                .color(TEXT_SECONDARY),
                            );
                        });

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.add(subtle_button("反选")).clicked() {
                                for key in &mut self.config.write().keys {
                                    key.enabled = !key.enabled;
                                }
                            }
                            if ui.add(subtle_button("全选")).clicked() {
                                for key in &mut self.config.write().keys {
                                    key.enabled = true;
                                }
                            }
                        });
                    });

                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(6.0);

                    ui.label(
                        egui::RichText::new("按键、延迟和启用状态")
                            .size(12.0)
                            .strong()
                            .color(TEXT_SECONDARY),
                    );
                    ui.add_space(6.0);

                    egui::ScrollArea::both().show(ui, |ui| {
                        // Calculate column widths to fill available space
                        let available = ui.available_width();
                        let col_num = 36.0; // # column
                        let col_key = 132.0; // 按键 column
                        let col_enabled = 54.0; // 启用 column
                        let col_status = 48.0; // 状态 column
                        let spacing = 12.0 * 5.0; // 5 gaps between 6 columns
                        let remaining =
                            (available - col_num - col_key - col_enabled - col_status - spacing)
                                .max(120.0);
                        let col_delay = remaining / 2.0; // 基础延迟, auto-expand
                        let col_range = remaining / 2.0; // 随机范围, auto-expand

                        egui::Grid::new("key_table")
                            .spacing(egui::vec2(12.0, 7.0))
                            .show(ui, |ui| {
                                // Header row
                                ui.label(
                                    egui::RichText::new("#")
                                        .strong()
                                        .size(12.0)
                                        .color(TEXT_SECONDARY),
                                );
                                ui.label(
                                    egui::RichText::new("按键")
                                        .strong()
                                        .size(12.0)
                                        .color(TEXT_SECONDARY),
                                );
                                ui.label(
                                    egui::RichText::new("基础延迟(ms)")
                                        .strong()
                                        .size(12.0)
                                        .color(TEXT_SECONDARY),
                                );
                                ui.label(
                                    egui::RichText::new("随机范围(ms)")
                                        .strong()
                                        .size(12.0)
                                        .color(TEXT_SECONDARY),
                                );
                                ui.label(
                                    egui::RichText::new("启用")
                                        .strong()
                                        .size(12.0)
                                        .color(TEXT_SECONDARY),
                                );
                                ui.label(
                                    egui::RichText::new("状态")
                                        .strong()
                                        .size(12.0)
                                        .color(TEXT_SECONDARY),
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
                                                .color(TEXT_SECONDARY),
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
                                        .rounding(egui::Rounding::same(3.0))
                                        .fill(if is_capturing {
                                            ACCENT_BLUE_SOFT
                                        } else {
                                            CARD_BG
                                        })
                                        .stroke(egui::Stroke::new(
                                            1.0,
                                            if is_capturing {
                                                ACCENT_BLUE
                                            } else {
                                                BORDER_STRONG
                                            },
                                        ));
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
                                        )
                                        .on_hover_cursor(egui::CursorIcon::Default);
                                        if base_delay != key.base_delay {
                                            edits[index].base_delay = Some(base_delay);
                                        }

                                        let mut random_range = key.random_range;
                                        ui.add_sized(
                                            [col_range, 28.0],
                                            egui::DragValue::new(&mut random_range)
                                                .range(0..=MAX_DELAY_MS)
                                                .speed(10),
                                        )
                                        .on_hover_cursor(egui::CursorIcon::Default);
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
                                                    .color(START_GREEN),
                                            );
                                        } else {
                                            ui.label(
                                                egui::RichText::new("\u{25cb}")
                                                    .size(14.0)
                                                    .color(TEXT_MUTED),
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
            });
    }

    fn render_status_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("status_bar")
            .exact_height(28.0)
            .frame(
                egui::Frame::none()
                    .fill(PANEL_BG)
                    .stroke(egui::Stroke::new(1.0, BORDER))
                    .inner_margin(egui::Margin::symmetric(12.0, 4.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let status = self.status.read().clone();
                    ui.label(egui::RichText::new(status).size(12.0).color(TEXT_SECONDARY));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if !self.hooks_available {
                            status_chip(
                                ui,
                                "全局快捷键不可用",
                                WARNING_ORANGE,
                                egui::Color32::from_rgb(255, 244, 232),
                            );
                        }
                    });
                });
            });
    }
}

impl eframe::App for AutoKeyApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        APP_BG.to_normalized_gamma_f32()
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        setup_visuals(ctx);

        if self
            .autostart_hide_at
            .is_some_and(|hide_at| Instant::now() >= hide_at)
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            self.autostart_hide_at = None;
            crate::logging::log_event("autostart", "main viewport hidden after first render");
        }

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

        // Update window and taskbar icons when either running state or config changes.
        if self.taskbar.enabled() {
            let running = self.is_running.load(Ordering::Acquire);
            let name = self.preferences.read().selected_config.clone();
            let state = (running, name.clone());
            let needs_refresh = self.last_icon_state.as_ref() != Some(&state);
            let retry_ready = self
                .last_icon_attempt
                .as_ref()
                .map(|(attempted_state, attempted_at)| {
                    attempted_state != &state
                        || attempted_at.elapsed() >= TASKBAR_ICON_RETRY_INTERVAL
                })
                .unwrap_or(true);

            if needs_refresh && retry_ready {
                self.last_icon_attempt = Some((state.clone(), Instant::now()));
                let icon = create_window_icon(running);
                ctx.send_viewport_cmd(egui::ViewportCommand::Icon(Some(Arc::new(icon))));
                if self.taskbar.update(running, &name) {
                    self.last_icon_state = Some(state);
                }
            }
        }

        self.autosave_if_changed();

        // Clicking X → cancel close and hide window to tray
        if ctx.input(|i| i.viewport().close_requested()) {
            if self.tray.is_some() && !self.exit_requested.load(Ordering::Acquire) {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.persist_window_state(ctx);
                self.save_now();
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            } else {
                self.persist_window_state(ctx);
                self.save_now();
            }
        }

        self.render_header(ctx);
        self.render_settings(ctx);
        self.render_key_table(ctx);
        self.render_status_bar(ctx);

        let repaint_after =
            if self.is_running.load(Ordering::Acquire) || self.capturing_key.is_some() {
                Duration::from_millis(200)
            } else {
                Duration::from_millis(750)
            };
        ctx.request_repaint_after(repaint_after);
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
    use_glow_renderer: bool,
    started_by_autostart: bool,
) -> Result<(), eframe::Error> {
    let title = format!(
        "{} - {}",
        crate::obfstr!("调度器"),
        preferences.read().selected_config
    );
    let (width, height, pos_x, pos_y) = {
        let prefs = preferences.read();
        (
            prefs.window_width,
            prefs.window_height,
            prefs.window_x,
            prefs.window_y,
        )
    };

    let icon = Arc::new(create_window_icon(false));

    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([width, height.min(700.0)])
        .with_title(title)
        .with_app_id(crate::APP_USER_MODEL_ID)
        .with_icon(icon);

    // Restore window position if previously saved
    if pos_x.is_finite() && pos_y.is_finite() {
        viewport = viewport.with_position([pos_x, pos_y]);
    }
    let options = eframe::NativeOptions {
        viewport,
        renderer: if use_glow_renderer {
            eframe::Renderer::Glow
        } else {
            eframe::Renderer::Wgpu
        },
        wgpu_options: eframe::egui_wgpu::WgpuConfiguration {
            supported_backends: eframe::wgpu::Backends::DX12 | eframe::wgpu::Backends::VULKAN,
            power_preference: eframe::wgpu::PowerPreference::LowPower,
            ..Default::default()
        },
        ..Default::default()
    };

    eframe::run_native(
        &crate::obfstr!("调度器"),
        options,
        Box::new(move |cc| {
            if let Some(render_state) = &cc.wgpu_render_state {
                let adapter = render_state.adapter.get_info();
                crate::logging::log_event(
                    "renderer",
                    &format!(
                        "active=wgpu backend={:?} adapter={} device={:?} driver={} driver_info={}",
                        adapter.backend,
                        adapter.name,
                        adapter.device_type,
                        adapter.driver,
                        adapter.driver_info
                    ),
                );
            } else {
                crate::logging::log_event("renderer", "active=glow");
            }
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
                started_by_autostart,
                cc.egui_ctx.clone(),
            );
            Ok(Box::new(app))
        }),
    )
}
