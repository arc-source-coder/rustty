use gpui::{
    Font, FontStyle, FontWeight, Hsla, Pixels, StrikethroughStyle, TextRun, UnderlineStyle, px,
};

use crate::color::{self, PaletteCache, rgb_to_hsla};
use ghostty_vt::{CellStyle, RawCell, RenderFrame};

/// A positioned text run ready for painting.
#[derive(Clone)]
pub struct PositionedTextRun {
    /// Column where this run starts.
    pub start_col: u16,
    /// Row (viewport-relative).
    pub row: u16,
    /// The text content.
    pub text: String,
    /// Number of grid cells this run covers.
    pub cell_count: u16,
    /// The style for this run (font, color, decorations).
    pub style: TextRun,
    /// Font size for shaping.
    pub font_size: Pixels,
}

/// A background rect for a span of non-default background color.
#[derive(Clone, Copy)]
pub struct BgRect {
    pub col: u16,
    pub row: u16,
    pub width: u16,
    pub color: Hsla,
}

/// Result of building runs for a single row.
pub struct RowRuns {
    pub text_runs: Vec<PositionedTextRun>,
    pub bg_rects: Vec<BgRect>,
}

/// The four font variants a terminal cell can use.
/// Pre-built once per frame so the inner cell loop never clones Arc-backed fields.
pub struct FontVariants {
    normal: Font,
    bold: Font,
    italic: Font,
    bold_italic: Font,
}

impl FontVariants {
    pub fn from_base(base: &Font) -> Self {
        // Each Font clones three Arc-backed fields (family, features, fallbacks).
        // Doing this four times here instead of once per cell saves O(cols) atomic ops.
        FontVariants {
            normal: base.clone(),
            bold: Font {
                weight: FontWeight::BOLD,
                style: FontStyle::Normal,
                family: base.family.clone(),
                features: base.features.clone(),
                fallbacks: base.fallbacks.clone(),
            },
            italic: Font {
                weight: base.weight,
                style: FontStyle::Italic,
                family: base.family.clone(),
                features: base.features.clone(),
                fallbacks: base.fallbacks.clone(),
            },
            bold_italic: Font {
                weight: FontWeight::BOLD,
                style: FontStyle::Italic,
                family: base.family.clone(),
                features: base.features.clone(),
                fallbacks: base.fallbacks.clone(),
            },
        }
    }

    fn select(&self, bold: bool, italic: bool) -> &Font {
        match (bold, italic) {
            (false, false) => &self.normal,
            (true, false) => &self.bold,
            (false, true) => &self.italic,
            (true, true) => &self.bold_italic,
        }
    }
}

/// Minimal style key for merge decisions — avoids building a full TextRun
/// and cloning a Font on every cell just to check compatibility.
#[derive(Clone, Copy, PartialEq)]
struct RunKey {
    bold: bool,
    italic: bool,
    fg: Hsla,
    underline_style: u8,
    strikethrough: bool,
}

impl RunKey {
    fn from_cell(style: Option<&CellStyle>, fg: Hsla) -> Self {
        RunKey {
            bold: style.map_or(false, |s| s.is_bold()),
            italic: style.map_or(false, |s| s.is_italic()),
            fg,
            underline_style: style.map_or(0, |s| s.underline_style()),
            strikethrough: style.map_or(false, |s| s.is_strikethrough()),
        }
    }
}

/// Build text runs and background rects for a single row.
///
/// Takes zero-copy slices into Zig memory. Grapheme lookups are
/// done on-demand via the RenderFrame (rare path).
pub fn build_row_runs(
    raw_cells: &[RawCell],
    styles: &[CellStyle],
    row: u16,
    palette: &PaletteCache,
    default_fg: Hsla,
    default_bg: Hsla,
    fonts: &FontVariants,
    font_size: Pixels,
    frame: &RenderFrame,
) -> RowRuns {
    let mut text_runs: Vec<PositionedTextRun> = Vec::new();
    let mut bg_rects: Vec<BgRect> = Vec::new();
    let mut current_run: Option<PositionedTextRun> = None;
    let mut current_key: Option<RunKey> = None;

    for (col_idx, raw) in raw_cells.iter().enumerate() {
        let col = col_idx as u16;

        // Skip wide-char spacer tails
        if raw.wide() == 2 {
            continue;
        }

        // Resolve style (only access styles array when needed).
        // bg_only cells (content_tag >= 2) carry their bg color in the raw
        // cell's content bits — NOT in the style slot, which may be stale
        // (the style loop in Ghostty's render_update is skipped for rows where
        // managedMemory() == false, leaving old data in the style SOA).
        // Decode them directly here to avoid reading a garbage style entry.
        let bg_only_color: Option<Hsla> = if raw.is_bg_only() {
            Some(match raw.content_tag() {
                2 => palette.get(raw.bg_palette_index()),
                3 => {
                    let (r, g, b) = raw.bg_rgb();
                    rgb_to_hsla(r, g, b)
                }
                _ => default_bg,
            })
        } else {
            None
        };

        let style: Option<&CellStyle> = if raw.style_id() != 0 {
            Some(&styles[col_idx])
        } else {
            None
        };

        // Resolve colors
        let mut fg = color::resolve_fg(style, palette, default_fg);
        let mut bg = bg_only_color.unwrap_or_else(|| color::resolve_bg(style, palette, default_bg));

        if style.map_or(false, |s| s.is_inverse()) {
            std::mem::swap(&mut fg, &mut bg);
        }
        if style.map_or(false, |s| s.is_invisible()) {
            fg = bg;
        }
        if style.map_or(false, |s| s.is_faint()) {
            fg.a *= 0.7;
        }

        // Background rect (non-default bg)
        if bg != default_bg {
            if let Some(last) = bg_rects.last_mut() {
                if last.row == row && last.color == bg && last.col + last.width == col {
                    last.width += 1;
                } else {
                    bg_rects.push(BgRect {
                        col,
                        row,
                        width: 1,
                        color: bg,
                    });
                }
            } else {
                bg_rects.push(BgRect {
                    col,
                    row,
                    width: 1,
                    color: bg,
                });
            }
        }

        // Skip cells without text
        if !raw.has_text() || raw.codepoint() == 0 || raw.codepoint() == b' ' as u32 {
            if let Some(run) = current_run.take() {
                text_runs.push(run);
                current_key = None;
            }
            continue;
        }

        // Build text content into a stack buffer — no allocation for the common path.
        // 32 bytes covers all realistic terminal grapheme clusters.
        // Pathological sequences are truncated.
        let ch = char::from_u32(raw.codepoint()).unwrap_or('\u{FFFD}');
        let mut buf = [0u8; 32];
        let mut buf_len = ch.encode_utf8(&mut buf).len();

        if raw.has_grapheme() {
           // Silently ignore graphemes for now. The current renderer
           // used the per-cell cell_grapheme() API.
           // The renderer rewrite will use the new row_grapheme API.
        }

        // Safety: every byte was written by char::encode_utf8, which always
        // produces valid UTF-8. No other writer touches buf.
        let cell_str = unsafe { std::str::from_utf8_unchecked(&buf[..buf_len]) };

        let key = RunKey::from_cell(style, fg);

        // Merge into current run when the style key matches and the run is
        // spatially adjacent. We compare the cheap RunKey before touching any
        // Arc-backed Font — no clone on the hot (merge) path.
        if let Some(ref mut run) = current_run {
            if current_key == Some(key) && run.start_col + run.cell_count == col {
                run.text.push_str(cell_str);
                run.style.len += cell_str.len();
                run.cell_count += 1;
                continue;
            }
            text_runs.push(current_run.take().unwrap());
        }

        // Start a new run. Build the TextRun here — the Font clone is
        // unavoidable since TextRun owns its font, but it happens at most once
        // per run (not once per cell), and we clone from the pre-built variant
        // rather than cloning all three Arc fields individually.
        let underline = match key.underline_style {
            0 => None,
            3 => Some(UnderlineStyle {
                thickness: px(1.0),
                color: Some(fg),
                wavy: true,
            }),
            _ => Some(UnderlineStyle {
                thickness: px(1.0),
                color: Some(fg),
                wavy: false,
            }),
        };
        let strikethrough = key.strikethrough.then_some(StrikethroughStyle {
            thickness: px(1.0),
            color: Some(fg),
        });

        let text_run = TextRun {
            len: cell_str.len(),
            font: fonts.select(key.bold, key.italic).clone(),
            color: fg,
            background_color: None,
            underline,
            strikethrough,
        };

        current_key = Some(key);
        current_run = Some(PositionedTextRun {
            start_col: col,
            row,
            text: cell_str.to_owned(),
            cell_count: 1,
            style: text_run,
            font_size,
        });
    }

    // Flush remaining run.
    if let Some(run) = current_run {
        text_runs.push(run);
    }

    RowRuns {
        text_runs,
        bg_rects,
    }
}
