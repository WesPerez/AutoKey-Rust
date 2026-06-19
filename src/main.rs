#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod engine;
mod gui;
mod hook;
mod hotkey;
mod humanizer;
mod icon;
mod input;
mod logging;
mod single_instance;
mod stealth;
mod tray;
mod window;

use parking_lot::RwLock;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use windows::core::PCWSTR;
use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};

pub const APP_USER_MODEL_ID: &str = "WesPerez.SysDispatcher";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppCommand {
    Start,
    Stop,
    Exit,
    ToggleRunning,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiAction {
    CapturedKey(u16),
}

struct AppState {
    is_running: Arc<AtomicBool>,
    key_running: Arc<RwLock<Vec<bool>>>,
    config: Arc<RwLock<config::Config>>,
    preferences: Arc<RwLock<config::AppPreferences>>,
    bound_window: Arc<RwLock<Option<isize>>>,
    status: Arc<RwLock<String>>,
}

impl AppState {
    fn new(config: config::Config, preferences: config::AppPreferences, status: String) -> Self {
        Self {
            is_running: Arc::new(AtomicBool::new(false)),
            key_running: Arc::new(RwLock::new(vec![false; config::KEY_SLOT_COUNT])),
            config: Arc::new(RwLock::new(config)),
            preferences: Arc::new(RwLock::new(preferences)),
            bound_window: Arc::new(RwLock::new(None)),
            status: Arc::new(RwLock::new(status)),
        }
    }
}

fn main() {
    if let Err(error) = run() {
        logging::log_error("fatal", &error);
        show_fatal_error(&error.to_string());
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    // Early anti-detection initialization
    stealth::init();
    configure_taskbar_identity();

    config::initialize_store()?;

    let instance = match single_instance::SingleInstance::try_acquire() {
        Ok(Some(instance)) => instance,
        Ok(None) => return Ok(()),
        Err(error) => {
            logging::log_error("single_instance", &error);
            return Err(error.into());
        }
    };

    let mut preferences = config::load_preferences();
    let (initial_config, initial_status) =
        match config::load_named_config(&preferences.selected_config) {
            Ok(config) => (
                config,
                format!("配置 [{}] 已加载", preferences.selected_config),
            ),
            Err(error) => {
                logging::log_error("load_initial_config", &error);
                preferences.selected_config = config::DEFAULT_CONFIG_NAME.to_owned();
                let fallback = config::load_named_config(config::DEFAULT_CONFIG_NAME);
                match fallback {
                    Ok(config) => (config, format!("无法加载配置, 已切换到默认 {error}")),
                    Err(inner) => {
                        logging::log_error("load_default_config", &inner);
                        (
                            config::Config::default(),
                            format!("无法加载任何配置: {inner}"),
                        )
                    }
                }
            }
        };

    let state = Arc::new(AppState::new(initial_config, preferences, initial_status));
    let (command_tx, command_rx) = std::sync::mpsc::channel::<AppCommand>();
    let (ui_tx, ui_rx) = std::sync::mpsc::channel::<UiAction>();

    humanizer::init();
    let mut engine = engine::AutomationEngine::spawn(
        command_rx,
        state.config.clone(),
        state.is_running.clone(),
        state.key_running.clone(),
        state.bound_window.clone(),
        state.status.clone(),
    )?;

    let hooks = match hook::GlobalHooks::install(
        command_tx.clone(),
        ui_tx,
        state.bound_window.clone(),
        state.status.clone(),
    ) {
        Ok(hooks) => Some(hooks),
        Err(error) => {
            logging::log_error("global_hooks", &error);
            *state.status.write() = format!("全局快捷键不可用: {error}");
            None
        }
    };

    let gui_result = gui::run_gui(
        state.config.clone(),
        state.preferences.clone(),
        command_tx.clone(),
        ui_rx,
        state.is_running.clone(),
        state.key_running.clone(),
        state.bound_window.clone(),
        state.status.clone(),
        instance.activation_handle(),
        hooks.is_some(),
    );

    let _ = command_tx.send(AppCommand::Exit);
    drop(hooks);
    engine.join();

    let preferences = state.preferences.read().clone();
    if let Err(error) = config::save_named_config(&preferences.selected_config, &state.config) {
        logging::log_error("save_config_on_exit", &error);
    }
    if let Err(error) = config::save_preferences(&preferences) {
        logging::log_error("save_preferences_on_exit", &error);
    }

    gui_result?;
    Ok(())
}

fn configure_taskbar_identity() {
    let app_id: Vec<u16> = APP_USER_MODEL_ID.encode_utf16().chain(Some(0)).collect();
    unsafe {
        let _ = SetCurrentProcessExplicitAppUserModelID(PCWSTR(app_id.as_ptr()));
    }
}

fn show_fatal_error(message: &str) {
    let title: Vec<u16> = crate::obfstr!("启动失败").encode_utf16().chain(Some(0)).collect();
    let message: Vec<u16> = format!("{message}\0").encode_utf16().collect();
    // SAFETY: Both UTF-16 strings are NUL-terminated for the duration of the call.
    unsafe {
        let _ = MessageBoxW(
            None,
            PCWSTR(message.as_ptr()),
            PCWSTR(title.as_ptr()),
            MB_OK | MB_ICONERROR,
        );
    }
}
