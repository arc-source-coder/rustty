use std::mem::MaybeUninit;

use utils::asserts::unreachable;

use crate::types::{ContentTag, CursorCoordinate, RawCell};

/// Rust mirror of Zig `RenderState.Cursor.Viewport`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CursorViewport {
    pub x: u16,
    pub y: u16,
    pub wide_tail: bool,
}

/// Rust mirror of Zig `?RenderState.Cursor.Viewport`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct OptionalCursorViewport {
    viewport: MaybeUninit<CursorViewport>,
    tag: u8,
}

impl OptionalCursorViewport {
    #[inline]
    pub const fn into_option(self) -> Option<CursorViewport> {
        if self.tag == 1 {
            return unsafe { Some(self.viewport.assume_init()) };
        }
        None
    }
}

#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CursorVisualStyle {
    Bar = 0,
    Block = 1,
    Underline = 2,
    BlockHollow = 3,
}

/// Mirror of Ghostty's `terminal.RenderState.Cursor`.
#[repr(C)]
pub struct RenderCursor {
    // Field order is memory order, not Ghostty declaration order.
    pub cell: RawCell,
    pub active: CursorCoordinate,
    pub style: CellStyle,
    pub viewport: OptionalCursorViewport,
    pub visual_style: CursorVisualStyle,
    pub password_input: bool,
    pub visible: bool,
    pub blinking: bool,
}

const _: () = assert!(size_of::<CursorViewport>() == 6);
const _: () = assert!(align_of::<CursorViewport>() == 2);
const _: () = assert!(size_of::<OptionalCursorViewport>() == 8);
const _: () = assert!(align_of::<OptionalCursorViewport>() == 2);
const _: () = assert!(size_of::<RenderCursor>() == 56);
const _: () = assert!(align_of::<RenderCursor>() == 8);

/// Zero-copy view into Ghostty's `color.RGB` (`packed struct(u24)`).
///
/// Zig's `packed struct(u24)` occupies 4 bytes in memory (padded to u32).
/// Bit layout (little-endian): `[7:0] r`, `[15:8] g`, `[23:16] b`, `[31:24] padding`.
/// This lets us read `[256]color.RGB` as `[256]ColorRGB` via pointer cast.
#[repr(C, align(4))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct ColorRGB([u8; 3]);

impl ColorRGB {
    /// Construct from individual components.
    #[inline]
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self([r, g, b])
    }

    pub const fn from_raw(value: u32) -> Self {
        let bytes = value.to_le_bytes();
        Self([bytes[0], bytes[1], bytes[2]])
    }

    #[inline]
    pub const fn from_array(value: [u8; 3]) -> Self {
        Self(value)
    }

    #[inline]
    pub const fn r(self) -> u8 {
        self.0[0]
    }

    #[inline]
    pub const fn g(self) -> u8 {
        self.0[1]
    }

    #[inline]
    pub const fn b(self) -> u8 {
        self.0[2]
    }

    #[inline]
    pub const fn to_u32(self) -> u32 {
        let rg = u16::from_le_bytes([self.0[0], self.0[1]]) as u32;
        rg | ((self.0[2] as u32) << 16)
    }

    #[inline]
    pub const fn to_bytes(self) -> [u8; 3] {
        self.0
    }

    #[inline]
    pub const fn with_alpha(self, alpha: u8) -> [u8; 4] {
        [self.0[0], self.0[1], self.0[2], alpha]
    }
}

/// Zero-copy Rust view of Zig `u21` storage.
///
/// Zig uses a four-byte stride and initializes the first three bytes.
/// Byte 3 remains implicit padding.
#[repr(C, align(4))]
#[derive(Clone, Copy)]
pub struct U21([u8; 3]);

impl U21 {
    #[inline]
    pub const fn get(self) -> u32 {
        let low = u16::from_le_bytes([self.0[0], self.0[1]]) as u32;
        low | ((self.0[2] as u32) << 16)
    }
}

const _: () = assert!(size_of::<U21>() == 4);
const _: () = assert!(align_of::<U21>() == 4);

/// Mirrors Zig's RenderState.Colors tagged union layout.
/// Layout verified by Zig comptime assertions.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct OptionalColorRGB {
    rgb: MaybeUninit<ColorRGB>,
    tag: u8,
}

impl OptionalColorRGB {
    #[inline]
    pub const fn into_option(self) -> Option<ColorRGB> {
        if self.tag == 1 {
            return unsafe { Some(self.rgb.assume_init()) };
        }
        None
    }
}

#[repr(C)]
pub struct RenderColors {
    pub background: ColorRGB,
    pub foreground: ColorRGB,
    cursor: OptionalColorRGB,
    pub palette: [ColorRGB; 256],
}

impl RenderColors {
    #[inline]
    pub const fn cursor_color(&self) -> Option<ColorRGB> {
        self.cursor.into_option()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Color {
    None,
    Palette(u8),
    Rgb(ColorRGB),
}

/// Mirrors Zig's terminal.Style.Color tagged union layout.
/// Layout verified by Zig comptime assertions.
///
/// Tag values: 0=none, 1=palette, 2=rgb
/// For palette: payload[0] holds the palette index.
/// For rgb: payload[0..=2] holds the color components.
/// For none: all payload bytes are undefined.
#[repr(C, align(4))]
#[derive(Clone, Copy)]
pub struct StyleColor {
    payload: [MaybeUninit<u8>; 4],
    tag: u8,
    // Bytes 5-7 are implicit Rust padding, matching Zig
}

impl PartialEq for StyleColor {
    fn eq(&self, other: &Self) -> bool {
        if self.tag != other.tag {
            return false;
        }

        match self.tag {
            0 => true,
            1 => unsafe { self.payload[0].assume_init() == other.payload[0].assume_init() },
            2 => unsafe { self.rgb_payload() == other.rgb_payload() },
            // Safety: Zig comptime asserts verify the enum only has 3 variants
            _ => unreachable(),
        }
    }
}

impl Eq for StyleColor {}

impl StyleColor {
    pub const NONE: Self = Self {
        payload: [MaybeUninit::new(0); 4],
        tag: 0,
    };

    /// Get the RGB payload from the `StyleColor`
    ///
    /// # Safety
    ///
    /// Caller guarantees the active tag is RGB, which initializes payload bytes 0 through 2.
    #[inline]
    const unsafe fn rgb_payload(&self) -> &[u8; 3] {
        unsafe { &*self.payload.as_ptr().cast::<[u8; 3]>() }
    }

    #[inline]
    pub const fn color(self) -> Color {
        match self.tag {
            0 => Color::None,
            1 => unsafe { Color::Palette(self.payload[0].assume_init()) },
            2 => unsafe { Color::Rgb(ColorRGB::from_array(*self.rgb_payload())) },
            // Safety: Zig comptime asserts verify the enum only has 3 variants
            _ => unreachable(),
        }
    }
}

/// Bright palette offset — Ghostty's `color.Name.bright_black` == 8.
const BRIGHT_PALETTE_OFFSET: usize = 8;

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum BoldColor {
    None,
    Color(ColorRGB),
    Bright,
}

#[repr(u8)]
#[derive(Clone, Copy, Eq, PartialEq)]
pub enum UnderlineStyle {
    None = 0,
    Single = 1,
    Double = 2,
    Curly = 3,
    Dotted = 4,
    Dashed = 5,
}

/// Zero-copy view into Ghostty's terminal.Style.
/// Layout verified by Zig comptime assertions on @sizeOf, @offsetOf,
/// and byte-level tagged union encoding.
#[repr(C, align(4))]
#[derive(Clone, PartialEq, Eq)]
pub struct CellStyle {
    pub fg_color: StyleColor,
    pub bg_color: StyleColor,
    pub underline: StyleColor,
    pub flags: u16,
}

// Verify CellStyle and StyleColor layouts match Zig side
const _: () = assert!(size_of::<StyleColor>() == 8);
const _: () = assert!(size_of::<CellStyle>() == 28);
const _: () = assert!(align_of::<CellStyle>() == 4);

impl CellStyle {
    pub const DEFAULT: &'static Self = &Self {
        fg_color: StyleColor::NONE,
        bg_color: StyleColor::NONE,
        underline: StyleColor::NONE,
        flags: 0,
    };

    #[inline]
    pub const fn is_bold(&self) -> bool {
        self.flags & (1 << 0) != 0
    }

    #[inline]
    pub const fn is_italic(&self) -> bool {
        self.flags & (1 << 1) != 0
    }

    #[inline]
    pub const fn is_faint(&self) -> bool {
        self.flags & (1 << 2) != 0
    }

    #[inline]
    pub const fn is_blink(&self) -> bool {
        self.flags & (1 << 3) != 0
    }

    #[inline]
    pub const fn is_inverse(&self) -> bool {
        self.flags & (1 << 4) != 0
    }

    #[inline]
    pub const fn is_invisible(&self) -> bool {
        self.flags & (1 << 5) != 0
    }

    #[inline]
    pub const fn is_strikethrough(&self) -> bool {
        self.flags & (1 << 6) != 0
    }

    #[inline]
    pub const fn is_overline(&self) -> bool {
        self.flags & (1 << 7) != 0
    }

    #[inline]
    pub const fn underline_style(&self) -> UnderlineStyle {
        match (self.flags >> 8) & 0x7 {
            0 => UnderlineStyle::None,
            1 => UnderlineStyle::Single,
            2 => UnderlineStyle::Double,
            3 => UnderlineStyle::Curly,
            4 => UnderlineStyle::Dotted,
            5 => UnderlineStyle::Dashed,
            _ => unreachable(),
        }
    }

    // TODO: doc comments
    #[inline]
    pub const fn bg(&self, cell: &RawCell, palette: &[ColorRGB; 256]) -> Option<ColorRGB> {
        match cell.content_tag() {
            ContentTag::BgColorPalette(idx) => Some(palette[idx]),
            ContentTag::BgColorRgb(rgb) => Some(rgb),
            _ => match self.bg_color.color() {
                Color::None => None,
                Color::Palette(idx) => Some(palette[idx as usize]),
                Color::Rgb(rgb) => Some(rgb),
            },
        }
    }

    #[inline]
    pub fn fg(&self, default: ColorRGB, pal: &[ColorRGB; 256], bold_color: BoldColor) -> ColorRGB {
        let is_bold = self.is_bold();
        match self.fg_color.color() {
            Color::None => match bold_color {
                BoldColor::Color(color) if is_bold => color,
                _ => default,
            },
            Color::Palette(idx) => {
                let idx = idx as usize;
                if is_bold && bold_color != BoldColor::None && idx < BRIGHT_PALETTE_OFFSET {
                    return pal[idx + BRIGHT_PALETTE_OFFSET];
                }
                pal[idx]
            }
            Color::Rgb(rgb) => match bold_color {
                BoldColor::Color(value) if is_bold && rgb == default => value,
                _ => rgb,
            },
        }
    }
}

/// Rust mirror of Zig `?[2]u16`, used by `RenderState.Row.selection`.
///
/// `tag == 0` means no selection; `tag == 1` means `range` is present.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct OptionalSelection {
    range: MaybeUninit<[u16; 2]>,
    tag: u8,
}

impl OptionalSelection {
    #[inline]
    pub const fn into_option(self) -> Option<[u16; 2]> {
        if self.tag == 1 {
            return unsafe { Some(self.range.assume_init()) };
        }
        None
    }
}

/// Zig `?[2]u16`, used by `RenderState.Row.selection`.
pub const OPTIONAL_SELECTION_SIZE: usize = 6;
pub const OPTIONAL_SELECTION_ALIGN: usize = 2;

const _: () = assert!(size_of::<OptionalSelection>() == OPTIONAL_SELECTION_SIZE);
const _: () = assert!(align_of::<OptionalSelection>() == OPTIONAL_SELECTION_ALIGN);
