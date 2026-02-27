use ghostty_vt::CursorState;
use gpui::{
    App, Bounds, Hsla, Pixels, Point, SharedString, TextAlign, TextRun, Window, fill, point, px,
    size,
};

use crate::terminal_element::CellMetrics;

/// Resolved cursor data ready for painting.
pub struct CursorLayout {
    pub origin: Point<Pixels>,
    pub width: Pixels,
    pub height: Pixels,
    pub color: Hsla,
    pub shape: CursorShape,
    /// For block cursor: the character under the cursor and the
    /// background color (used for inverse text rendering).
    pub block_text: Option<BlockCursorText>,
}

pub struct BlockCursorText {
    pub text: String,
    pub font: gpui::Font,
    pub font_size: Pixels,
    pub text_color: Hsla,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorShape {
    Block,
    Bar,
    Underline,
    HollowBlock,
    Hidden,
}

impl CursorShape {
    /// Map Ghostty's cursor style byte to our CursorShape.
    ///
    /// Ghostty style values (from terminal.zig CursorStyle enum):
    /// 0 = block, 1 = underline, 2 = bar,
    /// 3 = block_blink, 4 = underline_blink, 5 = bar_blink
    /// 6 = default (treated as block)
    pub fn from_ghostty(style: u8, visible: bool) -> Self {
        if !visible {
            return CursorShape::Hidden;
        }
        match style {
            0 | 3 | 6 => CursorShape::Block,
            1 | 4 => CursorShape::Underline,
            2 | 5 => CursorShape::Bar,
            _ => CursorShape::Block,
        }
    }
}

/// Build cursor layout from snapshot cursor state.
///
/// For block cursors, captures the character under the cursor so we can
/// re-paint it in inverse colors (text in bg color on cursor-colored rect).
pub fn build_cursor(
    state: &CursorState,
    metrics: &CellMetrics,
    viewport_origin: Point<Pixels>,
    cursor_color: Hsla,
    bg_color: Hsla,
    snapshot: &terminal::RenderSnapshot,
    base_font: &gpui::Font,
) -> Option<CursorLayout> {
    if state.in_viewport == 0 {
        return None;
    }

    let shape = CursorShape::from_ghostty(state.style, state.visible != 0);
    if shape == CursorShape::Hidden {
        return None;
    }

    let origin = point(
        viewport_origin.x + state.x as f32 * metrics.cell_width,
        viewport_origin.y + state.y as f32 * metrics.line_height,
    );

    let (width, height) = match shape {
        CursorShape::Block | CursorShape::HollowBlock => (metrics.cell_width, metrics.line_height),
        CursorShape::Bar => (px(2.0), metrics.line_height),
        CursorShape::Underline => (metrics.cell_width, px(2.0)),
        CursorShape::Hidden => unreachable!(),
    };

    // For block cursor: capture char under cursor for inverse rendering.
    let block_text = if shape == CursorShape::Block {
        snapshot
            .rows
            .get(state.y as usize)
            .and_then(|row| row.cells.as_ref())
            .and_then(|cells| cells.get(state.x as usize))
            .filter(|cell| cell.codepoint != 0 && cell.codepoint != b' ' as u32)
            .map(|cell| {
                let ch = char::from_u32(cell.codepoint).unwrap_or('\u{FFFD}');
                BlockCursorText {
                    text: ch.to_string(),
                    font: base_font.clone(),
                    font_size: metrics.font_size,
                    text_color: bg_color,
                }
            })
    } else {
        None
    };

    Some(CursorLayout {
        origin,
        width,
        height,
        color: cursor_color,
        shape,
        block_text,
    })
}

impl CursorLayout {
    /// Paint the cursor to the window.
    pub fn paint(&self, window: &mut Window, cx: &mut App, cell_width: Pixels) {
        let bounds = Bounds::new(self.origin, size(self.width, self.height));

        match self.shape {
            CursorShape::Block => {
                // Paint cursor rect.
                window.paint_quad(fill(bounds, self.color));

                // Paint character underneath in inverse (bg) color.
                if let Some(ref bt) = self.block_text {
                    let run = TextRun {
                        len: bt.text.len(),
                        font: bt.font.clone(),
                        color: bt.text_color,
                        ..Default::default()
                    };
                    if window
                        .text_system()
                        .shape_line(
                            SharedString::from(bt.text.clone()),
                            bt.font_size,
                            &[run],
                            Some(cell_width),
                        )
                        .paint(self.origin, self.height, TextAlign::Left, None, window, cx)
                        .is_err()
                    {
                        log::error!("Cursor paint failed")
                    };
                }
            }
            CursorShape::Bar | CursorShape::Underline => {
                window.paint_quad(fill(bounds, self.color));
            }
            CursorShape::HollowBlock => {
                let border = px(1.0);
                // Top
                window.paint_quad(fill(
                    Bounds::new(self.origin, size(self.width, border)),
                    self.color,
                ));
                // Bottom
                window.paint_quad(fill(
                    Bounds::new(
                        point(self.origin.x, self.origin.y + self.height - border),
                        size(self.width, border),
                    ),
                    self.color,
                ));
                // Left
                window.paint_quad(fill(
                    Bounds::new(self.origin, size(border, self.height)),
                    self.color,
                ));
                // Right
                window.paint_quad(fill(
                    Bounds::new(
                        point(self.origin.x + self.width - border, self.origin.y),
                        size(border, self.height),
                    ),
                    self.color,
                ));
            }
            CursorShape::Hidden => {}
        }
    }
}
