use font::metrics::FontMetrics;
use font::types::FontIndex;

use crate::font::atlas::AtlasFormat;

/// Parameters used to match DirectWrite's grayscale text rendering in the shader
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq)]
pub struct TextRenderingParams {
    /// Polynomial coefficients derived from the system DirectWrite gamma value.
    pub gamma_ratios: [f32; 4],
    /// System contrast adjustment for grayscale antialiasing.
    pub grayscale_enhanced_contrast: f32,
    /// Padding to complete the 16-byte HLSL constant-buffer row.
    _pad: [f32; 3],
}

impl TextRenderingParams {
    pub fn correction(self, color: [f32; 3]) -> [f32; 4] {
        let intensity = color[0] * 0.25 + color[1] * 0.50 + color[2] * 0.25;
        let p = self.gamma_ratios[0] * intensity + self.gamma_ratios[1];
        let q = self.gamma_ratios[2] * intensity + self.gamma_ratios[3];
        let contrast_factor =
            (3.0 - 4.0 * (color[0] * 0.30 + color[1] * 0.59 + color[2] * 0.11)).clamp(0.0, 1.0);

        [self.grayscale_enhanced_contrast * contrast_factor, -p, p - q, 1.0 + q]
    }
}

/// Atlas placement metadata for a rasterized face glyph or sprite.
/// Contains the data needed to build a quad instance at draw time.
///
/// Ghostty reference: `Glyph` struct in `font/Glyph.zig`.
/// WT parallel: `AtlasGlyphEntry` (offset, size, texcoord, shadingType).
#[derive(Default, Clone, Copy)]
pub struct Glyph {
    /// Pixel width of the rasterized glyph.
    pub width: u16,
    /// Pixel height of the rasterized glyph.
    pub height: u16,
    /// Horizontal offset from the cell's left edge to the glyph's left edge.
    pub offset_x: i32,
    /// Vertical offset from the cell's top edge to the glyph's top edge.
    /// This value is in screen space. Positive Y means move down.
    pub offset_y: i32,
    /// X coordinate of the top-left corner in the atlas texture.
    pub atlas_x: u16,
    /// Y coordinate of the top-left corner in the atlas texture.
    pub atlas_y: u16,

    pub atlas: AtlasFormat,
}

/// Render options that affect rasterization output and therefore must be
/// accounted for by caching. Only `grid_width` (2 bits) is packed into the key;
/// metrics belong to the grid that scopes the rasterizer and its cache.
#[derive(Clone, Copy)]
pub struct RenderOptions<'a> {
    pub metrics: &'a FontMetrics,
    /// Number of grid cells this glyph occupies (0 = unset, 1–3 valid).
    /// Ghostty: `RenderOptions.cell_width: ?u2`.
    pub grid_width: u8,
    // TODO: explore adding `Constraint` support
}

/// Position-independent glyph cache key packed into a single `u64`.
/// Scoped to one font grid. Absolute position is excluded; placement uses
/// whole-pixel shaper offsets and integer quad positions.
///
/// Ghostty reference: `GlyphKey.Packed` — a `packed struct(u64)`
/// of `(Collection.Index, glyph: u32, opts: packed struct(u16))`.
///
/// ## Bit layout
///
///   `[15:0]`  `FontIndex` (16 bits — 3 style + 13 index)
///   `[47:16]` `glyph_index` (32 bits)
///   `[49:48]` grid width (2 bits)
///   `[63:50]` reserved (14 bits)
///
#[derive(Clone, Copy, Eq, PartialEq, Hash)]
#[repr(transparent)]
pub struct GlyphKey(u64);

impl GlyphKey {
    /// Construct a packed key from components.
    #[inline]
    pub fn new(font_index: FontIndex, glyph_index: u32, grid_width: u8) -> Self {
        Self(
            u64::from(font_index.to_u16())
                | (u64::from(glyph_index) << 16)
                | (u64::from(grid_width & 0b11) << 48),
        )
    }

    /// Extract the dense font face id.
    #[inline]
    pub const fn font_index(self) -> FontIndex {
        // SAFETY: FontIndex is repr(transparent) over u16, and the
        // low 16 bits were produced by FontIndex::to_u16().
        unsafe { std::mem::transmute(self.0 as u16) }
    }

    /// Extract the glyph index.
    #[inline]
    pub const fn glyph_index(self) -> u32 {
        (self.0 >> 16) as u32
    }

    /// Extract the packed render options as a raw u16.
    #[inline]
    pub const fn packed_options(self) -> u16 {
        (self.0 >> 48) as u16
    }

    #[inline]
    pub const fn grid_width(self) -> u8 {
        self.packed_options() as u8 & 0b11
    }
}

const _: () = assert!(size_of::<GlyphKey>() == 8);
