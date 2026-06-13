use anyhow::{Context, Result};
use std::path::PathBuf;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};
use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};
use winreg::RegKey;

use crate::obfstr;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "{A1B2C3D4-E5F6-7890-ABCD-EF1234567890}";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    None,
    Show,
    Exit,
}

pub struct TrayController {
    tray: TrayIcon,
    show_id: MenuId,
    autostart_id: MenuId,
    exit_id: MenuId,
    autostart_item: CheckMenuItem,
    last_state: Option<(bool, String)>,
}

impl TrayController {
    pub fn new() -> Result<Self> {
        let menu = Menu::new();
        let show_item = MenuItem::new("显示", true, None);
        let autostart_item = CheckMenuItem::new("开机自启", true, is_autostart_enabled(), None);
        let separator = PredefinedMenuItem::separator();
        let exit_item = MenuItem::new("退出", true, None);
        menu.append_items(&[&show_item, &autostart_item, &separator, &exit_item])?;

        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .with_tooltip(obfstr!("调度器 - 已停止"))
            .with_icon(create_icon(false)?)
            .build()
            .context("无法创建系统托盘图标")?;

        Ok(Self {
            tray,
            show_id: show_item.id().clone(),
            autostart_id: autostart_item.id().clone(),
            exit_id: exit_item.id().clone(),
            autostart_item,
            last_state: None,
        })
    }

    pub fn poll(&self) -> TrayAction {
        while let Ok(event) = TrayIconEvent::receiver().try_recv() {
            if matches!(event, TrayIconEvent::DoubleClick { .. }) {
                return TrayAction::Show;
            }
        }

        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if event.id == self.show_id {
                return TrayAction::Show;
            }
            if event.id == self.exit_id {
                return TrayAction::Exit;
            }
            if event.id == self.autostart_id {
                let enabled = !is_autostart_enabled();
                if set_autostart(enabled).is_ok() {
                    self.autostart_item.set_checked(enabled);
                } else {
                    self.autostart_item.set_checked(is_autostart_enabled());
                }
            }
        }
        TrayAction::None
    }

    pub fn update(&mut self, is_running: bool, config_name: &str) {
        let state = (is_running, config_name.to_owned());
        if self.last_state.as_ref() == Some(&state) {
            return;
        }

        let status = if is_running { "运行中" } else { "已停止" };
        let tooltip = format!("{} - {} - {}", obfstr!("调度器"), config_name, status);
        let _ = self.tray.set_tooltip(Some(tooltip));
        if let Ok(icon) = create_icon(is_running) {
            let _ = self.tray.set_icon(Some(icon));
        }
        self.last_state = Some(state);
    }
}

fn create_icon(is_running: bool) -> Result<Icon> {
    const SIZE: usize = 32;
    let mut rgba = vec![0u8; SIZE * SIZE * 4];
    let accent = if is_running {
        [55, 180, 120, 255]
    } else {
        [203, 72, 72, 255]
    };

    for y in 0..SIZE {
        for x in 0..SIZE {
            let offset = (y * SIZE + x) * 4;
            let dx = x as f32 - 15.5;
            let dy = y as f32 - 15.5;
            let distance = (dx * dx + dy * dy).sqrt();
            let color = if distance <= 12.5 {
                accent
            } else {
                [28, 30, 34, 255]
            };
            rgba[offset..offset + 4].copy_from_slice(&color);
        }
    }
    Icon::from_rgba(rgba, SIZE as u32, SIZE as u32).context("无法创建托盘图标")
}

fn is_autostart_enabled() -> bool {
    let current_user = RegKey::predef(HKEY_CURRENT_USER);
    current_user
        .open_subkey_with_flags(RUN_KEY, KEY_READ)
        .and_then(|key| key.get_value::<String, _>(RUN_VALUE))
        .is_ok()
}

fn set_autostart(enabled: bool) -> Result<()> {
    let current_user = RegKey::predef(HKEY_CURRENT_USER);
    let (key, _) = current_user
        .create_subkey(RUN_KEY)
        .context("无法打开开机启动注册表项")?;

    if enabled {
        let executable: PathBuf = std::env::current_exe()?;
        key.set_value(RUN_VALUE, &format!("\"{}\"", executable.display()))?;
    } else {
        let _ = key.delete_value(RUN_VALUE);
    }
    Ok(())
}
