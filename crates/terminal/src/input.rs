//! Input encoding: GPUI events → VT byte sequences.
//!
//! The renderer normalizes GPUI events and calls methods here.
//! This module snapshots terminal input mode flags and encodes events
//! lock-free using ghostty's encode_key/encode_mouse functions.
//!
//! Key mapping strategy:
//! GPUI Keystroke.key (string) → W3C key code (string) → Ghostty Key (i32)
//! The GPUI→W3C mapping is a static string table (stable across Ghostty versions).
//! The W3C→Ghostty mapping uses Ghostty's own `Key.fromW3C()` via FFI.

use std::collections::HashMap;
use std::sync::OnceLock;

use ghostty::{InputOpts, encode_key, encode_mouse};

/// Maximum size for key/mouse encode output buffers.
/// Ghostty's key encoder can produce up to ~32 bytes for complex
/// kitty protocol sequences. 128 bytes is generous.
pub const ENCODE_BUF_SIZE: usize = 128;

/// Ghostty Mods packed bitfield (u16).
/// Must match ghostty/src/input/key_mods.zig Mods packed struct.
///
/// Layout: shift(1) | ctrl(1) | alt(1) | super(1) | caps_lock(1) |
///         num_lock(1) | sides(4) | padding(6)
fn pack_mods(shift: bool, ctrl: bool, alt: bool, super_: bool) -> u16 {
    let mut mods: u16 = 0;
    if shift {
        mods |= 1 << 0;
    }
    if ctrl {
        mods |= 1 << 1;
    }
    if alt {
        mods |= 1 << 2;
    }
    if super_ {
        mods |= 1 << 3;
    }
    mods
}

// --- GPUI key string → W3C key code mapping ---

/// Static mapping from GPUI `Keystroke.key` strings to W3C key code
/// strings. GPUI key strings are lowercase on Windows/Linux.
///
/// W3C key codes: https://www.w3.org/TR/uievents-code
/// Ghostty's `Key.fromW3C()` handles the W3C→enum conversion.
///
/// Single ASCII characters are handled separately (see `map_key`).
/// This table covers named keys only.
const GPUI_TO_W3C: &[(&str, &str)] = &[
    // Functional keys
    ("enter", "Enter"),
    ("return", "Enter"),
    ("tab", "Tab"),
    ("escape", "Escape"),
    ("backspace", "Backspace"),
    ("space", "Space"),
    // Control pad
    ("delete", "Delete"),
    ("insert", "Insert"),
    ("home", "Home"),
    ("end", "End"),
    ("pageup", "PageUp"),
    ("page_up", "PageUp"),
    ("pagedown", "PageDown"),
    ("page_down", "PageDown"),
    // Arrow keys
    ("left", "ArrowLeft"),
    ("right", "ArrowRight"),
    ("up", "ArrowUp"),
    ("down", "ArrowDown"),
    // Function keys
    ("f1", "F1"),
    ("f2", "F2"),
    ("f3", "F3"),
    ("f4", "F4"),
    ("f5", "F5"),
    ("f6", "F6"),
    ("f7", "F7"),
    ("f8", "F8"),
    ("f9", "F9"),
    ("f10", "F10"),
    ("f11", "F11"),
    ("f12", "F12"),
    ("f13", "F13"),
    ("f14", "F14"),
    ("f15", "F15"),
    ("f16", "F16"),
    ("f17", "F17"),
    ("f18", "F18"),
    ("f19", "F19"),
    ("f20", "F20"),
    ("f21", "F21"),
    ("f22", "F22"),
    ("f23", "F23"),
    ("f24", "F24"),
];

/// Lazily-built HashMap from GPUI key strings to resolved Ghostty Key
/// integers. Built once on first use. Includes both named keys from
/// `GPUI_TO_W3C` and single-char ASCII keys.
fn key_map() -> &'static HashMap<String, i32> {
    static MAP: OnceLock<HashMap<String, i32>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut map = HashMap::new();

        // Named keys via W3C codes
        for &(gpui_key, w3c_code) in GPUI_TO_W3C {
            if let Some(val) = ghostty::key_from_w3c(w3c_code) {
                map.insert(gpui_key.to_string(), val);
            }
        }

        // Single ASCII letters → W3C "KeyA".."KeyZ"
        for ch in b'a'..=b'z' {
            let gpui_key = String::from(ch as char);
            let w3c = format!("Key{}", (ch as char).to_ascii_uppercase());
            if let Some(val) = ghostty::key_from_w3c(&w3c) {
                map.insert(gpui_key, val);
            }
        }

        // Single ASCII digits → W3C "Digit0".."Digit9"
        for ch in b'0'..=b'9' {
            let gpui_key = String::from(ch as char);
            let w3c = format!("Digit{}", ch as char);
            if let Some(val) = ghostty::key_from_w3c(&w3c) {
                map.insert(gpui_key, val);
            }
        }

        // Symbol keys → W3C codes
        let symbols: &[(&str, &str)] = &[
            ("`", "Backquote"),
            ("\\", "Backslash"),
            ("[", "BracketLeft"),
            ("]", "BracketRight"),
            (",", "Comma"),
            ("=", "Equal"),
            ("-", "Minus"),
            (".", "Period"),
            ("'", "Quote"),
            (";", "Semicolon"),
            ("/", "Slash"),
            (" ", "Space"),
        ];
        for &(gpui_key, w3c_code) in symbols {
            if let Some(val) = ghostty::key_from_w3c(w3c_code) {
                map.insert(gpui_key.to_string(), val);
            }
        }

        map
    })
}

/// Map a GPUI Keystroke.key string to a Ghostty Key enum integer.
/// Returns `None` for keys we don't handle (modifier-only keys,
/// media keys, etc.).
pub(crate) fn map_key(key: &str) -> Option<i32> {
    if let Some(value) = key_map().get(key) {
        return Some(*value);
    }

    if key.as_bytes().iter().any(|b| b.is_ascii_uppercase()) {
        let lower = key.to_ascii_lowercase();
        return key_map().get(lower.as_str()).copied();
    }

    None
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

/// Encode a GPUI key event into VT bytes using Ghostty's encoder.
///
/// `opts` is a mode snapshot captured under the terminal mutex by the caller.
/// Encoding runs entirely lock-free against the snapshot.
///
/// Returns `Some(Vec<u8>)` with the encoded bytes, or `None` if the
/// keystroke produces no terminal output.
pub fn encode_key_event<'a>(
    opts: InputOpts,
    keystroke: &gpui::Keystroke,
    is_held: bool,
    buf: &'a mut [u8; ENCODE_BUF_SIZE],
) -> Option<&'a [u8]> {
    let key = keystroke.key.as_str();
    let ghostty_key = map_key(key)?;

    let mods = pack_mods(
        keystroke.modifiers.shift,
        keystroke.modifiers.control,
        keystroke.modifiers.alt,
        keystroke.modifiers.platform,
    );

    // action: 0=release, 1=press, 2=repeat
    let action: u8 = if is_held { 2 } else { 1 };

    // Text for kitty keyboard protocol: use key_char if available,
    // otherwise use the key string for single printable chars.
    let text = keystroke
        .key_char
        .as_deref()
        .unwrap_or(if key.len() == 1 { key } else { "" });

    // Unshifted codepoint: the key as if shift wasn't pressed.
    // Critical for kitty keyboard protocol to encode shifted keys correctly.
    let unshifted_codepoint = compute_unshifted_codepoint(keystroke);

    let n = encode_key(
        opts,
        ghostty_key,
        mods,
        action,
        text.as_bytes(),
        unshifted_codepoint,
        buf,
    );

    if n == 0 { None } else { Some(&buf[..n]) }
}

/// Encode a bracketed paste. Wraps text with `\x1b[200~` / `\x1b[201~`
/// if bracketed paste mode is active in `opts`, otherwise sends as-is.
pub fn encode_paste(opts: InputOpts, text: &str) -> Vec<u8> {
    if opts.bracketed_paste {
        let mut out = Vec::with_capacity(text.len() + 12);
        out.extend_from_slice(b"\x1b[200~");
        out.extend_from_slice(text.as_bytes());
        out.extend_from_slice(b"\x1b[201~");
        out
    } else {
        text.as_bytes().to_vec()
    }
}

/// Encode a focus change event.
/// Returns `Some(bytes)` if focus event mode (DEC 1004) is active in `opts`,
/// `None` otherwise.
pub fn encode_focus_change(opts: InputOpts, focused: bool) -> Option<&'static [u8]> {
    if !opts.focus_event_mode {
        return None;
    }
    if focused {
        Some(b"\x1b[I")
    } else {
        Some(b"\x1b[O")
    }
}

/// Encode a mouse event using Ghostty's encoder.
///
/// `opts` is a mode snapshot captured under the terminal mutex by the caller.
/// Encoding runs entirely lock-free against the snapshot.
///
/// `button`: 0=left, 1=middle, 2=right, 64=scroll_up, 65=scroll_down
/// `action`: 0=press, 1=release, 2=motion
/// `shift`, `alt`, `ctrl`: modifier state
/// `x`, `y`: 0-indexed cell coordinates
///
/// Returns `Some(Vec<u8>)` with encoded bytes, or `None` if mouse
/// reporting is disabled or the event produces no output.
#[allow(clippy::too_many_arguments)]
pub fn encode_mouse_event(
    opts: InputOpts,
    button: u8,
    action: u8,
    shift: bool,
    alt: bool,
    ctrl: bool,
    x: u16,
    y: u16,
    buf: &mut [u8; ENCODE_BUF_SIZE],
) -> Option<&[u8]> {
    let mods: u8 = (shift as u8) | ((alt as u8) << 1) | ((ctrl as u8) << 2);
    let n = encode_mouse(opts, button, action, mods, x, y, &mut buf[..]);
    if n == 0 { None } else { Some(&buf[..n]) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_key_letters() {
        // Verify single-char mapping resolves via W3C
        assert!(map_key("a").is_some());
        assert!(map_key("z").is_some());
        // Different letters should map to different keys
        assert_ne!(map_key("a"), map_key("b"));
    }

    #[test]
    fn map_key_digits() {
        assert!(map_key("0").is_some());
        assert!(map_key("9").is_some());
        assert_ne!(map_key("0"), map_key("1"));
    }

    #[test]
    fn map_key_named() {
        assert!(map_key("enter").is_some());
        assert_eq!(map_key("enter"), map_key("return")); // aliases
        assert!(map_key("escape").is_some());
        assert!(map_key("backspace").is_some());
        assert!(map_key("tab").is_some());
        assert!(map_key("left").is_some());
        assert!(map_key("f1").is_some());
        assert!(map_key("f12").is_some());
        assert!(map_key("pageup").is_some());
        assert_eq!(map_key("pageup"), map_key("page_up")); // aliases
        assert!(map_key("delete").is_some());
        assert!(map_key("home").is_some());
        assert!(map_key("end").is_some());
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
    fn pack_mods_bitfield() {
        assert_eq!(pack_mods(false, false, false, false), 0);
        assert_eq!(pack_mods(true, false, false, false), 0b0001);
        assert_eq!(pack_mods(false, true, false, false), 0b0010);
        assert_eq!(pack_mods(false, false, true, false), 0b0100);
        assert_eq!(pack_mods(false, false, false, true), 0b1000);
        assert_eq!(pack_mods(true, true, true, true), 0b1111);
    }
}
