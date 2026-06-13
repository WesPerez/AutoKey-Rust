use crate::config::{
    delete_named_config, list_config_names, load_into, save_named_config, AppPreferences, Config,
    DEFAULT_CONFIG_NAME, KEY_SLOT_COUNT, MAX_DELAY_MS, MIN_DELAY_MS,
};
use crate::hook::GlobalHooks;
use crate::hotkey::key_display_name;
use crate::single_instance::SingleInstance;
use crate::tray::{TrayAction, TrayController};
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

const AUTOSAVE_INTERVAL: Duration = Duration::from_millis(750);

const CHINESE_FONT_CANDIDATES: &[&str] = &[
    r"C:\Windows\Fonts\NotoSansSC-VF.ttf",
    r"C:\Windows\Fonts\MiSans-Normal.ttf",
    r"C:\Windows\Fonts\msyh.ttf",
    r"C:\Windows\Fonts\msyh.ttc",
    r"C:\Windows\Fonts\simhei.ttf",
    r"C:\Windows\Fonts\simsun.ttc",
];

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HotkeyCaptureTarget {
    CycleConfig,
    CurrentConfig,
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
            .name("autokey-activation-wake".to_owned())
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
    capturing_hotkey: Option<HotkeyCaptureTarget>,
    tray: Option<TrayController>,
    really_closing: bool,
    hide_requested: bool,
    last_saved_config: Config,
    last_saved_preferences: AppPreferences,
    last_autosave: Instant,
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
        let tray = if std::env::var_os("AUTOKEY_DISABLE_TRAY").is_some() {
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
            capturing_hotkey: None,
            tray,
            really_closing: false,
            hide_requested: false,
            last_saved_config,
            last_saved_preferences,
            last_autosave: Instant::now(),
        };
        app.refresh_profiles_and_hotkeys();
        app
    }

    fn refresh_profiles_and_hotkeys(&mut self) {
        self.profile_names = list_config_names().unwrap_or_default();
        let preferences = self.preferences.read();
        let cycle = preferences.cycle_config_hotkey.clone();
        let config = self.config.read();
        let profile_hotkeys: Vec<(String, String)> = self
            .profile_names
            .iter()
            .filter_map(|name| {
                if *name == preferences.selected_config {
                    Some((name.clone(), config.config_hotkey.clone()))
                } else {
                    None
                }
            })
            .collect();
        GlobalHooks::update_hotkeys(&cycle, &profile_hotkeys);
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
            }
            self.last_saved_config = current_config;
        }

        let current_preferences = self.preferences.read().clone();
        if current_preferences != self.last_saved_preferences {
            if let Err(error) = crate::config::save_preferences(&current_preferences) {
                *self.status.write() =
                    format!("\u{4fdd}\u{5b58}\u{504f}\u{597d}\u{5931}\u{8d25}: {error}");
            }
            self.last_saved_preferences = current_preferences;
        }
    }

    fn persist_window_size(&mut self, ctx: &egui::Context) {
        let Some(rect) = ctx.input(|input| input.viewport().inner_rect) else {
            return;
        };
        let size = rect.size();
        if !size.x.is_finite() || !size.y.is_finite() || size.x <= 0.0 || size.y <= 0.0 {
            return;
        }

        let preferences = {
            let mut preferences = self.preferences.write();
            preferences.window_width = size.x;
            preferences.window_height = size.y;
            preferences.sanitize();
            preferences.clone()
        };

        if let Err(error) = crate::config::save_preferences(&preferences) {
            crate::logging::log_error("save_window_size", &error);
            *self.status.write() = format!("保存窗口尺寸失败: {error}");
        } else {
            self.last_saved_preferences = preferences;
        }
    }

    fn process_ui_actions(&mut self) {
        while let Ok(action) = self.ui_rx.try_recv() {
            match action {
                UiAction::NextConfig => {
                    if let Some(pos) = self
                        .profile_names
                        .iter()
                        .position(|n| *n == self.preferences.read().selected_config)
                    {
                        let next = (pos + 1) % self.profile_names.len();
                        let name = self.profile_names[next].clone();
                        if let Err(error) = load_into(&name, &self.config) {
                            *self.status.write() = format!(
                                "\u{5207}\u{6362}\u{914d}\u{7f6e}\u{5931}\u{8d25}: {error}"
                            );
                        } else {
                            self.preferences.write().selected_config = name.clone();
                            self.profile_name_edit = name;
                            self.refresh_profiles_and_hotkeys();
                        }
                    }
                }
                UiAction::LoadConfig(name) => {
                    if let Err(error) = load_into(&name, &self.config) {
                        *self.status.write() =
                            format!("\u{52a0}\u{8f7d}\u{914d}\u{7f6e}\u{5931}\u{8d25}: {error}");
                    } else {
                        self.preferences.write().selected_config = name.clone();
                        self.profile_name_edit = name;
                        self.refresh_profiles_and_hotkeys();
                    }
                }
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
                UiAction::CapturedHotkey(text) => {
                    if let Some(target) = self.capturing_hotkey.take() {
                        match target {
                            HotkeyCaptureTarget::CycleConfig => {
                                self.preferences.write().cycle_config_hotkey = text;
                            }
                            HotkeyCaptureTarget::CurrentConfig => {
                                self.config.write().config_hotkey = text;
                            }
                        }
                        self.refresh_profiles_and_hotkeys();
                    }
                    GlobalHooks::cancel_capture();
                }
            }
        }
    }

    fn render_header(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("header")
            .exact_height(60.0)
            .show(ctx, |ui| {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(egui::RichText::new("AutoKey").size(22.0).strong());
                        ui.label(
                            egui::RichText::new("Windows \u{6309}\u{952e}\u{8c03}\u{5ea6}\u{5668}")
                                .size(12.0)
                                .color(ui.visuals().weak_text_color()),
                        );
                    });

                    let right_size = egui::vec2(ui.available_width(), 40.0);
                    ui.allocate_ui_with_layout(
                        right_size,
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            let running = self.is_running.load(Ordering::Acquire);
                            let button = egui::Button::new(if running {
                                "\u{505c}\u{6b62}"
                            } else {
                                "\u{542f}\u{52a8}"
                            })
                            .min_size(egui::vec2(72.0, 32.0))
                            .fill(if running {
                                egui::Color32::from_rgb(171, 64, 64)
                            } else {
                                egui::Color32::from_rgb(49, 126, 95)
                            });
                            if ui
                                .add(button)
                                .on_hover_text(
                                    "\u{4e5f}\u{53ef}\u{4ee5}\u{5355}\u{72ec}\u{6309}\u{4e0b}\u{5de6} Alt \u{5207}\u{6362}\u{8fd0}\u{884c}\u{72b6}\u{6001}",
                                )
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
                                    "\u{8fd0}\u{884c}\u{4e2d}"
                                } else {
                                    "\u{5df2}\u{505c}\u{6b62}"
                                })
                                .color(if running {
                                    egui::Color32::from_rgb(76, 176, 128)
                                } else {
                                    ui.visuals().weak_text_color()
                                }),
                            );
                        },
                    );
                });
                ui.add_space(4.0);
            });
    }

    fn render_settings(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("settings")
            .resizable(false)
            .exact_width(300.0)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.add_space(12.0);
                    ui.heading("\u{8fd0}\u{884c}\u{8bbe}\u{7f6e}");
                    ui.add_space(8.0);
                    self.render_run_settings(ui);

                    ui.add_space(16.0);
                    ui.separator();
                    ui.add_space(12.0);
                    ui.heading("\u{53d1}\u{9001}\u{76ee}\u{6807}");
                    ui.add_space(8.0);
                    self.render_target_settings(ui);

                    ui.add_space(16.0);
                    ui.separator();
                    ui.add_space(12.0);
                    ui.heading("\u{914d}\u{7f6e}\u{6863}");
                    ui.add_space(8.0);
                    self.render_profile_settings(ui);
                    ui.add_space(12.0);
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
        ui.label("\u{63d0}\u{793a}: Ctrl+Alt+Space \u{7ed1}\u{5b9a}\u{5149}\u{6807}\u{4e0b}\u{7a97}\u{53e3}\u{ff0c}\u{53f3}\u{952e}\u{62d6}\u{62fd}\u{4e5f}\u{53ef}\u{7ed1}\u{5b9a}");
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
        ui.label("\u{5feb}\u{6377}\u{952e}:");

        let config_hotkey = self.config.read().config_hotkey.clone();
        let cycle_hotkey = self.preferences.read().cycle_config_hotkey.clone();

        ui.horizontal(|ui| {
            ui.label("\u{5f53}\u{524d}\u{914d}\u{7f6e}:");
            let capturing = self.capturing_hotkey == Some(HotkeyCaptureTarget::CurrentConfig);
            let button_text = if capturing {
                "\u{6309} Esc \u{53d6}\u{6d88}..."
            } else if config_hotkey.is_empty() {
                "\u{70b9}\u{51fb}\u{6355}\u{83b7}"
            } else {
                &config_hotkey
            };
            if ui.button(button_text).clicked() {
                if capturing {
                    self.capturing_hotkey = None;
                    GlobalHooks::cancel_capture();
                } else {
                    self.capturing_hotkey = Some(HotkeyCaptureTarget::CurrentConfig);
                    GlobalHooks::begin_hotkey_capture();
                }
            }
            if !config_hotkey.is_empty() && ui.button("\u{6e05}\u{9664}").clicked() {
                self.config.write().config_hotkey.clear();
                self.refresh_profiles_and_hotkeys();
            }
        });

        ui.horizontal(|ui| {
            ui.label("\u{5faa}\u{73af}\u{914d}\u{7f6e}:");
            let capturing = self.capturing_hotkey == Some(HotkeyCaptureTarget::CycleConfig);
            let button_text = if capturing {
                "\u{6309} Esc \u{53d6}\u{6d88}..."
            } else if cycle_hotkey.is_empty() {
                "\u{70b9}\u{51fb}\u{6355}\u{83b7}"
            } else {
                &cycle_hotkey
            };
            if ui.button(button_text).clicked() {
                if capturing {
                    self.capturing_hotkey = None;
                    GlobalHooks::cancel_capture();
                } else {
                    self.capturing_hotkey = Some(HotkeyCaptureTarget::CycleConfig);
                    GlobalHooks::begin_hotkey_capture();
                }
            }
            if !cycle_hotkey.is_empty() && ui.button("\u{6e05}\u{9664}").clicked() {
                self.preferences.write().cycle_config_hotkey.clear();
                self.refresh_profiles_and_hotkeys();
            }
        });
    }

    fn render_key_table(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("\u{5168}\u{9009}").clicked() {
                        for key in &mut self.config.write().keys {
                            key.enabled = true;
                        }
                    }
                    if ui.button("\u{53cd}\u{9009}").clicked() {
                        for key in &mut self.config.write().keys {
                            key.enabled = !key.enabled;
                        }
                    }
                });
                ui.add_space(6.0);

                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("#").strong());
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new("\u{6309}\u{952e}").strong());
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new("\u{57fa}\u{7840}\u{5ef6}\u{8fdf}(ms)").strong());
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new("\u{968f}\u{673a}\u{8303}\u{56f4}(ms)").strong());
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new("\u{542f}\u{7528}").strong());
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new("\u{72b6}\u{6001}").strong());
                });
                ui.separator();

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
                        let is_active = key_running.get(index).copied().unwrap_or(false);

                        ui.horizontal(|ui| {
                            ui.label(format!("{}", index + 1));
                            ui.add_space(8.0);

                            let button_text = if is_capturing {
                                "\u{6309}\u{4efb}\u{610f}\u{952e}..."
                            } else {
                                &key.key_name
                            };
                            let button =
                                egui::Button::new(button_text).min_size(egui::vec2(100.0, 24.0));
                            if ui.add(button).clicked() {
                                if is_capturing {
                                    self.capturing_key = None;
                                    GlobalHooks::cancel_capture();
                                } else {
                                    self.capturing_key = Some(index);
                                    GlobalHooks::begin_key_capture();
                                }
                            }
                            ui.add_space(8.0);

                            let mut base_delay = key.base_delay;
                            ui.add(
                                egui::DragValue::new(&mut base_delay)
                                    .range(MIN_DELAY_MS..=MAX_DELAY_MS)
                                    .speed(10),
                            );
                            if base_delay != key.base_delay {
                                edits[index].base_delay = Some(base_delay);
                            }
                            ui.add_space(8.0);

                            let mut random_range = key.random_range;
                            ui.add(
                                egui::DragValue::new(&mut random_range)
                                    .range(0..=MAX_DELAY_MS)
                                    .speed(10),
                            );
                            if random_range != key.random_range {
                                edits[index].random_range = Some(random_range);
                            }
                            ui.add_space(8.0);

                            let mut enabled = key.enabled;
                            ui.checkbox(&mut enabled, "");
                            if enabled != key.enabled {
                                edits[index].enabled = Some(enabled);
                            }
                            ui.add_space(8.0);

                            if is_active {
                                ui.label(
                                    egui::RichText::new("\u{25cf}")
                                        .color(egui::Color32::from_rgb(76, 176, 128)),
                                );
                            } else {
                                ui.label(
                                    egui::RichText::new("\u{25cb}")
                                        .color(ui.visuals().weak_text_color()),
                                );
                            }
                        });
                    }
                }

                if edits.iter().any(|e| {
                    e.base_delay.is_some() || e.random_range.is_some() || e.enabled.is_some()
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
    }

    fn render_status_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("status_bar")
            .exact_height(28.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let status = self.status.read().clone();
                    ui.label(status);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if !self.hooks_available {
                            ui.colored_label(
                                egui::Color32::from_rgb(220, 160, 40),
                                "\u{5168}\u{5c40}\u{5feb}\u{6377}\u{952e}\u{4e0d}\u{53ef}\u{7528}",
                            );
                        }
                    });
                });
            });
    }
}

impl eframe::App for AutoKeyApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if ctx.theme() != egui::Theme::Dark {
            ctx.set_theme(egui::Theme::Dark);
        }

        self.process_ui_actions();

        if let Some(tray) = &self.tray {
            match tray.poll() {
                TrayAction::Show => {
                    crate::window::restore_own_main_window();
                }
                TrayAction::Exit => {
                    self.really_closing = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                TrayAction::None => {}
            }
        }

        if let Some(tray) = &mut self.tray {
            let running = self.is_running.load(Ordering::Acquire);
            let name = self.preferences.read().selected_config.clone();
            tray.update(running, &name);
        }

        self.autosave_if_changed();

        if ctx.input(|i| i.viewport().close_requested()) {
            self.persist_window_size(ctx);
            if self.tray.is_some() && !self.really_closing {
                self.hide_requested = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            }
        }

        if self.hide_requested {
            self.hide_requested = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }

        self.render_header(ctx);
        self.render_settings(ctx);
        self.render_key_table(ctx);
        self.render_status_bar(ctx);

        if self.is_running.load(Ordering::Acquire) {
            ctx.request_repaint_after(Duration::from_millis(100));
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
    let title = format!("AutoKey - {}", preferences.read().selected_config);
    let (width, height) = {
        let prefs = preferences.read();
        (prefs.window_width, prefs.window_height)
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([width, height])
            .with_title(title),
        ..Default::default()
    };

    eframe::run_native(
        "AutoKey",
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
