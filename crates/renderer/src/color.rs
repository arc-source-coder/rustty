use ghostty_vt::{CellStyle, ColorRGB};
use gpui::{Hsla, Rgba};
use rapidhash::v3::rapidhash_v3;

pub fn palette_hash(palette: &[ColorRGB; 256]) -> u64 {
    // Safety: ColorRGB is #[repr(C)], 3 × u8, no padding (size == 3).
    // Pod + Zeroable derived in ghostty-vt. cast_slice is safe for align-1 types.
    let bytes: &[u8] = bytemuck::cast_slice(palette);
    rapidhash_v3(bytes)
}

/// Convert a raw RGB triplet to GPUI Hsla.
pub fn rgb_to_hsla(r: u8, g: u8, b: u8) -> Hsla {
    Rgba {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a: 1.0,
    }
    .into()
}

/// Resolve a ColorRGB (from ghostty_vt) to Hsla.
pub fn color_rgb_to_hsla(c: ColorRGB) -> Hsla {
    rgb_to_hsla(c.r, c.g, c.b)
}

/// Pre-resolved palette colors. Built once when the palette changes,
/// then cached in `TerminalElementState` across frames.
/// Avoids 256 × float conversions per frame when palette is unchanged.
#[derive(Clone)]
pub struct PaletteCache {
    colors: [Hsla; 256],
}

impl PaletteCache {
    pub fn from_raw(palette: &[ColorRGB; 256]) -> Self {
        let mut colors = [Hsla::default(); 256];
        for i in 0..256 {
            colors[i] = color_rgb_to_hsla(palette[i]);
        }
        Self { colors }
    }

    pub fn get(&self, index: u8) -> Hsla {
        self.colors[index as usize]
    }
}

/// Resolve the foreground color from a CellStyle.
/// Returns default_fg when style is None (default style, style_id == 0).
pub fn resolve_fg(style: Option<&CellStyle>, palette: &PaletteCache, default_fg: Hsla) -> Hsla {
    match style {
        None => default_fg,
        Some(s) => match s.fg.tag {
            1 => palette.get(s.fg.r),
            2 => rgb_to_hsla(s.fg.r, s.fg.g, s.fg.b),
            _ => default_fg,
        },
    }
}

/// Resolve the background color from a CellStyle (style_id != 0 path).
/// Does NOT handle bg_only cells — those are decoded directly from RawCell
/// in build_row_runs to avoid stale style SOA slots.
pub fn resolve_bg(style: Option<&CellStyle>, palette: &PaletteCache, default_bg: Hsla) -> Hsla {
    match style {
        None => default_bg,
        Some(s) => match s.bg.tag {
            1 => palette.get(s.bg.r),
            2 => rgb_to_hsla(s.bg.r, s.bg.g, s.bg.b),
            _ => default_bg,
        },
    }
}
