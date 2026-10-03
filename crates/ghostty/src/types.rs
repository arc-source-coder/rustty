use utils::asserts::unreachable;

use crate::ffi::ColorRGB;
use core::ffi::c_void;

/// Effects of a selection gesture, captured under the terminal lock.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct SelectionUpdate {
    /// Selection or viewport changed; the caller should wake the renderer.
    pub needs_redraw: bool,
    /// Whether the caller should keep scheduling selection autoscroll ticks.
    pub autoscroll: bool,
}

pub type BellCallback = unsafe extern "C" fn(userdata: *mut c_void);
pub type TitleCallback = unsafe extern "C" fn(userdata: *mut c_void, ptr: *const u8, len: usize);
pub type OutputCallback = unsafe extern "C" fn(userdata: *mut c_void);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContentTag {
    Codepoint,
    CodepointGrapheme,
    BgColorPalette(usize),
    BgColorRgb(ColorRGB),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Width {
    Narrow,
    Wide,
    SpacerTail,
    SpacerHead,
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
    #[inline]
    pub const fn content_tag(self) -> ContentTag {
        match self.0 & 0x3 {
            0 => ContentTag::Codepoint,
            1 => ContentTag::CodepointGrapheme,
            2 => {
                let idx = ((self.0 >> 2) & 0xFF) as usize;
                ContentTag::BgColorPalette(idx)
            }
            3 => ContentTag::BgColorRgb(ColorRGB::from_raw((self.0 >> 2) as u32)),
            _ => unreachable(),
        }
    }

    /// Primary codepoint (u21), or zero when this cell stores a background color.
    #[inline]
    pub const fn codepoint(self) -> u32 {
        match self.content_tag() {
            ContentTag::Codepoint | ContentTag::CodepointGrapheme => {
                // Strip the upper 11 bits since Ghostty stores a u21.
                ((self.0 >> 2) & 0x1F_FFFF) as u32
            }
            ContentTag::BgColorPalette(_) | ContentTag::BgColorRgb(_) => 0,
        }
    }

    /// The width in grid cells that this cell takes up.
    pub const fn grid_width(self) -> u8 {
        match self.width() {
            Width::Narrow | Width::SpacerHead | Width::SpacerTail => 1,
            Width::Wide => 2,
        }
    }

    /// Style ID (0 = default, no style lookup needed).
    #[inline]
    pub const fn style_id(self) -> u16 {
        ((self.0 >> 26) & 0xFFFF) as u16
    }

    /// Wide property: 0=narrow, 1=wide, 2=spacer_tail, 3=spacer_head.
    #[inline]
    pub const fn width(self) -> Width {
        match (self.0 >> 42) & 0x3 {
            0 => Width::Narrow,
            1 => Width::Wide,
            2 => Width::SpacerTail,
            3 => Width::SpacerHead,
            _ => unreachable(),
        }
    }

    /// True if this cell has no text or styling
    #[inline]
    pub fn is_empty(self) -> bool {
        match self.content_tag() {
            ContentTag::Codepoint | ContentTag::CodepointGrapheme => {
                !self.has_text() && (self.width() == Width::Narrow)
            }
            ContentTag::BgColorPalette(_) | ContentTag::BgColorRgb(_) => false,
        }
    }

    /// True if this cell has renderable text (codepoint or grapheme).
    #[inline]
    pub const fn has_text(self) -> bool {
        match self.content_tag() {
            ContentTag::Codepoint | ContentTag::CodepointGrapheme => self.codepoint() != 0,
            ContentTag::BgColorPalette(_) | ContentTag::BgColorRgb(_) => false,
        }
    }

    #[inline]
    pub const fn has_styling(self) -> bool {
        self.style_id() != 0
    }

    /// True if this cell has a grapheme cluster (multi-codepoint).
    #[inline]
    pub fn has_grapheme(self) -> bool {
        self.content_tag() == ContentTag::CodepointGrapheme
    }
}

// Verify RawCell layout matches Ghostty's page.Cell
const _: () = assert!(size_of::<RawCell>() == 8);
const _: () = assert!(align_of::<RawCell>() == 8);

#[repr(C)]
pub struct MouseMode {
    pub is_alternate_screen: bool,
    pub is_mouse_reporting: bool,
    pub is_mouse_alternate_scroll: bool,
    pub is_mouse_shift_capture: bool,
}

/// Mirrors Ghostty's `renderer.size.Size` ABI.
#[repr(C)]
#[derive(Default, Eq, PartialEq)]
pub struct TerminalDimensions {
    pub screen: ScreenSize,
    pub cell: CellSize,
    pub padding: Padding,
}

#[repr(C)]
#[derive(Copy, Clone, Default, Eq, PartialEq)]
pub struct ScreenSize {
    pub width: u32,
    pub height: u32,
}

#[repr(C)]
#[derive(Copy, Clone, Default, Eq, PartialEq)]
pub struct CellSize {
    pub width: u32,
    pub height: u32,
}

#[repr(C)]
#[derive(Default, Eq, PartialEq)]
pub struct Padding {
    pub top: u32,
    pub bottom: u32,
    pub right: u32,
    pub left: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CursorCoordinate {
    pub x: u16,
    pub y: u32,
}

/// Scrollbar positioning info from Ghostty's PageList.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScrollbarInfo {
    /// Total rows in the page list (scrollback + active area).
    pub total_rows: u64,
    /// Row offset of the viewport from the top of scrollback.
    pub top_row: u64,
    /// Number of rows visible in the viewport (== terminal rows).
    pub viewport_rows: u64,
}

#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dirty {
    Clean = 0,
    Partial = 1,
    Full = 2,
}
