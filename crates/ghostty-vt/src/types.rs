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

/// Zero-copy view into Ghostty's `color.RGB` (`packed struct(u24)`).
///
/// Zig's `packed struct(u24)` occupies 4 bytes in memory (padded to u32).
/// Bit layout (little-endian): `[7:0] r`, `[15:8] g`, `[23:16] b`, `[31:24] padding`.
/// This lets us read `[256]color.RGB` as `[256]ColorRGB` via pointer cast.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct ColorRGB(u32);

impl ColorRGB {
    /// Construct from individual components.
    #[inline]
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self(r as u32 | (g as u32) << 8 | (b as u32) << 16)
    }

    #[inline]
    pub const fn r(self) -> u8 {
        (self.0 & 0xFF) as u8
    }

    #[inline]
    pub const fn g(self) -> u8 {
        ((self.0 >> 8) & 0xFF) as u8
    }

    #[inline]
    pub const fn b(self) -> u8 {
        ((self.0 >> 16) & 0xFF) as u8
    }

    /// Pack as `[r, g, b, 0xFF]` in little-endian u32 — GPU-ready RGBA.
    #[inline]
    pub const fn to_rgba_u32(self) -> u32 {
        (self.0 & 0x00FF_FFFF) | 0xFF00_0000
    }

    /// Pack as `[r, g, b, a]` in little-endian u32.
    #[inline]
    pub const fn to_rgba_u32_with_alpha(self, a: u8) -> u32 {
        (self.0 & 0x00FF_FFFF) | (a as u32) << 24
    }

    #[inline]
    pub fn to_float4(self) -> [f32; 4] {
        [self.r() as f32 / 255.0, self.g() as f32 / 255.0, self.b() as f32 / 255.0, 1.0]
    }
}

impl std::fmt::Debug for ColorRGB {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ColorRGB")
            .field("r", &self.r())
            .field("g", &self.g())
            .field("b", &self.b())
            .finish()
    }
}

/// Mirrors Zig's RenderState.Colors tagged union layout.
/// Layout verified by Zig comptime assertions.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct OptionalColorRGB {
    rgb: ColorRGB,
    tag: u8,
    _pad: [u8; 3],
}

impl OptionalColorRGB {
    #[inline]
    pub fn into_option(self) -> Option<ColorRGB> {
        match self.tag {
            0 => None,
            1 => Some(self.rgb),
            _ => None,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RenderColors {
    pub background: ColorRGB,
    pub foreground: ColorRGB,
    cursor: OptionalColorRGB,
    pub palette: [ColorRGB; 256],
}

impl RenderColors {
    #[inline]
    pub fn cursor_color(&self) -> Option<ColorRGB> {
        self.cursor.into_option()
    }
}

/// Zero-copy view into Ghostty's page.Cell packed struct(u64).
///
/// Bit layout (from comptime probes, verified by Zig assertions):
///   [1:0]   content_tag: 0=codepoint, 1=codepoint_grapheme,
///                        2=bg_color_palette, 3=bg_color_rgb
///   [22:2]  content: u21 codepoint, u8 palette index, or packed RGB
///   [25:23] (unused content bits)
///   [41:26] style_id: u16 (0 = default style)
///   [43:42] wide: 0=narrow, 1=wide, 2=spacer_tail, 3=spacer_head
///   [44]    protected
///   [45]    hyperlink
///   [47:46] semantic_content
///   [63:48] _padding
#[repr(transparent)]
#[derive(Clone, Copy)]
pub struct RawCell(u64);

impl RawCell {
    /// Content tag: 0=codepoint, 1=codepoint_grapheme,
    /// 2=bg_color_palette, 3=bg_color_rgb
    #[inline(always)]
    pub fn content_tag(&self) -> u8 {
        (self.0 & 0x3) as u8
    }

    /// Primary codepoint (u21, valid when content_tag is 0 or 1).
    #[inline(always)]
    pub fn codepoint(&self) -> u32 {
        ((self.0 >> 2) & 0x1F_FFFF) as u32
    }

    /// Style ID (0 = default, no style lookup needed).
    #[inline(always)]
    pub fn style_id(&self) -> u16 {
        ((self.0 >> 26) & 0xFFFF) as u16
    }

    /// Wide property: 0=narrow, 1=wide, 2=spacer_tail, 3=spacer_head.
    #[inline(always)]
    pub fn wide(&self) -> u8 {
        ((self.0 >> 42) & 0x3) as u8
    }

    /// True if this cell is a spacer (tail or head).
    #[inline(always)]
    pub fn is_spacer(&self) -> bool {
        self.wide() >= 2
    }

    /// True if this cell has renderable text (codepoint or grapheme).
    #[inline(always)]
    pub fn has_text(&self) -> bool {
        let tag = self.content_tag();
        tag <= 1 // codepoint or codepoint_grapheme
    }

    /// True if this cell has a grapheme cluster (multi-codepoint).
    #[inline(always)]
    pub fn has_grapheme(&self) -> bool {
        self.content_tag() == 1
    }

    /// True if this cell is bg-only (no text, only background color).
    #[inline(always)]
    pub fn is_bg_only(&self) -> bool {
        self.content_tag() >= 2
    }

    /// Palette index for bg_color_palette cells (content_tag == 2).
    #[inline(always)]
    pub fn bg_palette_index(&self) -> u8 {
        ((self.0 >> 2) & 0xFF) as u8
    }

    /// RGB for bg_color_rgb cells (content_tag == 3).
    /// Returns (r, g, b).
    #[inline(always)]
    pub fn bg_rgb(&self) -> (u8, u8, u8) {
        let bits = (self.0 >> 2) as u32;
        let r = (bits & 0xFF) as u8;
        let g = ((bits >> 8) & 0xFF) as u8;
        let b = ((bits >> 16) & 0xFF) as u8;
        (r, g, b)
    }
}

// Verify RawCell layout matches Ghostty's page.Cell
const _: () = assert!(std::mem::size_of::<RawCell>() == 8);
const _: () = assert!(std::mem::align_of::<RawCell>() == 8);

/// Mirrors Zig's terminal.Style.Color tagged union layout.
/// Layout verified by Zig comptime assertions.
///
/// Tag values: 0=none, 1=palette, 2=rgb
/// For palette: r holds the palette index.
/// For rgb: r/g/b hold the color components.
/// For none: all payload bytes are undefined.
#[repr(C, align(4))]
#[derive(Clone, Copy)]
pub struct StyleColor {
    // This is also palette index when tag == 1
    pub r: u8,
    pub g: u8,
    pub b: u8,
    _pad: u8,
    // Offset 4
    pub tag: u8,
    // Offset 5-7
    _pad2: [u8; 3],
}

/// Zero-copy view into Ghostty's terminal.Style.
/// Layout verified by Zig comptime assertions on @sizeOf, @offsetOf,
/// and byte-level tagged union encoding.
#[repr(C, align(4))]
#[derive(Clone, Copy)]
pub struct CellStyle {
    pub fg: StyleColor,
    pub bg: StyleColor,
    pub underline: StyleColor,
    pub flags: u16,
    // DO NOT REMOVE — needed to match terminal.Style's align(4) trailing padding
    _pad: [u8; 2],
}

// Verify CellStyle and StyleColor layouts match Zig side
const _: () = assert!(std::mem::size_of::<StyleColor>() == 8);
const _: () = assert!(std::mem::size_of::<CellStyle>() == 28);
const _: () = assert!(std::mem::align_of::<CellStyle>() == 4);

impl CellStyle {
    pub fn is_bold(&self) -> bool {
        self.flags & (1 << 0) != 0
    }
    pub fn is_italic(&self) -> bool {
        self.flags & (1 << 1) != 0
    }
    pub fn is_faint(&self) -> bool {
        self.flags & (1 << 2) != 0
    }
    pub fn is_blink(&self) -> bool {
        self.flags & (1 << 3) != 0
    }
    pub fn is_inverse(&self) -> bool {
        self.flags & (1 << 4) != 0
    }
    pub fn is_invisible(&self) -> bool {
        self.flags & (1 << 5) != 0
    }
    pub fn is_strikethrough(&self) -> bool {
        self.flags & (1 << 6) != 0
    }
    pub fn is_overline(&self) -> bool {
        self.flags & (1 << 7) != 0
    }
    pub fn underline_style(&self) -> u8 {
        ((self.flags >> 8) & 0x7) as u8
    }
}

/// FFI-compatible mirror of Zig `[]const u21`.
/// Since @sizeOf(u21) == @sizeOf(u32), ptr points to u32 values.
#[repr(C)]
pub struct GraphemeSlice {
    pub ptr: *const u32,
    pub len: usize,
}

impl GraphemeSlice {
    /// Convert to a Rust slice, returning None if ptr is null or len is 0.
    ///
    /// # Safety
    ///
    /// Caller must ensure `ptr` is valid for `len` elements and that the
    /// resulting slice does not outlive the backing storage.
    pub unsafe fn as_slice(&self) -> Option<&[u32]> {
        if self.ptr.is_null() || self.len == 0 {
            None
        } else {
            Some(unsafe { std::slice::from_raw_parts(self.ptr, self.len) })
        }
    }
}

/// Scrollbar positioning info from Ghostty's PageList.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ScrollbarInfo {
    /// Total rows in page list (scrollback + active area).
    pub total_rows: u64,
    /// Row offset of viewport from top of scrollback.
    pub top_row: u64,
    /// Number of visible viewport rows.
    pub viewport_rows: u64,
}

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
