use ghostty_vt::CursorState;
use gpui::{
    App, BorderStyle, Bounds, Hsla, Pixels, Point, ShapedLine, SharedString, TextAlign, TextRun,
    Window, fill, outline, point, px, size,
};

use crate::terminal_element::CellMetrics;

/// Resolved cursor data ready for painting.
pub struct CursorLayout {
    origin: Point<Pixels>,
    block_width: Pixels,
    line_height: Pixels,
    color: Hsla,
    shape: CursorShape,
    /// For block cursor: the shaped text to paint in inverse color.
    block_text: Option<ShapedLine>,
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
    /// Ghostty style values (from render.zig ghostty_vt_terminal_render_cursor):
    /// 0 = bar, 1 = block, 2 = underline, 3 = block_hollow
    pub fn from_ghostty(style: u8, visible: bool) -> Self {
        if !visible {
            return CursorShape::Hidden;
        }
        match style {
            0 => CursorShape::Bar,
            1 => CursorShape::Block,
            2 => CursorShape::Underline,
            3 => CursorShape::HollowBlock,
            _ => CursorShape::Block,
        }
    }

    /// Returns the bounds for this cursor shape, matching Zed's editor behavior.
    fn bounds(
        &self,
        origin: Point<Pixels>,
        block_width: Pixels,
        line_height: Pixels,
    ) -> Bounds<Pixels> {
        match self {
            CursorShape::Bar => Bounds {
                origin,
                size: size(px(2.0), line_height),
            },
            CursorShape::Block | CursorShape::HollowBlock => Bounds {
                origin,
                size: size(block_width, line_height),
            },
            CursorShape::Underline => Bounds {
                origin: origin + point(px(0.0), line_height - px(2.0)),
                size: size(block_width, px(2.0)),
            },
            CursorShape::Hidden => Bounds::default(),
        }
    }
}

/// Build cursor layout from snapshot cursor state.
///
/// Note: Ghostty provides cursor x,y as viewport-relative coordinates (0-indexed).
/// The viewport origin is applied during painting, not here.
pub fn build_cursor(
    state: &CursorState,
    metrics: &CellMetrics,
    cursor_color: Hsla,
    bg_color: Hsla,
    snapshot: &terminal::RenderSnapshot,
    base_font: &gpui::Font,
    window: &mut Window,
) -> Option<CursorLayout> {
    if state.in_viewport == 0 {
        return None;
    }

    let shape = CursorShape::from_ghostty(state.style, state.visible != 0);
    if shape == CursorShape::Hidden {
        return None;
    }

    // Store relative position within the viewport (matches Zed's approach)
    // The viewport origin is added during paint()
    let origin = point(
        state.x as f32 * metrics.cell_width,
        state.y as f32 * metrics.line_height,
    );

    // Get the cell under the cursor for block text rendering
    let cell = snapshot
        .rows
        .get(state.y as usize)
        .and_then(|row| row.cells.as_ref())
        .and_then(|cells| cells.get(state.x as usize));

    // Extract the character under the cursor for width calculation
    let cursor_char = cell
        .filter(|c| c.codepoint != 0)
        .and_then(|c| char::from_u32(c.codepoint));

    // For block cursor, shape the text so we can paint it in inverse color
    let block_text = if shape == CursorShape::Block {
        cell.filter(|c| c.codepoint != 0 && c.codepoint != b' ' as u32)
            .and_then(|c| {
                let ch = char::from_u32(c.codepoint).unwrap_or('\u{FFFD}');
                let text = ch.to_string();
                let run = TextRun {
                    len: text.len(),
                    font: base_font.clone(),
                    color: bg_color, // Inverse: background color for text
                    ..Default::default()
                };

                // Shape the text to get proper metrics
                let shaped = window.text_system().shape_line(
                    SharedString::from(text),
                    metrics.font_size,
                    &[run],
                    Some(metrics.cell_width),
                );

                Some(shaped)
            })
    } else {
        None
    };

    // Determine block width: use cell width for whitespace to avoid cursor stretching,
    // otherwise use max of cell width and shaped text width for wide characters
    let block_width = if cursor_char.map_or(false, |c| c.is_whitespace()) {
        metrics.cell_width
    } else if let Some(ref text) = block_text {
        text.width.max(metrics.cell_width)
    } else {
        metrics.cell_width
    };

    Some(CursorLayout {
        origin,
        block_width,
        line_height: metrics.line_height,
        color: cursor_color,
        shape,
        block_text,
    })
}

impl CursorLayout {
    /// Paint the cursor to the window.
    pub fn paint(&mut self, viewport_origin: Point<Pixels>, window: &mut Window, cx: &mut App) {
        // Combine stored relative origin with viewport origin
        let absolute_origin = self.origin + viewport_origin;
        let bounds = self
            .shape
            .bounds(absolute_origin, self.block_width, self.line_height);

        // Draw background or border quad based on shape
        let cursor_quad = if matches!(self.shape, CursorShape::HollowBlock) {
            outline(bounds, self.color, BorderStyle::Solid)
        } else {
            fill(bounds, self.color)
        };

        window.paint_quad(cursor_quad);

        // For block cursor, paint the text on top in inverse color
        if let Some(ref block_text) = self.block_text {
            let text_origin = absolute_origin;
            if block_text
                .paint(
                    text_origin,
                    self.line_height,
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                )
                .is_err()
            {
                log::warn!("Failed to paint block cursor");
            };
        }
    }
}
