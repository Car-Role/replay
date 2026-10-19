//! Maps parsed hotkeys (Windows key codes) to global-hotkey key codes.

use global_hotkey::hotkey::{Code, HotKey, Modifiers};

use crate::hotkey::Hotkey;

fn code(vk: u32) -> Option<Code> {
    use Code::*;
    const LETTERS: [Code; 26] = [
        KeyA, KeyB, KeyC, KeyD, KeyE, KeyF, KeyG, KeyH, KeyI, KeyJ, KeyK, KeyL, KeyM, KeyN, KeyO, KeyP, KeyQ, KeyR, KeyS,
        KeyT, KeyU, KeyV, KeyW, KeyX, KeyY, KeyZ,
    ];
    const DIGITS: [Code; 10] = [Digit0, Digit1, Digit2, Digit3, Digit4, Digit5, Digit6, Digit7, Digit8, Digit9];
    const NUMPAD: [Code; 10] = [Numpad0, Numpad1, Numpad2, Numpad3, Numpad4, Numpad5, Numpad6, Numpad7, Numpad8, Numpad9];
    const FKEYS: [Code; 24] = [
        F1, F2, F3, F4, F5, F6, F7, F8, F9, F10, F11, F12, F13, F14, F15, F16, F17, F18, F19, F20, F21, F22, F23, F24,
    ];
    Some(match vk {
        0x41..=0x5a => LETTERS[(vk - 0x41) as usize],
        0x30..=0x39 => DIGITS[(vk - 0x30) as usize],
        0x60..=0x69 => NUMPAD[(vk - 0x60) as usize],
        0x70..=0x87 => FKEYS[(vk - 0x70) as usize],
        0x2c => PrintScreen,
        0x91 => ScrollLock,
        0x13 => Pause,
        0x2d => Insert,
        0x2e => Delete,
        0x24 => Home,
        0x23 => End,
        0x21 => PageUp,
        0x22 => PageDown,
        0x20 => Space,
        0x09 => Tab,
        0x0d => Enter,
        0x08 => Backspace,
        0x26 => ArrowUp,
        0x28 => ArrowDown,
        0x25 => ArrowLeft,
        0x27 => ArrowRight,
        0xc0 => Backquote,
        _ => return None,
    })
}

pub fn to_global(h: &Hotkey) -> Option<HotKey> {
    let mut m = Modifiers::empty();
    if h.ctrl {
        m |= Modifiers::CONTROL;
    }
    if h.alt {
        m |= Modifiers::ALT;
    }
    if h.shift {
        m |= Modifiers::SHIFT;
    }
    if h.win {
        m |= Modifiers::META;
    }
    Some(HotKey::new(Some(m), code(h.vk)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_default_hotkeys() {
        let h = crate::hotkey::parse("Alt+Shift+F10").unwrap();
        let g = to_global(&h).unwrap();
        assert_eq!(g.key, Code::F10);
        assert_eq!(g.mods, Modifiers::ALT | Modifiers::SHIFT);
    }
}
