use std::ffi::{c_int, c_void};
use std::mem::MaybeUninit;
use std::ptr::NonNull;

#[repr(u32)]
#[derive(Clone, Copy, Eq, PartialEq)]
pub enum Sprite {
    Underline = 0x200000,
    UnderlineDouble,
    UnderlineDotted,
    UnderlineDashed,
    UnderlineCurly,
    Strikethrough,
    Overline,
    CursorRect,
    CursorHollowRect,
    CursorBar,
    CursorUnderline,
}

pub struct SpriteRasterizer {
    handle: NonNull<c_void>,
}

/// FFI mirror of `FontMetrics`
#[repr(C)]
pub struct SpriteMetrics {
    pub cell_width: u32,
    pub cell_height: u32,
    pub cell_baseline: u32,

    pub underline_position: u32,
    pub underline_thickness: u32,

    pub strikethrough_position: u32,
    pub strikethrough_thickness: u32,

    pub overline_position: i32,
    pub overline_thickness: u32,

    pub box_thickness: u32,
    pub cursor_thickness: u32,
    pub cursor_height: u32,

    pub icon_height: f64,
    pub icon_height_single: f64,
    pub face_width: f64,
    pub face_height: f64,
    pub face_y: f64,
}

#[derive(Debug)]
pub enum SpriteError {
    InvalidCellWidth,
    InvalidBitmap,
}

impl std::fmt::Display for SpriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidBitmap => f.write_str("invalid bitmap"),
            Self::InvalidCellWidth => f.write_str("invalid cell width"),
        }
    }
}

impl std::error::Error for SpriteError {}

impl SpriteRasterizer {
    pub fn new(metrics: SpriteMetrics) -> Result<Self, SpriteError> {
        let ptr = unsafe { ghostty_sprite_rasterizer_new(&raw const metrics) };
        let handle = NonNull::new(ptr).ok_or(SpriteError::InvalidBitmap)?;

        Ok(Self { handle })
    }

    #[inline]
    pub fn rasterize(&mut self, cp: u32, cell_width: u8) -> Result<SpriteBitmap<'_>, SpriteError> {
        if cell_width > 3 {
            return Err(SpriteError::InvalidCellWidth);
        }

        let mut raw: MaybeUninit<RawSpriteBitmap> = MaybeUninit::uninit();
        let result = unsafe { ghostty_sprite_rasterize(self.handle, cp, cell_width, &mut raw) };
        if result != 0 {
            return Err(SpriteError::InvalidBitmap);
        }

        // Safety: If the call succeeded, Zig initializes the entire bitmap.
        let raw = unsafe { raw.assume_init() };

        let len = if raw.width == 0 || raw.height == 0 {
            0
        } else {
            raw.stride as usize * (raw.height - 1) as usize + raw.width as usize
        };

        if raw.pixels.is_null() || raw.stride < raw.width {
            return Err(SpriteError::InvalidBitmap);
        }

        let pixels = unsafe { std::slice::from_raw_parts(raw.pixels, len) };

        Ok(SpriteBitmap {
            pixels,
            stride: raw.stride,
            width: raw.width,
            height: raw.height,
            offset_x: raw.offset_x,
            offset_y: raw.offset_y,
        })
    }
}

impl Drop for SpriteRasterizer {
    fn drop(&mut self) {
        unsafe { ghostty_sprite_rasterizer_free(self.handle) };
    }
}

#[inline]
pub fn has_codepoint(cp: u32) -> bool {
    unsafe { ghostty_sprite_has_codepoint(cp) }
}

#[repr(C)]
struct RawSpriteBitmap {
    pixels: *const u8,
    stride: u32,
    width: u32,
    height: u32,
    offset_x: i32,
    offset_y: i32,
}

/// Cropped 8-bit alpha pixels borrowed from the sprite rasterizer.
/// The borrow prevents another rasterization or destruction of
/// the rasterizer while the bitmap is in use.
pub struct SpriteBitmap<'a> {
    pub pixels: &'a [u8],
    /// Source row pitch in bytes, which may exceed the cropped width.
    pub stride: u32,
    /// Cropped width in pixels.
    pub width: u32,
    /// Cropped height in pixels.
    pub height: u32,
    /// Crop origin relative to the cell's left edge, positive rightward.
    pub offset_x: i32,
    /// Crop origin relative to the cell's top edge, positive downward.
    pub offset_y: i32,
}

unsafe extern "C" {
    fn ghostty_sprite_has_codepoint(cp: u32) -> bool;
    fn ghostty_sprite_rasterizer_new(metrics: *const SpriteMetrics) -> *mut c_void;
    fn ghostty_sprite_rasterizer_free(ptr: NonNull<c_void>);

    fn ghostty_sprite_rasterize(
        ptr: NonNull<c_void>,
        cp: u32,
        cell_width: u8,
        out: &mut MaybeUninit<RawSpriteBitmap>,
    ) -> c_int;
}
