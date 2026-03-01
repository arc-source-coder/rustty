use bytemuck::{Pod, Zeroable};
use core::ffi::c_void;

pub(crate) type BellCallback = unsafe extern "C" fn(userdata: *mut c_void);
pub(crate) type TitleCallback =
    unsafe extern "C" fn(userdata: *mut c_void, ptr: *const u8, len: usize);
pub(crate) type ResponseCallback =
    unsafe extern "C" fn(userdata: *mut c_void, ptr: *const u8, len: usize);

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CursorState {
    pub x: u16,
    pub y: u16,
    pub in_viewport: u8,
    pub style: u8,
    pub visible: u8,
    pub blinking: u8,
    pub password_input: u8,
    pub wide_tail: u8,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, Pod, Zeroable)]
pub struct ColorRGB {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ColorState {
    pub background: ColorRGB,
    pub foreground: ColorRGB,
    pub cursor_color: ColorRGB,
    pub has_cursor_color: u8,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FlatCell {
    /// Primary codepoint (0 = empty cell)
    pub codepoint: u32,
    /// Number of extra codepoints in the grapheme cluster (0 for simple chars)
    pub grapheme_len: u8,
    /// Wide property: 0=narrow, 1=wide, 2=spacer_tail, 3=spacer_head
    pub wide: u8,

    /// Foreground color type: 0=none/default, 1=palette, 2=rgb
    pub fg_color_type: u8,
    pub fg_r: u8,
    pub fg_g: u8,
    pub fg_b: u8,
    pub fg_palette: u8,

    /// Background color type: 0=none/default, 1=palette, 2=rgb
    /// Note: for bg_color_palette/bg_color_rgb content_tags, bg is set
    /// from the cell content directly (not from style).
    pub bg_color_type: u8,
    pub bg_r: u8,
    pub bg_g: u8,
    pub bg_b: u8,
    pub bg_palette: u8,

    /// Underline color type: 0=none, 1=palette, 2=rgb
    pub ul_color_type: u8,
    pub ul_r: u8,
    pub ul_g: u8,
    pub ul_b: u8,
    pub ul_palette: u8,

    /// Style flags bitfield (matches Ghostty Style.Flags packed u16):
    /// bit 0: bold, 1: italic, 2: faint, 3: blink, 4: inverse,
    /// 5: invisible, 6: strikethrough, 7: overline
    /// bits 8-10: underline (0=none,1=single,2=double,3=curly,4=dotted,5=dashed)
    pub style_flags: u16,
    pub _padding: [u8; 2],
}

// Verify ABI matches Zig side
const _: () = assert!(std::mem::size_of::<FlatCell>() == 28);
const _: () = assert!(std::mem::align_of::<FlatCell>() == 4);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirtyState {
    Clean,
    Partial,
    Full,
}

impl DirtyState {
    pub(crate) fn from_raw(value: u8) -> Self {
        match value {
            0 => DirtyState::Clean,
            1 => DirtyState::Partial,
            _ => DirtyState::Full,
        }
    }

    pub fn is_dirty(self) -> bool {
        self != DirtyState::Clean
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseMode {
    None,
    X10,
    Normal,
    Button,
    Any,
}

impl MouseMode {
    pub(crate) fn from_raw(value: u8) -> Self {
        match value {
            0 => MouseMode::None,
            1 => MouseMode::X10,
            2 => MouseMode::Normal,
            3 => MouseMode::Button,
            _ => MouseMode::Any,
        }
    }

    pub(crate) fn to_raw(self) -> u8 {
        match self {
            MouseMode::None => 0,
            MouseMode::X10 => 1,
            MouseMode::Normal => 2,
            MouseMode::Button => 3,
            MouseMode::Any => 4,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseFormat {
    X10,
    Utf8,
    Sgr,
    Urxvt,
    SgrPixels,
}

impl MouseFormat {
    pub(crate) fn from_raw(value: u8) -> Self {
        match value {
            0 => MouseFormat::X10,
            1 => MouseFormat::Utf8,
            2 => MouseFormat::Sgr,
            3 => MouseFormat::Urxvt,
            _ => MouseFormat::SgrPixels,
        }
    }

    pub(crate) fn to_raw(self) -> u8 {
        match self {
            MouseFormat::X10 => 0,
            MouseFormat::Utf8 => 1,
            MouseFormat::Sgr => 2,
            MouseFormat::Urxvt => 3,
            MouseFormat::SgrPixels => 4,
        }
    }
}
