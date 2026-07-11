pub const VK_SHIFT: u16 = 0x10;
pub const VK_CONTROL: u16 = 0x11;
pub const VK_ALT: u16 = 0x12;
pub const VK_WIN: u16 = 0x5B;

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
