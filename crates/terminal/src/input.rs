//! Input normalization: GPUI events → zconpty wire structs.
//!
//! The UI layer lowers GPUI events into zconpty-owned structs here.
//!
//! Key mapping strategy:
//! Rust forwards normalized text/modifiers/native metadata.
//! zconpty owns the Windows-native key resolution details.

use gpui::{KeyDownEvent, KeyUpEvent, Keystroke, ModifiersChangedEvent, WindowsNativeKey};
use zconpty::{KeyAction, KeyEvent, Modifiers, W3cCode};

const RIGHT_ALT_PRESSED: u32 = 0x0001;
const LEFT_CTRL_PRESSED: u32 = 0x0008;
const RIGHT_CTRL_PRESSED: u32 = 0x0004;
const CAPSLOCK_ON: u32 = 0x0080;
const NUMLOCK_ON: u32 = 0x0020;

pub trait ToZconptyMods {
    fn mods(&self) -> Modifiers;
}

impl ToZconptyMods for gpui::Modifiers {
    fn mods(&self) -> Modifiers {
        Modifiers(
            u16::from(self.shift)
                | (u16::from(self.control) << 1)
                | (u16::from(self.alt) << 2)
                | (u16::from(self.platform) << 3),
        )
    }
}

impl ToZconptyMods for KeyDownEvent {
    fn mods(&self) -> Modifiers {
        apply_native(self.keystroke.modifiers.mods(), self.native_key)
    }
}

impl ToZconptyMods for KeyUpEvent {
    fn mods(&self) -> Modifiers {
        apply_native(self.keystroke.modifiers.mods(), self.native_key)
    }
}

impl ToZconptyMods for ModifiersChangedEvent {
    fn mods(&self) -> Modifiers {
        let mut mods = self.modifiers.mods();
        if self.capslock.on {
            mods.0 |= 1 << 4;
        }
        apply_native(mods, self.changed_native_key)
    }
}

#[inline]
fn apply_native(mut mods: Modifiers, nk: Option<WindowsNativeKey>) -> Modifiers {
    if let Some(native_key) = nk {
        if (native_key.control_key_state & CAPSLOCK_ON) != 0 {
            mods.0 |= 1 << 4;
        }
        if (native_key.control_key_state & NUMLOCK_ON) != 0 {
            mods.0 |= 1 << 5;
        }
    }
    mods
}

pub trait ToKeyEvent {
    fn to_key_event(&self) -> Option<KeyEvent>;
}

impl ToKeyEvent for ModifiersChangedEvent {
    #[inline]
    fn to_key_event(&self) -> Option<KeyEvent> {
        let native_key = self.changed_native_key?;
        Some(KeyEvent {
            action: match native_key.is_down {
                true => KeyAction::Press,
                false => KeyAction::Release,
            },
            mods: self.mods(),
            consumed_mods: Modifiers(0),
            repeat_count: 1,
            code: W3cCode::UNKNOWN,
            text_len: 0,
            text: [0; 32],
            unshifted_codepoint: 0,
            composing: false,
            has_win_vk: true,
            win_vk: native_key.virtual_key,
            has_win_scan: true,
            win_scan: native_key.scan_code,
            has_win_control_key_state: true,
            win_control_key_state: native_key.control_key_state,
        })
    }
}

impl ToKeyEvent for KeyDownEvent {
    #[inline]
    fn to_key_event(&self) -> Option<KeyEvent> {
        let action = match self.is_held {
            true => KeyAction::Repeat,
            false => KeyAction::Press,
        };
        to_key_event(self, action, &self.keystroke, self.native_key)
    }
}

impl ToKeyEvent for KeyUpEvent {
    #[inline]
    fn to_key_event(&self) -> Option<KeyEvent> {
        to_key_event(self, KeyAction::Release, &self.keystroke, self.native_key)
    }
}

#[inline]
fn to_key_event<T: ToZconptyMods + ConsumedModifiers>(
    this: &T,
    action: KeyAction,
    keystroke: &Keystroke,
    native_key: Option<WindowsNativeKey>,
) -> Option<KeyEvent> {
    let native_key_exists = native_key.is_some();

    let code = 'code: {
        let Some(code) = W3cCode::parse(keystroke.key.as_str()) else {
            if native_key_exists {
                break 'code W3cCode::UNKNOWN;
            }
            return None;
        };
        code
    };

    let mut text: [u8; 32] = [0; 32];
    let source = keystroke.key_char.as_deref().unwrap_or("");
    let text_len = source.len().min(text.len());

    let bytes = source.as_bytes();
    text[..text_len].copy_from_slice(&bytes[..text_len]);

    Some(KeyEvent {
        action,
        mods: this.mods(),
        consumed_mods: this.consumed_modifiers(),
        repeat_count: 1,
        code,
        text_len: text_len as u8,
        text,
        unshifted_codepoint: keystroke.unshifted_codepoint(),
        composing: false,
        has_win_vk: native_key_exists,
        win_vk: native_key.map_or(0, |nk| nk.virtual_key),
        has_win_scan: native_key_exists,
        win_scan: native_key.map_or(0, |nk| nk.scan_code),
        has_win_control_key_state: native_key_exists,
        win_control_key_state: native_key.map_or(0, |nk| nk.control_key_state),
    })
}

pub trait UnshiftedCodepoint {
    /// Get the unshifted codepoint for the key event.
    /// Essential for the Kitty keyboard protocol to correctly encode shifted keys.
    fn unshifted_codepoint(&self) -> u32;
}

impl UnshiftedCodepoint for Keystroke {
    fn unshifted_codepoint(&self) -> u32 {
        if self.key.len() == 1 {
            let Some(c) = self.key.chars().next() else {
                return 0;
            };
            // For single ASCII letters (a-z), ASCII digits, and symbols,
            // unshifted is the key itself (lowercased for letters)
            if c.is_ascii_graphic() && !c.is_ascii_uppercase() || c == ' ' {
                return c as u32;
            }
        }

        // For other keys, try to derive from key_char
        let key_char = self.key_char.as_deref();
        if let Some(c) = key_char.and_then(|s| s.chars().next()) {
            // If shift is held, unshifted is lowercase; otherwise use as-is
            if self.modifiers.shift {
                return c.to_ascii_lowercase() as u32;
            }
            return c as u32;
        }

        0
    }
}

pub trait ConsumedModifiers {
    fn consumed_modifiers(&self) -> Modifiers;
}

impl ConsumedModifiers for KeyDownEvent {
    fn consumed_modifiers(&self) -> Modifiers {
        if self.keystroke.key_char.as_deref().unwrap_or("").is_empty() {
            return Modifiers(0);
        }

        // AltGr generates text while reporting Ctrl+Alt on Windows.
        // Mark these as consumed so text input doesn't look like a Ctrl+Alt binding.
        // TODO: Find out a better way to do this than a heuristic.
        if let Some(native_key) = self.native_key {
            let state = native_key.control_key_state;
            const CTRL_PRESSED: u32 = LEFT_CTRL_PRESSED | RIGHT_CTRL_PRESSED;
            if (state & RIGHT_ALT_PRESSED) != 0 && (state & CTRL_PRESSED) != 0 {
                return Modifiers((1 << 1) | (1 << 2));
            }
        }

        Modifiers(0)
    }
}

impl ConsumedModifiers for KeyUpEvent {
    fn consumed_modifiers(&self) -> Modifiers {
        Modifiers(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_key_mods_bitfield() {
        let mut modifiers = gpui::Modifiers::default();
        assert_eq!(modifiers.mods(), Modifiers(0));

        modifiers.shift = true;
        assert_eq!(modifiers.mods(), Modifiers(0b0001));

        modifiers = gpui::Modifiers::default();
        modifiers.control = true;
        assert_eq!(modifiers.mods(), Modifiers(0b0010));

        modifiers = gpui::Modifiers::default();
        modifiers.alt = true;
        assert_eq!(modifiers.mods(), Modifiers(0b0100));

        modifiers = gpui::Modifiers::default();
        modifiers.platform = true;
        assert_eq!(modifiers.mods(), Modifiers(0b1000));

        modifiers = gpui::Modifiers {
            shift: true,
            control: true,
            alt: true,
            platform: true,
            function: false,
        };
        assert_eq!(modifiers.mods(), Modifiers(0b1111));
    }
}
