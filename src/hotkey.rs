//! Parses hotkey text such as `Alt+Shift+F10` into RegisterHotKey arguments.

use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hotkey {
    pub mods: HOT_KEY_MODIFIERS,
    pub vk: u32,
}

pub fn parse(text: &str) -> Result<Hotkey, String> {
    let mut mods = MOD_NOREPEAT;
    let mut vk = None;
    for part in text.split('+').map(str::trim).filter(|p| !p.is_empty()) {
        let up = part.to_ascii_uppercase();
        match up.as_str() {
            "CTRL" | "CONTROL" => mods |= MOD_CONTROL,
            "ALT" => mods |= MOD_ALT,
            "SHIFT" => mods |= MOD_SHIFT,
            "WIN" | "WINDOWS" | "SUPER" => mods |= MOD_WIN,
            _ => {
                if vk.is_some() {
                    return Err(format!("\"{text}\" has more than one key"));
                }
                vk = Some(key_code(&up).ok_or_else(|| format!("unknown key \"{part}\""))?);
            }
        }
    }
    let vk = vk.ok_or_else(|| format!("\"{text}\" has no key"))?;
    Ok(Hotkey { mods, vk })
}

fn key_code(k: &str) -> Option<u32> {
    if let Some(n) = k.strip_prefix('F').and_then(|n| n.parse::<u32>().ok()) {
        return (1..=24).contains(&n).then_some(0x70 + n - 1);
    }
    if k.len() == 1 {
        let c = k.as_bytes()[0];
        if c.is_ascii_uppercase() || c.is_ascii_digit() {
            return Some(u32::from(c));
        }
    }
    if let Some(n) = k.strip_prefix("NUM").and_then(|n| n.parse::<u32>().ok()) {
        return (n <= 9).then_some(0x60 + n);
    }
    Some(match k {
        "PRINTSCREEN" | "PRTSC" => 0x2c,
        "SCROLLLOCK" => 0x91,
        "PAUSE" => 0x13,
        "INSERT" | "INS" => 0x2d,
        "DELETE" | "DEL" => 0x2e,
        "HOME" => 0x24,
        "END" => 0x23,
        "PAGEUP" | "PGUP" => 0x21,
        "PAGEDOWN" | "PGDN" => 0x22,
        "SPACE" => 0x20,
        "TAB" => 0x09,
        "ENTER" | "RETURN" => 0x0d,
        "BACKSPACE" => 0x08,
        "UP" => 0x26,
        "DOWN" => 0x28,
        "LEFT" => 0x25,
        "RIGHT" => 0x27,
        "`" | "TILDE" => 0xc0,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_keys() {
        let h = parse("Alt+F10").unwrap();
        assert_eq!(h.vk, 0x79);
        assert_eq!(h.mods, MOD_ALT | MOD_NOREPEAT);
        let h = parse("ctrl + shift + s").unwrap();
        assert_eq!(h.vk, u32::from(b'S'));
        assert_eq!(h.mods, MOD_CONTROL | MOD_SHIFT | MOD_NOREPEAT);
        assert!(parse("Alt").is_err());
        assert!(parse("Alt+F25").is_err());
        assert!(parse("A+B").is_err());
    }
}
