use core::ffi::c_void;

pub type BellCallback = unsafe extern "C" fn(userdata: *mut c_void);
pub type TitleCallback = unsafe extern "C" fn(userdata: *mut c_void, ptr: *const u8, len: usize);

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
#[derive(Debug, Clone, Copy, Default)]
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
    pub codepoint: u32,
    pub grapheme_len: u8,
    pub wide: u8,
    pub fg_color_type: u8,
    pub fg_r: u8,
    pub fg_g: u8,
    pub fg_b: u8,
    pub fg_palette: u8,
    pub bg_color_type: u8,
    pub bg_r: u8,
    pub bg_g: u8,
    pub bg_b: u8,
    pub bg_palette: u8,
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
