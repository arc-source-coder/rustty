use ghostty_vt::{ColorRGB, FlatCell};
use gpui::{Hsla, Rgba};
use rapidhash::v3::rapidhash_v3;

pub fn palette_hash(palette: &[ghostty_vt::ColorRGB; 256]) -> u64 {
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

/// Pre-resolved palette colors. Built once per frame from the
/// snapshot's raw palette data. Avoids per-cell conversion.
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

/// Resolve the foreground color from a FlatCell.
///
/// - type 0: default foreground
/// - type 1: palette color
/// - type 2: direct RGB
pub fn resolve_fg(cell: &FlatCell, palette: &PaletteCache, default_fg: Hsla) -> Hsla {
    match cell.fg_color_type {
        0 => default_fg,
        1 => palette.get(cell.fg_palette),
        _ => rgb_to_hsla(cell.fg_r, cell.fg_g, cell.fg_b),
    }
}

/// Resolve the background color from a FlatCell.
pub fn resolve_bg(cell: &FlatCell, palette: &PaletteCache, default_bg: Hsla) -> Hsla {
    match cell.bg_color_type {
        0 => default_bg,
        1 => palette.get(cell.bg_palette),
        _ => rgb_to_hsla(cell.bg_r, cell.bg_g, cell.bg_b),
    }
}

// --- Style flag extraction ---
// FlatCell.style_flags is a packed u16 bitfield (matches Ghostty Style.Flags).

const FLAG_BOLD: u16 = 1 << 0;
const FLAG_ITALIC: u16 = 1 << 1;
const FLAG_FAINT: u16 = 1 << 2;
const FLAG_INVERSE: u16 = 1 << 4;
const FLAG_INVISIBLE: u16 = 1 << 5;
const FLAG_STRIKETHROUGH: u16 = 1 << 6;
const UNDERLINE_MASK: u16 = 0b111 << 8;
const UNDERLINE_SHIFT: u16 = 8;

pub fn is_bold(cell: &FlatCell) -> bool {
    cell.style_flags & FLAG_BOLD != 0
}
pub fn is_italic(cell: &FlatCell) -> bool {
    cell.style_flags & FLAG_ITALIC != 0
}
pub fn is_faint(cell: &FlatCell) -> bool {
    cell.style_flags & FLAG_FAINT != 0
}
pub fn is_inverse(cell: &FlatCell) -> bool {
    cell.style_flags & FLAG_INVERSE != 0
}
pub fn is_invisible(cell: &FlatCell) -> bool {
    cell.style_flags & FLAG_INVISIBLE != 0
}
pub fn is_strikethrough(cell: &FlatCell) -> bool {
    cell.style_flags & FLAG_STRIKETHROUGH != 0
}

/// Underline variant: 0=none, 1=single, 2=double, 3=curly, 4=dotted, 5=dashed
pub fn underline_style(cell: &FlatCell) -> u8 {
    ((cell.style_flags & UNDERLINE_MASK) >> UNDERLINE_SHIFT) as u8
}
