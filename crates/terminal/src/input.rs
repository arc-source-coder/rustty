//! Input normalization: GPUI events → zconpty wire structs.
//!
//! The UI layer lowers GPUI events into zconpty-owned structs here.
//!
//! Key mapping strategy:
//! Rust forwards normalized text/modifiers/native metadata.
//! zconpty owns the Windows-native key resolution details.

use gpui::WindowsNativeKey;
use zconpty::{KeyAction, KeyEvent, Mods, W3cCode, key_from_w3c};

const RIGHT_ALT_PRESSED: u32 = 0x0001;
const LEFT_CTRL_PRESSED: u32 = 0x0008;
const RIGHT_CTRL_PRESSED: u32 = 0x0004;
const CAPSLOCK_ON: u32 = 0x0080;
const NUMLOCK_ON: u32 = 0x0020;

/// Map a GPUI Keystroke.key string to a W3C-based physical key enum.
/// Returns `None` for keys we don't handle (modifier-only keys,
/// media keys, etc.).
pub(crate) fn map_key(key: &str) -> Option<W3cCode> {
    if key.len() == 1 {
        return map_single_char_key(key.as_bytes()[0]);
    }

    let lower;
    let key = if key.as_bytes().iter().any(|b| b.is_ascii_uppercase()) {
        lower = key.to_ascii_lowercase();
        lower.as_str()
    } else {
        key
    };

    let w3c = match key {
        "return" => "enter",
        "pageup" | "page_up" => "page_up",
        "pagedown" | "page_down" => "page_down",
        "left" => "arrow_left",
        "right" => "arrow_right",
        "up" => "arrow_up",
        "down" => "arrow_down",
        "enter" | "tab" | "escape" | "backspace" | "space" | "delete" | "insert" | "home"
        | "end" | "f1" | "f2" | "f3" | "f4" | "f5" | "f6" | "f7" | "f8" | "f9" | "f10" | "f11"
        | "f12" | "f13" | "f14" | "f15" | "f16" | "f17" | "f18" | "f19" | "f20" | "f21" | "f22"
        | "f23" | "f24" => key,
        _ => return None,
    };

    key_from_w3c(w3c.as_bytes())
}

pub(crate) fn pack_key_mods(
    modifiers: &gpui::Modifiers,
    native_key: Option<WindowsNativeKey>,
) -> Mods {
    let mut packed = (modifiers.shift as u16)
        | ((modifiers.control as u16) << 1)
        | ((modifiers.alt as u16) << 2)
        | ((modifiers.platform as u16) << 3);

    if let Some(native) = native_key {
        if (native.control_key_state & CAPSLOCK_ON) != 0 {
            packed |= 1 << 4;
        }
        if (native.control_key_state & NUMLOCK_ON) != 0 {
            packed |= 1 << 5;
        }
    }

    Mods(packed)
}

pub(crate) fn pack_mouse_mods(modifiers: &gpui::Modifiers) -> Mods {
    Mods(
        (modifiers.shift as u16)
            | ((modifiers.control as u16) << 1)
            | ((modifiers.alt as u16) << 2)
            | ((modifiers.platform as u16) << 3),
    )
}

pub(crate) fn normalize_key_event(
    keystroke: &gpui::Keystroke,
    native_key: Option<WindowsNativeKey>,
    action: KeyAction,
    is_held: bool,
) -> Option<KeyEvent> {
    let code = if let Some(code) = map_key(keystroke.key.as_str()) {
        code
    } else if native_key.is_some() {
        W3cCode::from_raw(0)
    } else {
        return None;
    };
    let mut text = [0u8; 32];
    let source = keystroke.key_char.as_deref().unwrap_or("");
    let bytes = source.as_bytes();
    let text_len = bytes.len().min(text.len());
    text[..text_len].copy_from_slice(&bytes[..text_len]);

    let mods = pack_key_mods(&keystroke.modifiers, native_key);
    let consumed_mods = compute_consumed_mods(native_key, text_len);

    Some(KeyEvent {
        action: normalize_key_action(action, is_held),
        mods,
        consumed_mods,
        repeat_count: 1,
        code,
        text_len: text_len as u8,
        text,
        unshifted_codepoint: compute_unshifted_codepoint(keystroke),
        composing: 0,
        has_win_vk: native_key.is_some() as u8,
        win_vk: native_key.map_or(0, |native| native.virtual_key),
        has_win_scan: native_key.is_some() as u8,
        win_scan: native_key.map_or(0, |native| native.scan_code),
        has_win_control_key_state: native_key.is_some() as u8,
        win_control_key_state: native_key.map_or(0, |native| native.control_key_state),
    })
}

pub(crate) fn normalize_modifier_event(
    modifiers: &gpui::Modifiers,
    native_key: WindowsNativeKey,
) -> KeyEvent {
    KeyEvent {
        action: if native_key.is_down {
            KeyAction::Press
        } else {
            KeyAction::Release
        },
        mods: pack_key_mods(modifiers, Some(native_key)),
        consumed_mods: Mods(0),
        repeat_count: 1,
        code: W3cCode::from_raw(0),
        text_len: 0,
        text: [0; 32],
        unshifted_codepoint: 0,
        composing: 0,
        has_win_vk: 1,
        win_vk: native_key.virtual_key,
        has_win_scan: 1,
        win_scan: native_key.scan_code,
        has_win_control_key_state: 1,
        win_control_key_state: native_key.control_key_state,
    }
}

fn compute_consumed_mods(native_key: Option<WindowsNativeKey>, text_len: usize) -> Mods {
    if text_len == 0 {
        return Mods(0);
    }

    // AltGr generates text while reporting Ctrl+Alt on Windows.
    // Mark these as consumed so text input doesn't look like a Ctrl+Alt binding.
    // TODO: Find out a better way to do this than a heuristic.
    if let Some(native) = native_key {
        let state = native.control_key_state;
        let ctrl = LEFT_CTRL_PRESSED | RIGHT_CTRL_PRESSED;
        if (state & RIGHT_ALT_PRESSED) != 0 && (state & ctrl) != 0 {
            return Mods((1 << 1) | (1 << 2));
        }
    }

    Mods(0)
}

fn normalize_key_action(action: KeyAction, is_held: bool) -> KeyAction {
    match action {
        KeyAction::Press if is_held => KeyAction::Repeat,
        _ => action,
    }
}

/// Compute the unshifted codepoint from a keystroke.
/// Essential for the Kitty keyboard protocol to correctly encode shifted keys.
fn compute_unshifted_codepoint(keystroke: &gpui::Keystroke) -> u32 {
    let shift = keystroke.modifiers.shift;
    let key = &keystroke.key;

    // For single ASCII letters (a-z), the unshifted is always lowercase
    if key.len() == 1
        && let Some(c) = key.chars().next()
        && c.is_ascii_lowercase()
    {
        return c as u32;
    }

    // For ASCII digits and symbols, unshifted is the key itself (lowercased for letters)
    if key.len() == 1
        && let Some(c) = key.chars().next()
        && (c.is_ascii_digit() || c.is_ascii_punctuation() || c == ' ')
    {
        return c as u32;
    }

    // For other keys, try to derive from key_char
    if let Some(ref kc) = keystroke.key_char
        && let Some(c) = kc.chars().next()
    {
        // If shift is held, unshifted is lowercase; otherwise use as-is
        return if shift {
            c.to_ascii_lowercase() as u32
        } else {
            c as u32
        };
    }

    0
}

fn map_single_char_key(byte: u8) -> Option<W3cCode> {
    match byte {
        b'a'..=b'z' => map_letter_key(byte),
        b'A'..=b'Z' => map_letter_key(byte.to_ascii_lowercase()),
        b'0'..=b'9' => map_digit_key(byte),
        b'`' => key_from_w3c(b"backquote"),
        b'\\' => key_from_w3c(b"backslash"),
        b'[' => key_from_w3c(b"bracket_left"),
        b']' => key_from_w3c(b"bracket_right"),
        b',' => key_from_w3c(b"comma"),
        b'=' => key_from_w3c(b"equal"),
        b'-' => key_from_w3c(b"minus"),
        b'.' => key_from_w3c(b"period"),
        b'\'' => key_from_w3c(b"quote"),
        b';' => key_from_w3c(b"semicolon"),
        b'/' => key_from_w3c(b"slash"),
        b' ' => key_from_w3c(b"space"),
        _ => None,
    }
}

fn map_letter_key(byte: u8) -> Option<W3cCode> {
    let mut code = *b"key_a";
    code[4] = byte;
    key_from_w3c(&code)
}

fn map_digit_key(byte: u8) -> Option<W3cCode> {
    let mut code = *b"digit_0";
    code[6] = byte;
    key_from_w3c(&code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_key_letters() {
        assert_eq!(map_key("a"), key_from_w3c(b"key_a"));
        assert_eq!(map_key("z"), key_from_w3c(b"key_z"));
        assert_ne!(map_key("a"), map_key("b"));
    }

    #[test]
    fn map_key_digits() {
        assert_eq!(map_key("0"), key_from_w3c(b"digit_0"));
        assert_eq!(map_key("9"), key_from_w3c(b"digit_9"));
        assert_ne!(map_key("0"), map_key("1"));
    }

    #[test]
    fn map_key_named() {
        assert_eq!(map_key("enter"), key_from_w3c(b"enter"));
        assert_eq!(map_key("enter"), map_key("return")); // aliases
        assert_eq!(map_key("escape"), key_from_w3c(b"escape"));
        assert_eq!(map_key("backspace"), key_from_w3c(b"backspace"));
        assert_eq!(map_key("tab"), key_from_w3c(b"tab"));
        assert_eq!(map_key("left"), key_from_w3c(b"arrow_left"));
        assert_eq!(map_key("f1"), key_from_w3c(b"f1"));
        assert_eq!(map_key("f12"), key_from_w3c(b"f12"));
        assert_eq!(map_key("pageup"), key_from_w3c(b"page_up"));
        assert_eq!(map_key("pageup"), map_key("page_up")); // aliases
        assert_eq!(map_key("delete"), key_from_w3c(b"delete"));
        assert_eq!(map_key("home"), key_from_w3c(b"home"));
        assert_eq!(map_key("end"), key_from_w3c(b"end"));
    }

    #[test]
    fn map_key_symbols() {
        assert!(map_key(";").is_some());
        assert!(map_key("/").is_some());
        assert!(map_key(" ").is_some());
        assert!(map_key("-").is_some());
        assert!(map_key("[").is_some());
    }

    #[test]
    fn map_key_unknown_returns_none() {
        assert_eq!(map_key("shift"), None);
        assert_eq!(map_key("control"), None);
        assert_eq!(map_key("alt"), None);
        assert_eq!(map_key("capslock"), None);
        assert_eq!(map_key("play"), None);
    }

    #[test]
    fn pack_key_mods_bitfield() {
        let mut modifiers = gpui::Modifiers::default();
        assert_eq!(pack_key_mods(&modifiers, None), Mods(0));

        modifiers.shift = true;
        assert_eq!(pack_key_mods(&modifiers, None), Mods(0b0001));

        modifiers = gpui::Modifiers::default();
        modifiers.control = true;
        assert_eq!(pack_key_mods(&modifiers, None), Mods(0b0010));

        modifiers = gpui::Modifiers::default();
        modifiers.alt = true;
        assert_eq!(pack_key_mods(&modifiers, None), Mods(0b0100));

        modifiers = gpui::Modifiers::default();
        modifiers.platform = true;
        assert_eq!(pack_key_mods(&modifiers, None), Mods(0b1000));

        modifiers = gpui::Modifiers {
            shift: true,
            control: true,
            alt: true,
            platform: true,
            ..gpui::Modifiers::default()
        };
        assert_eq!(pack_key_mods(&modifiers, None), Mods(0b1111));
    }

    #[test]
    fn pack_key_mods_includes_lock_bits_from_native_state() {
        let native = WindowsNativeKey {
            control_key_state: CAPSLOCK_ON | NUMLOCK_ON,
            ..WindowsNativeKey::default()
        };

        assert_eq!(
            pack_key_mods(&gpui::Modifiers::default(), Some(native)),
            Mods(0b11_0000)
        );
    }
}
