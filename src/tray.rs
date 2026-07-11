use anyhow::{Context, Result};
use tray_icon::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

pub struct TrayController {
    tray: TrayIcon,
    autostart_item: CheckMenuItem,
    last_state: Option<(bool, String)>,
}

impl TrayController {
    pub fn new() -> Result<Self> {
        let menu = Menu::new();
        let show_item = MenuItem::with_id("show", "显示", true, None);
        let autostart_item = CheckMenuItem::with_id(
            "autostart",
            "开机自启",
            true,
            crate::autostart::is_enabled(),
            None,
        );
        let separator = PredefinedMenuItem::separator();
        let exit_item = MenuItem::with_id("exit", "退出", true, None);
        menu.append_items(&[&show_item, &autostart_item, &separator, &exit_item])?;

        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .with_tooltip(&crate::obfstr!("调度器 - 已停止"))
            .with_icon(create_icon(false, crate::config::DEFAULT_CONFIG_NAME)?)
            .build()
            .context("无法创建系统托盘图标")?;

        Ok(Self {
            tray,
            autostart_item,
            last_state: None,
        })
    }

    pub fn poll(&self) {
        self.autostart_item
            .set_checked(crate::autostart::is_enabled());
    }

    pub fn window_handle(&self) -> isize {
        self.tray.window_handle() as isize
    }

    pub fn update(&mut self, is_running: bool, config_name: &str) {
        let state = (is_running, config_name.to_owned());
        if self.last_state.as_ref() == Some(&state) {
            return;
        }

        let status = if is_running { "运行中" } else { "已停止" };
        let tooltip = format!("{} - {config_name} - {status}", crate::obfstr!("调度器"));
        let _ = self.tray.set_tooltip(Some(tooltip));
        if let Ok(icon) = create_icon(is_running, config_name) {
            let _ = self.tray.set_icon(Some(icon));
        }
        self.last_state = Some(state);
    }
}

fn create_icon(is_running: bool, config_name: &str) -> Result<Icon> {
    let rgba = crate::icon::render_icon_rgba(is_running, config_name);

    Icon::from_rgba(
        rgba,
        crate::icon::ICON_SIZE as u32,
        crate::icon::ICON_SIZE as u32,
    )
    .context("无法创建托盘图标")
}
