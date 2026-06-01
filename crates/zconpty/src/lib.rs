use std::ffi::c_void;
use std::fmt::{Display, Formatter};

unsafe extern "C" {
    fn zconpty_start_console_server(terminal_ptr: *mut c_void, out_session: *mut isize) -> i32;
    fn zconpty_stop_console_server(session: isize);
    fn zconpty_send_key(session: isize, event: KeyEvent);
    fn zconpty_send_mouse(session: isize, event: MouseEvent);
    fn zconpty_send_paste(session: isize, ptr: *const u8, len: usize);
    fn zconpty_send_focus(session: isize, focused: bool);
    fn zconpty_send_resize(session: isize, cols: u16, rows: u16);
    fn zconpty_key_from_w3c(code_ptr: *const u8, code_len: usize) -> i32;
}

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
    pub const fn from_raw(raw: u16) -> Self {
        Self(raw)
    }

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
pub struct Mods(pub u16);

impl Mods {
    pub const MOD_SHIFT: u16 = 1 << 0;
    pub const MOD_CTRL: u16 = 1 << 1;
    pub const MOD_ALT: u16 = 1 << 2;
    pub const MOD_SUPER: u16 = 1 << 3;
    pub const MOD_CAPS_LOCK: u16 = 1 << 4;
    pub const MOD_NUM_LOCK: u16 = 1 << 5;
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyEvent {
    pub action: KeyAction,
    pub mods: Mods,
    pub consumed_mods: Mods,
    pub repeat_count: u16,
    pub code: W3cCode,
    pub text_len: u8,
    pub text: [u8; 32],
    pub unshifted_codepoint: u32,
    pub composing: u8,
    pub has_win_vk: u8,
    pub win_vk: u16,
    pub has_win_scan: u8,
    pub win_scan: u16,
    pub has_win_control_key_state: u8,
    pub win_control_key_state: u32,
}

impl KeyEvent {
    pub const fn new(action: KeyAction, code: W3cCode) -> Self {
        Self {
            action,
            mods: Mods(0),
            consumed_mods: Mods(0),
            repeat_count: 1,
            code,
            text_len: 0,
            text: [0; 32],
            unshifted_codepoint: 0,
            composing: 0,
            has_win_vk: 0,
            win_vk: 0,
            has_win_scan: 0,
            win_scan: 0,
            has_win_control_key_state: 0,
            win_control_key_state: 0,
        }
    }

    pub const fn press(code: W3cCode) -> Self {
        Self::new(KeyAction::Press, code)
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MouseEvent {
    pub button: MouseButton,
    pub action: MouseAction,
    pub mods: Mods,
    pub position: MousePosition,
}

#[repr(i8)]
#[derive(Clone, Copy, Debug, PartialEq)]
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

#[repr(i32)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MouseAction {
    Press = 0,
    Release = 1,
    Move = 2,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MousePosition {
    // Mouse position in cells
    pub x: u32,
    pub y: u32,
    // Mouse position in screen pixels
    pub x_px: f32,
    pub y_px: f32,
}

pub struct ConPTY {
    session: isize,
}

pub fn key_from_w3c(code: &[u8]) -> Option<W3cCode> {
    if code.is_empty() {
        return None;
    }

    let resolved = unsafe { zconpty_key_from_w3c(code.as_ptr(), code.len()) };
    u16::try_from(resolved).ok().map(W3cCode)
}

impl ConPTY {
    pub fn new(terminal_handle: *mut c_void) -> Result<Self, StartError> {
        let mut session = 0;
        let hresult = unsafe { zconpty_start_console_server(terminal_handle, &mut session) };
        if hresult < 0 {
            return Err(StartError { hresult });
        }

        Ok(Self { session })
    }

    pub fn send_key(&self, event: KeyEvent) {
        // Per-session host-ingress invariant: exactly one host-owned thread
        // should call `send_*` for a given session. Device responses bypass
        // host ingress and inject directly from Zig's console thread.
        unsafe { zconpty_send_key(self.session, event) };
    }

    pub fn send_mouse(&self, event: MouseEvent) {
        unsafe { zconpty_send_mouse(self.session, event) };
    }

    pub fn send_paste(&self, text: &[u8]) {
        if text.is_empty() {
            return;
        }

        unsafe { zconpty_send_paste(self.session, text.as_ptr(), text.len()) };
    }

    pub fn send_focus(&self, focused: bool) {
        unsafe { zconpty_send_focus(self.session, focused) };
    }

    pub fn send_resize(&self, cols: u16, rows: u16) {
        unsafe { zconpty_send_resize(self.session, cols, rows) };
    }
}

impl Drop for ConPTY {
    fn drop(&mut self) {
        unsafe {
            zconpty_stop_console_server(self.session);
        }
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
const _: () = assert!(std::mem::size_of::<MouseAction>() == std::mem::size_of::<std::ffi::c_int>());
const _: () =
    assert!(std::mem::align_of::<MouseAction>() == std::mem::align_of::<std::ffi::c_int>());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_from_w3c_resolves_known_codes() {
        assert!(key_from_w3c(b"key_a").is_some());
        assert!(key_from_w3c(b"enter").is_some());
        assert!(key_from_w3c(b"arrow_left").is_some());
        // Mixed-case W3C names should keep working for compatibility.
        assert!(key_from_w3c(b"ArrowLeft").is_some());
    }

    #[test]
    fn key_from_w3c_rejects_unknown_codes() {
        assert_eq!(key_from_w3c(b""), None);
        assert_eq!(key_from_w3c(b"FooBar"), None);
    }

    #[test]
    fn key_event_press_sets_safe_defaults() {
        let code = key_from_w3c(b"arrow_up").expect("arrow_up should resolve");
        let event = KeyEvent::press(code);

        assert_eq!(event.action, KeyAction::Press);
        assert_eq!(event.code, code);
        assert_eq!(event.repeat_count, 1);
        assert_eq!(event.text_len, 0);
    }
}
