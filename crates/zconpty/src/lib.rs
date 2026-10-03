use std::ffi::c_void;
use std::fmt::{Display, Formatter};

unsafe extern "C" {
    fn zconpty_start_console_server(terminal_ptr: *const c_void, out_session: *mut isize) -> i32;
    fn zconpty_stop_console_server(session: isize);
    fn zconpty_send_key(session: isize, event: KeyEvent);
    fn zconpty_send_mouse(session: isize, event: MouseEvent);
    fn zconpty_send_paste(session: isize, ptr: *const u8, len: usize);
    fn zconpty_send_text(session: isize, ptr: *const u8, len: usize);
    fn zconpty_send_focus(session: isize, focused: bool);
    fn zconpty_send_resize(session: isize, cols: u16, rows: u16);
    fn zconpty_w3c_code_from_bytes(code_ptr: *const u8, code_len: usize) -> i32;
}

// TODO: Explore u8
#[repr(i32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyAction {
    Release = 0,
    Press = 1,
    Repeat = 2,
}

#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct W3cCode(u16);

impl W3cCode {
    pub const UNKNOWN: W3cCode = W3cCode::from_raw(0);

    /// Map a string to a W3C-based physical key enum.
    ///
    /// Handles character keys and action keys (arrows, home, end, page_up/down,
    /// backspace, delete, insert, enter, tab, escape, function keys (f1-f24) etc.)
    ///
    /// Returns `None` for modifier-only keys, media keys, etc.
    // NOTE(renderer-refactor): This could be more accurate/complete. Currently it
    // only handles GPUI-specific parsing and can reject valid inputs like "arrow_up".
    pub fn parse(key: &str) -> Option<Self> {
        if key.len() == 1 {
            let byte = key.as_bytes()[0];
            return match byte {
                // Format is 'key_T'
                b'a'..=b'z' => W3cCode::from_bytes(&[b'k', b'e', b'y', b'_', byte]),
                // Format is 'key_T'
                b'A'..=b'Z' => {
                    W3cCode::from_bytes(&[b'k', b'e', b'y', b'_', byte.to_ascii_lowercase()])
                }
                // Format is `digit_T'
                b'0'..=b'9' => W3cCode::from_bytes(&[b'd', b'i', b'g', b'i', b't', b'_', byte]),
                b'`' => W3cCode::from_bytes(b"backquote"),
                b'\\' => W3cCode::from_bytes(b"backslash"),
                b'[' => W3cCode::from_bytes(b"bracket_left"),
                b']' => W3cCode::from_bytes(b"bracket_right"),
                b',' => W3cCode::from_bytes(b"comma"),
                b'=' => W3cCode::from_bytes(b"equal"),
                b'-' => W3cCode::from_bytes(b"minus"),
                b'.' => W3cCode::from_bytes(b"period"),
                b'\'' => W3cCode::from_bytes(b"quote"),
                b';' => W3cCode::from_bytes(b"semicolon"),
                b'/' => W3cCode::from_bytes(b"slash"),
                b' ' => W3cCode::from_bytes(b"space"),
                _ => None,
            };
        }

        // GPUI always lower-cases the key. 'A' is 'shift' + 'a'
        let w3c = match key {
            "return" => "enter",
            "pageup" | "page_up" => "page_up",
            "pagedown" | "page_down" => "page_down",
            "left" => "arrow_left",
            "right" => "arrow_right",
            "up" => "arrow_up",
            "down" => "arrow_down",
            "enter" | "tab" | "escape" | "backspace" | "space" | "delete" | "insert" | "home"
            | "end" | "f1" | "f2" | "f3" | "f4" | "f5" | "f6" | "f7" | "f8" | "f9" | "f10"
            | "f11" | "f12" | "f13" | "f14" | "f15" | "f16" | "f17" | "f18" | "f19" | "f20"
            | "f21" | "f22" | "f23" | "f24" => key,
            _ => return None,
        };

        W3cCode::from_bytes(w3c.as_bytes())
    }

    #[inline]
    pub fn from_bytes(code: &[u8]) -> Option<Self> {
        if code.is_empty() {
            return None;
        }

        let resolved = unsafe { zconpty_w3c_code_from_bytes(code.as_ptr(), code.len()) };
        u16::try_from(resolved).ok().map(W3cCode)
    }

    #[inline]
    pub const fn from_raw(raw: u16) -> Self {
        Self(raw)
    }

    #[inline]
    pub const fn as_raw(self) -> u16 {
        self.0
    }
}

impl From<W3cCode> for u16 {
    fn from(value: W3cCode) -> Self {
        value.0
    }
}

impl From<W3cCode> for i32 {
    fn from(value: W3cCode) -> Self {
        i32::from(value.0)
    }
}

/// Packing Order:
/// shift - bit 0
/// ctrl - bit 1
/// alt - bit 2
/// super - bit 3
/// caps_lock - bit 4
/// num_lock - bit 5
/// reserved - bit 6-15
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Modifiers(pub u16);

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyEvent {
    pub action: KeyAction,
    pub mods: Modifiers,
    pub consumed_mods: Modifiers,
    pub repeat_count: u16,
    pub code: W3cCode,
    pub text_len: u8,
    pub text: [u8; 32],
    pub unshifted_codepoint: u32,
    pub composing: bool,
    pub has_win_vk: bool,
    pub win_vk: u16,
    pub has_win_scan: bool,
    pub win_scan: u16,
    pub has_win_control_key_state: bool,
    pub win_control_key_state: u32,
}

impl KeyEvent {
    #[inline]
    pub const fn new(action: KeyAction, code: W3cCode) -> Self {
        Self {
            action,
            mods: Modifiers(0),
            consumed_mods: Modifiers(0),
            repeat_count: 1,
            code,
            text_len: 0,
            text: [0; 32],
            unshifted_codepoint: 0,
            composing: false,
            has_win_vk: false,
            win_vk: 0,
            has_win_scan: false,
            win_scan: 0,
            has_win_control_key_state: false,
            win_control_key_state: 0,
        }
    }

    #[inline]
    pub const fn press(code: W3cCode) -> Self {
        Self::new(KeyAction::Press, code)
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MouseEvent {
    pub position: MousePosition,
    pub modifiers: Modifiers,
    pub button: MouseButton,
    pub action: MouseAction,
}

#[repr(i8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MouseButton {
    /// No button pressed
    None = -1,
    /// Unknown button
    Unknown = 0,

    Left = 1,
    Right = 2,
    Middle = 3,

    /// Scroll up is Button 4 for encoding
    WheelUp = 4,
    /// Scroll down is Button 5 for encoding
    WheelDown = 5,
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MouseAction {
    Press = 0,
    Release = 1,
    Move = 2,
}

/// Mouse position in screen pixels
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MousePosition {
    pub x_px: f32,
    pub y_px: f32,
}

// TODO: Replace isize with u64
pub struct ConPTY {
    session: isize,
}

impl ConPTY {
    // TODO: Replace *const c_void with u64 handle
    #[allow(clippy::missing_errors_doc)]
    #[allow(clippy::not_unsafe_ptr_arg_deref)]
    pub fn new(terminal_handle: *const c_void) -> Result<Self, StartError> {
        let mut session = 0;

        // Safety: zconpty never dereferences the terminal handle. It is
        // stored and passed back as a handle in future calls into the terminal.
        let hresult = unsafe { zconpty_start_console_server(terminal_handle, &raw mut session) };
        if hresult < 0 {
            return Err(StartError { hresult });
        }

        Ok(Self { session })
    }

    #[inline]
    pub fn send_key(&self, event: KeyEvent) {
        // Per-session host-ingress invariant: exactly one host-owned thread
        // should call `send_*` for a given session. Device responses bypass
        // host ingress and inject directly from Zig's console thread.
        unsafe { zconpty_send_key(self.session, event) };
    }

    #[inline]
    pub fn send_mouse(&self, event: MouseEvent) {
        unsafe { zconpty_send_mouse(self.session, event) };
    }

    #[inline]
    pub fn send_text(&self, text: &[u8]) {
        if text.is_empty() {
            return;
        }

        unsafe { zconpty_send_text(self.session, text.as_ptr(), text.len()) };
    }

    #[inline]
    pub fn send_paste(&self, text: &[u8]) {
        if text.is_empty() {
            return;
        }

        unsafe { zconpty_send_paste(self.session, text.as_ptr(), text.len()) };
    }

    #[inline]
    pub fn send_focus(&self, focused: bool) {
        unsafe { zconpty_send_focus(self.session, focused) };
    }

    #[inline]
    pub fn send_resize(&self, cols: u16, rows: u16) {
        unsafe { zconpty_send_resize(self.session, cols, rows) };
    }
}

impl Drop for ConPTY {
    #[inline]
    fn drop(&mut self) {
        unsafe { zconpty_stop_console_server(self.session) };
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartError {
    pub hresult: i32,
}

impl Display for StartError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "failed to start ConPTY console session: HRESULT=0x{:08X}",
            self.hresult
        )
    }
}

impl std::error::Error for StartError {}

const _: () = assert!(std::mem::size_of::<KeyAction>() == std::mem::size_of::<std::ffi::c_int>());
const _: () = assert!(std::mem::align_of::<KeyAction>() == std::mem::align_of::<std::ffi::c_int>());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_key_letters() {
        assert_eq!(W3cCode::parse("a"), W3cCode::from_bytes(b"key_a"));
        assert_eq!(W3cCode::parse("z"), W3cCode::from_bytes(b"key_z"));
        assert_ne!(W3cCode::parse("a"), W3cCode::parse("b"));
    }

    #[test]
    fn parse_key_digits() {
        assert_eq!(W3cCode::parse("0"), W3cCode::from_bytes(b"digit_0"));
        assert_eq!(W3cCode::parse("9"), W3cCode::from_bytes(b"digit_9"));
        assert_ne!(W3cCode::parse("0"), W3cCode::parse("1"));
    }

    #[test]
    fn parse_key_named() {
        assert_eq!(W3cCode::parse("enter"), W3cCode::from_bytes(b"enter"));
        assert_eq!(W3cCode::parse("enter"), W3cCode::parse("return")); // aliases
        assert_eq!(W3cCode::parse("escape"), W3cCode::from_bytes(b"escape"));
        assert_eq!(
            W3cCode::parse("backspace"),
            W3cCode::from_bytes(b"backspace")
        );
        assert_eq!(W3cCode::parse("tab"), W3cCode::from_bytes(b"tab"));
        assert_eq!(W3cCode::parse("left"), W3cCode::from_bytes(b"arrow_left"));
        assert_eq!(W3cCode::parse("f1"), W3cCode::from_bytes(b"f1"));
        assert_eq!(W3cCode::parse("f12"), W3cCode::from_bytes(b"f12"));
        assert_eq!(W3cCode::parse("pageup"), W3cCode::from_bytes(b"page_up"));
        assert_eq!(W3cCode::parse("pageup"), W3cCode::parse("page_up")); // aliases
        assert_eq!(W3cCode::parse("delete"), W3cCode::from_bytes(b"delete"));
        assert_eq!(W3cCode::parse("home"), W3cCode::from_bytes(b"home"));
        assert_eq!(W3cCode::parse("end"), W3cCode::from_bytes(b"end"));
    }

    #[test]
    fn parse_key_symbols() {
        assert!(W3cCode::parse(";").is_some());
        assert!(W3cCode::parse("/").is_some());
        assert!(W3cCode::parse(" ").is_some());
        assert!(W3cCode::parse("-").is_some());
        assert!(W3cCode::parse("[").is_some());
    }

    #[test]
    fn parse_key_unknown_returns_none() {
        assert_eq!(W3cCode::parse("shift"), None);
        assert_eq!(W3cCode::parse("control"), None);
        assert_eq!(W3cCode::parse("alt"), None);
        assert_eq!(W3cCode::parse("capslock"), None);
        assert_eq!(W3cCode::parse("play"), None);
    }

    #[test]
    fn from_bytes_resolves_known_codes() {
        assert!(W3cCode::from_bytes(b"key_a").is_some());
        assert!(W3cCode::from_bytes(b"enter").is_some());
        assert!(W3cCode::from_bytes(b"arrow_left").is_some());
        // Mixed-case W3C names should keep working for compatibility.
        assert!(W3cCode::from_bytes(b"ArrowLeft").is_some());
    }

    #[test]
    fn from_bytes_rejects_unknown_codes() {
        assert_eq!(W3cCode::from_bytes(b""), None);
        assert_eq!(W3cCode::from_bytes(b"FooBar"), None);
    }

    #[test]
    fn key_event_press_sets_safe_defaults() {
        let code = W3cCode::from_bytes(b"arrow_up").expect("arrow_up should resolve");
        let event = KeyEvent::press(code);

        assert_eq!(event.action, KeyAction::Press);
        assert_eq!(event.code, code);
        assert_eq!(event.repeat_count, 1);
        assert_eq!(event.text_len, 0);
    }
}
