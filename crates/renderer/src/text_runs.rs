use gpui::{
    Font, FontStyle, FontWeight, Hsla, Pixels, StrikethroughStyle, TextRun, UnderlineStyle, px,
};

use crate::color::{self, PaletteCache};
use terminal::RowSnapshot;

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

/// Build text runs and background rects for a single row.
///
/// Uses `PaletteCache` for color resolution (no FFI calls).
/// Reads grapheme data from the snapshot's pre-captured `graphemes` vec.
pub fn build_row_runs(
    row_snapshot: &RowSnapshot,
    row: u16,
    palette: &PaletteCache,
    default_fg: Hsla,
    default_bg: Hsla,
    base_font: &Font,
    font_size: Pixels,
) -> RowRuns {
    let cells = match &row_snapshot.cells {
        Some(c) => c,
        None => {
            return RowRuns {
                text_runs: Vec::new(),
                bg_rects: Vec::new(),
            };
        }
    };

    let mut text_runs: Vec<PositionedTextRun> = Vec::new();
    let mut bg_rects: Vec<BgRect> = Vec::new();
    let mut current_run: Option<PositionedTextRun> = None;

    let mut col: u16 = 0;
    for cell in cells {
        // Skip wide-char spacer tails.
        if cell.wide == 2 {
            col += 1;
            continue;
        }

        // --- Resolve colors ---
        let mut fg = color::resolve_fg(cell, palette, default_fg);
        let mut bg = color::resolve_bg(cell, palette, default_bg);

        if color::is_inverse(cell) {
            std::mem::swap(&mut fg, &mut bg);
        }
        if color::is_invisible(cell) {
            fg = bg;
        }
        if color::is_faint(cell) {
            fg.a *= 0.7;
        }

        // --- Background rect (non-default bg) ---
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

        // --- Skip empty cells ---
        if cell.codepoint == 0 || cell.codepoint == b' ' as u32 {
            if let Some(run) = current_run.take() {
                text_runs.push(run);
            }
            col += 1;
            continue;
        }

        // --- Build text content ---
        let ch = char::from_u32(cell.codepoint).unwrap_or('\u{FFFD}');
        let mut cell_text = String::with_capacity(4);
        cell_text.push(ch);

        // Grapheme cluster: append extra codepoints from snapshot.
        if cell.grapheme_len > 0
            && let Some(codepoints) = row_snapshot
                .graphemes
                .iter()
                .find(|(c, _)| *c == col)
                .map(|(_, cps)| cps)
        {
            for &cp in codepoints {
                if let Some(c) = char::from_u32(cp) {
                    cell_text.push(c);
                }
            }
        }

        // --- Build TextRun style ---
        let weight = if color::is_bold(cell) {
            FontWeight::BOLD
        } else {
            base_font.weight
        };

        let style = if color::is_italic(cell) {
            FontStyle::Italic
        } else {
            FontStyle::Normal
        };

        let underline = match color::underline_style(cell) {
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

        let strikethrough = if color::is_strikethrough(cell) {
            Some(StrikethroughStyle {
                thickness: px(1.0),
                color: Some(fg),
            })
        } else {
            None
        };

        let text_run = TextRun {
            len: cell_text.len(),
            font: Font {
                weight,
                style,
                family: base_font.family.clone(),
                features: base_font.features.clone(),
                fallbacks: base_font.fallbacks.clone(),
            },
            color: fg,
            background_color: None,
            underline,
            strikethrough,
        };

        // --- Merge into current run or start new ---
        if let Some(ref mut run) = current_run {
            if can_merge(&run.style, &text_run)
                && run.row == row
                && run.start_col + run.cell_count == col
            {
                run.text.push_str(&cell_text);
                run.style.len += cell_text.len();
                run.cell_count += 1;
            } else {
                text_runs.push(current_run.take().unwrap());
                current_run = Some(PositionedTextRun {
                    start_col: col,
                    row,
                    text: cell_text,
                    cell_count: 1,
                    style: text_run,
                    font_size,
                });
            }
        } else {
            current_run = Some(PositionedTextRun {
                start_col: col,
                row,
                text: cell_text,
                cell_count: 1,
                style: text_run,
                font_size,
            });
        }

        col += 1;
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

/// Check if two TextRuns have compatible styles for merging.
fn can_merge(a: &TextRun, b: &TextRun) -> bool {
    a.font == b.font
        && a.color == b.color
        && a.underline == b.underline
        && a.strikethrough == b.strikethrough
}
