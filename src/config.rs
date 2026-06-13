use anyhow::{bail, Context, Result};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use windows::core::PCWSTR;

use crate::obfstr;
use windows::Win32::Storage::FileSystem::{
    MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
};

pub const KEY_SLOT_COUNT: usize = 12;
pub const MIN_DELAY_MS: u32 = 20;
pub const MAX_DELAY_MS: u32 = 3_600_000;
pub const DEFAULT_CONFIG_NAME: &str = "默认";
pub const DEFAULT_CYCLE_HOTKEY: &str = "Ctrl+Z";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeyConfig {
    #[serde(alias = "IsEnabled")]
    pub enabled: bool,
    #[serde(alias = "KeyCode")]
    pub vk_code: u16,
    #[serde(alias = "KeyName")]
    pub key_name: String,
    #[serde(alias = "Delay")]
    pub base_delay: u32,
    #[serde(alias = "RandomDelay")]
    pub random_range: u32,
}

impl Default for KeyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            vk_code: 0,
            key_name: "未设置".to_owned(),
            base_delay: 1000,
            random_range: 100,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    #[serde(alias = "Keys")]
    pub keys: Vec<KeyConfig>,
    #[serde(alias = "IndependentLoop")]
    pub independent_loop: bool,
    #[serde(alias = "GlobalRandomDelay")]
    pub global_random_delay: u32,
    pub max_loops: i32,
    #[serde(
        alias = "anti_pattern_level",
        alias = "AntiPatternLevel",
        rename = "timing_variation_level"
    )]
    pub timing_variation_level: u8,
    #[serde(alias = "ConfigHotkey")]
    pub config_hotkey: String,
}

impl Default for Config {
    fn default() -> Self {
        let mut keys = vec![KeyConfig::default(); KEY_SLOT_COUNT];
        for key in keys.iter_mut().take(4) {
            key.enabled = true;
        }

        Self {
            keys,
            independent_loop: true,
            global_random_delay: 100,
            max_loops: -1,
            timing_variation_level: 2,
            config_hotkey: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppPreferences {
    #[serde(alias = "SelectedConfig")]
    pub selected_config: String,
    #[serde(alias = "CycleConfigHotkey")]
    pub cycle_config_hotkey: String,
    pub window_width: f32,
    pub window_height: f32,
}

impl Default for AppPreferences {
    fn default() -> Self {
        Self {
            selected_config: DEFAULT_CONFIG_NAME.to_owned(),
            cycle_config_hotkey: DEFAULT_CYCLE_HOTKEY.to_owned(),
            window_width: 1040.0,
            window_height: 700.0,
        }
    }
}

impl AppPreferences {
    pub fn sanitize(&mut self) {
        self.selected_config = sanitize_config_name(&self.selected_config);
        self.cycle_config_hotkey = self.cycle_config_hotkey.trim().chars().take(64).collect();
        if !self.window_width.is_finite() {
            self.window_width = 1040.0;
        }
        if !self.window_height.is_finite() {
            self.window_height = 700.0;
        }
        self.window_width = self.window_width.clamp(820.0, 7680.0);
        self.window_height = self.window_height.clamp(560.0, 4320.0);
    }
}

#[derive(Debug, Deserialize)]
#[serde(default)]
struct LegacyConfig {
    #[serde(rename = "Keys")]
    keys: Vec<KeyConfig>,
    #[serde(rename = "IndependentLoop")]
    independent_loop: bool,
    #[serde(rename = "GlobalRandomDelay")]
    global_random_delay: u32,
    #[serde(rename = "LoopMode")]
    loop_mode: String,
    #[serde(rename = "ConfigHotkey")]
    config_hotkey: String,
    #[serde(rename = "AntiPatternLevel")]
    timing_variation_level: u8,
}

impl Default for LegacyConfig {
    fn default() -> Self {
        Self {
            keys: Vec::new(),
            independent_loop: true,
            global_random_delay: 100,
            loop_mode: "循环到手动停止".to_owned(),
            config_hotkey: String::new(),
            timing_variation_level: 2,
        }
    }
}

impl Config {
    pub fn sanitize(&mut self) {
        self.keys.truncate(KEY_SLOT_COUNT);
        while self.keys.len() < KEY_SLOT_COUNT {
            self.keys.push(KeyConfig::default());
        }

        for key in &mut self.keys {
            key.key_name = key.key_name.trim().chars().take(24).collect();
            if key.key_name.is_empty() {
                key.key_name = "未设置".to_owned();
            }
            if !(1..=254).contains(&key.vk_code) {
                key.vk_code = 0;
            }
            key.base_delay = key.base_delay.clamp(MIN_DELAY_MS, MAX_DELAY_MS);
            key.random_range = key.random_range.min(MAX_DELAY_MS);
        }

        self.global_random_delay = self.global_random_delay.min(MAX_DELAY_MS);
        self.max_loops = self.max_loops.clamp(-1, 1_000_000);
        self.timing_variation_level = self.timing_variation_level.min(2);
        self.config_hotkey = self.config_hotkey.trim().chars().take(64).collect();
    }

    pub fn validation_error(&self) -> Option<String> {
        if !self
            .keys
            .iter()
            .any(|key| key.enabled && (1..=254).contains(&key.vk_code))
        {
            return Some("请至少设置并启用一个按键".to_owned());
        }

        self.keys.iter().enumerate().find_map(|(index, key)| {
            (key.enabled && !(1..=254).contains(&key.vk_code))
                .then(|| format!("第 {} 行已启用，但尚未设置按键", index + 1))
        })
    }
}

pub fn initialize_store() -> Result<()> {
    fs::create_dir_all(config_directory())?;
    migrate_old_csharp_configs()?;
    migrate_old_app_state()?;

    let default_path = named_config_path(DEFAULT_CONFIG_NAME);
    if !default_path.exists() {
        let previous_single = app_directory().join("config.json");
        let initial = if previous_single.exists() {
            read_config_file(&previous_single).unwrap_or_default()
        } else {
            Config::default()
        };
        write_json_atomic(&default_path, &initial)?;
    }
    Ok(())
}

pub fn load_preferences() -> AppPreferences {
    let mut preferences: AppPreferences = read_json(&preferences_path()).unwrap_or_default();
    preferences.sanitize();
    preferences
}

pub fn save_preferences(preferences: &AppPreferences) -> Result<()> {
    let mut preferences = preferences.clone();
    preferences.sanitize();
    write_json_atomic(&preferences_path(), &preferences)
}

pub fn list_config_names() -> Result<Vec<String>> {
    let mut names = BTreeSet::new();
    names.insert(DEFAULT_CONFIG_NAME.to_owned());

    for entry in fs::read_dir(config_directory())? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) == Some("json") {
            if let Some(name) = path.file_stem().and_then(|value| value.to_str()) {
                names.insert(name.to_owned());
            }
        }
    }

    let mut result: Vec<String> = names.into_iter().collect();
    result.sort_by_key(|name| (name != DEFAULT_CONFIG_NAME, name.to_lowercase()));
    Ok(result)
}

pub fn load_named_config(name: &str) -> Result<Config> {
    let name = sanitize_config_name(name);
    let path = named_config_path(&name);
    if !path.exists() {
        bail!("配置 [{name}] 不存在");
    }
    read_config_file(&path)
}

pub fn save_named_config(name: &str, config: &Arc<RwLock<Config>>) -> Result<String> {
    let name = sanitize_config_name(name);
    let snapshot = {
        let mut config = config.write();
        config.sanitize();
        config.clone()
    };
    write_json_atomic(&named_config_path(&name), &snapshot)?;
    Ok(name)
}

pub fn delete_named_config(name: &str) -> Result<()> {
    let name = sanitize_config_name(name);
    if name == DEFAULT_CONFIG_NAME {
        bail!("默认配置不能删除");
    }

    let path = named_config_path(&name);
    if !path.exists() {
        bail!("配置 [{name}] 不存在");
    }
    fs::remove_file(&path).with_context(|| format!("无法删除 {}", path.display()))
}

pub fn load_into(name: &str, config: &Arc<RwLock<Config>>) -> Result<()> {
    *config.write() = load_named_config(name)?;
    Ok(())
}

pub fn sanitize_config_name(name: &str) -> String {
    let mut result = String::with_capacity(name.len());
    for character in name.trim().chars().take(64) {
        if character.is_control() || r#"<>:"/\|?*"#.contains(character) {
            result.push('_');
        } else {
            result.push(character);
        }
    }
    let result = result.trim_end_matches([' ', '.']).to_owned();
    if result.is_empty() || result == "." || result == ".." {
        DEFAULT_CONFIG_NAME.to_owned()
    } else {
        let device_name = result
            .split('.')
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        let reserved = matches!(
            device_name.as_str(),
            "CON"
                | "PRN"
                | "AUX"
                | "NUL"
                | "COM1"
                | "COM2"
                | "COM3"
                | "COM4"
                | "COM5"
                | "COM6"
                | "COM7"
                | "COM8"
                | "COM9"
                | "LPT1"
                | "LPT2"
                | "LPT3"
                | "LPT4"
                | "LPT5"
                | "LPT6"
                | "LPT7"
                | "LPT8"
                | "LPT9"
        );
        if reserved {
            format!("_{result}")
        } else {
            result
        }
    }
}

pub fn app_directory() -> PathBuf {
    app_data_directory().join(obfstr!("KeyScheduler"))
}

pub fn config_directory() -> PathBuf {
    app_directory().join("configs")
}

pub fn preferences_path() -> PathBuf {
    app_directory().join("app-state.json")
}

fn app_data_directory() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn named_config_path(name: &str) -> PathBuf {
    config_directory().join(format!("{}.json", sanitize_config_name(name)))
}

fn read_config_file(path: &Path) -> Result<Config> {
    let content =
        fs::read_to_string(path).with_context(|| format!("无法读取 {}", path.display()))?;
    let value: serde_json::Value = serde_json::from_str(&content)
        .with_context(|| format!("配置文件格式无效: {}", path.display()))?;

    let mut config = if value.get("Keys").is_some() {
        let legacy: LegacyConfig = serde_json::from_value(value)?;
        Config {
            keys: legacy.keys,
            independent_loop: legacy.independent_loop,
            global_random_delay: legacy.global_random_delay,
            max_loops: parse_legacy_loop_mode(&legacy.loop_mode),
            timing_variation_level: legacy.timing_variation_level,
            config_hotkey: legacy.config_hotkey,
        }
    } else {
        serde_json::from_value(value)?
    };
    config.sanitize();
    Ok(config)
}

fn parse_legacy_loop_mode(value: &str) -> i32 {
    if value.contains("手动停止") {
        return -1;
    }
    value
        .chars()
        .filter(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .unwrap_or(-1)
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let content =
        fs::read_to_string(path).with_context(|| format!("无法读取 {}", path.display()))?;
    serde_json::from_str(&content).with_context(|| format!("JSON 格式无效: {}", path.display()))
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let content = serde_json::to_vec_pretty(value)?;
    let temp_path = path.with_extension(format!("{}.tmp", fastrand::u64(..)));
    let mut file = fs::File::create(&temp_path)
        .with_context(|| format!("无法创建临时文件 {}", temp_path.display()))?;
    file.write_all(&content)?;
    file.sync_all()?;
    drop(file);

    let temp_wide: Vec<u16> = temp_path.as_os_str().encode_wide().chain(Some(0)).collect();
    let path_wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: Both UTF-16 buffers are NUL-terminated and valid for the call.
    let result = unsafe {
        MoveFileExW(
            PCWSTR(temp_wide.as_ptr()),
            PCWSTR(path_wide.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if let Err(error) = result {
        let _ = fs::remove_file(&temp_path);
        return Err(error).with_context(|| format!("无法写入 {}", path.display()));
    }
    Ok(())
}

fn migrate_old_csharp_configs() -> Result<()> {
    let old_directory = app_data_directory().join("AutoKey").join("configs");
    if !old_directory.exists() {
        return Ok(());
    }

    for entry in fs::read_dir(old_directory)? {
        let entry = entry?;
        let source = entry.path();
        if source.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let Some(name) = source.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        let destination = named_config_path(name);
        if destination.exists() {
            continue;
        }
        if let Ok(config) = read_config_file(&source) {
            write_json_atomic(&destination, &config)?;
        }
    }
    Ok(())
}

fn migrate_old_app_state() -> Result<()> {
    let destination = preferences_path();
    if destination.exists() {
        return Ok(());
    }

    let old_directory = app_data_directory().join("AutoKey");
    if !old_directory.exists() {
        return Ok(());
    }

    let old_state = old_directory.join("app-state.json");
    let mut preferences: AppPreferences = read_json(&old_state).unwrap_or_default();
    preferences.sanitize();

    let old_config = old_directory
        .join("configs")
        .join(format!("{}.json", preferences.selected_config));
    if let Ok(content) = fs::read_to_string(old_config) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(width) = value.get("WindowWidth").and_then(serde_json::Value::as_f64) {
                preferences.window_width = width as f32;
            }
            if let Some(height) = value
                .get("WindowHeight")
                .and_then(serde_json::Value::as_f64)
            {
                preferences.window_height = height as f32;
            }
            preferences.sanitize();
        }
    }

    write_json_atomic(&destination, &preferences)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_invalid_values_without_disabling_rows() {
        let mut config = Config {
            keys: vec![KeyConfig {
                enabled: true,
                vk_code: 0,
                key_name: "  ".to_owned(),
                base_delay: 1,
                random_range: u32::MAX,
            }],
            global_random_delay: u32::MAX,
            max_loops: i32::MAX,
            timing_variation_level: 9,
            independent_loop: true,
            config_hotkey: String::new(),
        };

        config.sanitize();

        assert_eq!(config.keys.len(), KEY_SLOT_COUNT);
        assert!(config.keys[0].enabled);
        assert_eq!(config.keys[0].base_delay, MIN_DELAY_MS);
        assert_eq!(config.keys[0].random_range, MAX_DELAY_MS);
        assert_eq!(config.timing_variation_level, 2);
        assert_eq!(config.max_loops, 1_000_000);
        assert!(config.validation_error().is_some());
    }

    #[test]
    fn parses_legacy_loop_modes() {
        assert_eq!(parse_legacy_loop_mode("循环到手动停止"), -1);
        assert_eq!(parse_legacy_loop_mode("循环10次"), 10);
    }

    #[test]
    fn sanitizes_profile_names() {
        assert_eq!(sanitize_config_name(" demo:*? "), "demo___");
        assert_eq!(sanitize_config_name("   "), DEFAULT_CONFIG_NAME);
        assert_eq!(sanitize_config_name("demo... "), "demo");
        assert_eq!(sanitize_config_name("CON"), "_CON");
        assert_eq!(sanitize_config_name("lpt1.notes"), "_lpt1.notes");
    }

    #[test]
    fn sanitizes_preferences() {
        let mut preferences = AppPreferences {
            selected_config: " demo? ".to_owned(),
            cycle_config_hotkey: "  Ctrl+1  ".to_owned(),
            window_width: f32::NAN,
            window_height: 10.0,
        };
        preferences.sanitize();
        assert_eq!(preferences.selected_config, "demo_");
        assert_eq!(preferences.cycle_config_hotkey, "Ctrl+1");
        assert_eq!(preferences.window_width, 1040.0);
        assert_eq!(preferences.window_height, 560.0);
    }
}
