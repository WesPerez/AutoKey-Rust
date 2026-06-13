use anyhow::{bail, Result};
use std::collections::BTreeSet;

pub const VK_SHIFT: u16 = 0x10;
pub const VK_CONTROL: u16 = 0x11;
pub const VK_ALT: u16 = 0x12;
pub const VK_WIN: u16 = 0x5B;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hotkey {
    pub keys: BTreeSet<u16>,
}

impl Hotkey {
    pub fn parse(value: &str) -> Result<Self> {
        let mut keys = BTreeSet::new();
        for part in value
            .split('+')
            .map(str::trim)
            .filter(|part| !part.is_empty())
        {
            keys.insert(parse_part(part)?);
        }
        if keys.is_empty() {
            bail!("快捷键为空");
        }
        if keys.iter().filter(|key| !is_modifier(**key)).count() != 1 {
            bail!("快捷键必须包含且只能包含一个主键");
        }
        Ok(Self { keys })
    }

    pub fn from_keys(keys: impl IntoIterator<Item = u16>) -> Result<Self> {
        let keys: BTreeSet<u16> = keys.into_iter().map(normalize_vk).collect();
        if keys.iter().filter(|key| !is_modifier(**key)).count() != 1 {
            bail!("快捷键必须包含且只能包含一个主键");
        }
        Ok(Self { keys })
    }

    pub fn display(&self) -> String {
        let mut ordered = Vec::new();
        for modifier in [VK_CONTROL, VK_SHIFT, VK_ALT, VK_WIN] {
            if self.keys.contains(&modifier) {
                ordered.push(modifier);
            }
        }
        ordered.extend(self.keys.iter().copied().filter(|key| !is_modifier(*key)));
        ordered
            .into_iter()
            .map(hotkey_key_name)
            .collect::<Vec<_>>()
            .join("+")
    }

    pub fn matches(&self, pressed: &BTreeSet<u16>) -> bool {
        &self.keys == pressed
    }
}

pub fn normalize_vk(vk: u16) -> u16 {
    match vk {
        0xA0 | 0xA1 => VK_SHIFT,
        0xA2 | 0xA3 => VK_CONTROL,
        0xA4 | 0xA5 => VK_ALT,
        0x5C => VK_WIN,
        value => value,
    }
}

pub fn is_modifier(vk: u16) -> bool {
    matches!(normalize_vk(vk), VK_SHIFT | VK_CONTROL | VK_ALT | VK_WIN)
}

pub fn key_display_name(vk: u16) -> String {
    match normalize_vk(vk) {
        VK_CONTROL => "Ctrl".to_owned(),
        VK_SHIFT => "Shift".to_owned(),
        VK_ALT => "Alt".to_owned(),
        VK_WIN => "Win".to_owned(),
        0x08 => "退格键".to_owned(),
        0x09 => "Tab键".to_owned(),
        0x0D => "回车键".to_owned(),
        0x1B => "Esc键".to_owned(),
        0x20 => "空格键".to_owned(),
        0x21 => "PageUp键".to_owned(),
        0x22 => "PageDown键".to_owned(),
        0x23 => "End键".to_owned(),
        0x24 => "Home键".to_owned(),
        0x25 => "左光标键".to_owned(),
        0x26 => "上光标键".to_owned(),
        0x27 => "右光标键".to_owned(),
        0x28 => "下光标键".to_owned(),
        0x2D => "Insert键".to_owned(),
        0x2E => "Delete键".to_owned(),
        0x30..=0x39 | 0x41..=0x5A => char::from_u32(vk as u32)
            .map(|value| format!("{value}键"))
            .unwrap_or_else(|| format!("VK {vk}")),
        0x60..=0x69 => format!("小键盘{}", vk - 0x60),
        0x6A => "小键盘*".to_owned(),
        0x6B => "小键盘+".to_owned(),
        0x6D => "小键盘-".to_owned(),
        0x6E => "小键盘.".to_owned(),
        0x6F => "小键盘/".to_owned(),
        0x70..=0x87 => format!("F{}键", vk - 0x70 + 1),
        value => format!("VK {value}"),
    }
}

fn parse_part(part: &str) -> Result<u16> {
    let upper = part.to_ascii_uppercase();
    let value = match upper.as_str() {
        "CTRL" | "CONTROL" => VK_CONTROL,
        "SHIFT" => VK_SHIFT,
        "ALT" => VK_ALT,
        "WIN" | "WINDOWS" => VK_WIN,
        "ESC" | "ESCAPE" => 0x1B,
        "SPACE" => 0x20,
        "ENTER" | "RETURN" => 0x0D,
        "TAB" => 0x09,
        "BACKSPACE" => 0x08,
        "PAGEUP" | "PGUP" => 0x21,
        "PAGEDOWN" | "PGDN" => 0x22,
        "END" => 0x23,
        "HOME" => 0x24,
        "LEFT" => 0x25,
        "UP" => 0x26,
        "RIGHT" => 0x27,
        "DOWN" => 0x28,
        "INSERT" | "INS" => 0x2D,
        "DELETE" | "DEL" => 0x2E,
        _ if upper.len() == 1 => {
            let byte = upper.as_bytes()[0];
            if byte.is_ascii_alphanumeric() {
                byte as u16
            } else {
                bail!("无法识别按键 {part}");
            }
        }
        _ if upper.starts_with('F') => {
            let number: u16 = upper[1..]
                .parse()
                .with_context(|| format!("无法识别按键 {part}"))?;
            if !(1..=24).contains(&number) {
                bail!("功能键超出 F1..F24");
            }
            0x70 + number - 1
        }
        _ if upper.starts_with("VK") => {
            let number: u16 = upper[2..]
                .parse()
                .with_context(|| format!("无法识别按键 {part}"))?;
            if !(1..=254).contains(&number) {
                bail!("虚拟键码超出 1..=254");
            }
            number
        }
        _ => bail!("无法识别按键 {part}"),
    };
    Ok(normalize_vk(value))
}

fn hotkey_key_name(vk: u16) -> String {
    match normalize_vk(vk) {
        VK_CONTROL => "Ctrl".to_owned(),
        VK_SHIFT => "Shift".to_owned(),
        VK_ALT => "Alt".to_owned(),
        VK_WIN => "Win".to_owned(),
        0x1B => "Esc".to_owned(),
        0x20 => "Space".to_owned(),
        0x08 => "Backspace".to_owned(),
        0x09 => "Tab".to_owned(),
        0x0D => "Enter".to_owned(),
        0x21 => "PageUp".to_owned(),
        0x22 => "PageDown".to_owned(),
        0x23 => "End".to_owned(),
        0x24 => "Home".to_owned(),
        0x25 => "Left".to_owned(),
        0x26 => "Up".to_owned(),
        0x27 => "Right".to_owned(),
        0x28 => "Down".to_owned(),
        0x2D => "Insert".to_owned(),
        0x2E => "Delete".to_owned(),
        0x30..=0x39 | 0x41..=0x5A => char::from_u32(vk as u32)
            .map(|value| value.to_string())
            .unwrap_or_else(|| format!("VK{vk}")),
        0x70..=0x87 => format!("F{}", vk - 0x70 + 1),
        value => format!("VK{value}"),
    }
}

use anyhow::Context;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_normalizes_hotkeys() {
        let hotkey = Hotkey::parse("shift+ctrl+a").unwrap();
        assert_eq!(hotkey.display(), "Ctrl+Shift+A");
        assert!(hotkey.keys.contains(&0x41));
    }

    #[test]
    fn rejects_modifier_only_hotkeys() {
        assert!(Hotkey::parse("Ctrl+Shift").is_err());
    }

    #[test]
    fn captured_hotkeys_round_trip_through_text() {
        for key in [0x0D, 0x25, 0x60, 0x87, 0xBA] {
            let hotkey = Hotkey::from_keys([VK_CONTROL, key]).unwrap();
            assert_eq!(Hotkey::parse(&hotkey.display()).unwrap(), hotkey);
        }
    }
}
