use ghostty_vt::{
    ColorRGB, ColorState, CursorState, DirtyState, FlatCell, ScrollbarInfo, Terminal,
};

/// Owned snapshot of all render data, built inside one Mutex lock scope.
/// The renderer uses this for text shaping and painting without holding
/// the terminal lock.
#[derive(Clone)]
pub struct RenderSnapshot {
    pub dirty: DirtyState,
    pub colors: ColorState,
    pub cursor: CursorState,
    pub num_rows: u16,
    pub num_cols: u16,
    pub palette: [ColorRGB; 256],
    pub rows: Vec<RowSnapshot>,
    pub scrollbar: ScrollbarInfo,
    pub is_alternate_screen: bool,
}

/// Per-row snapshot data.
#[derive(Clone)]
pub struct RowSnapshot {
    pub cells: Option<Vec<FlatCell>>,
    pub dirty: bool,
    pub selection: Option<(u16, u16)>,
    /// Grapheme codepoints for cells with grapheme_len > 0.
    /// Vec of (col_index, codepoints).
    pub graphemes: Vec<(u16, Vec<u32>)>,
}

impl RenderSnapshot {
    /// Build a snapshot from a locked Terminal.
    ///
    /// Calls `render_update()` → `begin_frame()` →
    /// copies all data → drops frame (clears dirty) → returns owned snapshot.
    /// The caller must hold the Mutex lock.
    pub fn capture(terminal: &mut Terminal) -> Self {
        terminal.render_update();
        let frame = terminal.begin_frame();

        let scrollbar = terminal.scrollbar_info();
        let is_alternate_screen = terminal.is_alternate_screen();

        let dirty = frame.dirty();
        let colors = frame.colors();
        let cursor = frame.cursor();
        let num_rows = frame.rows();
        let num_cols = frame.cols();

        let mut palette = [ColorRGB::default(); 256];
        for i in 0..=255u8 {
            palette[i as usize] = frame.palette_color(i);
        }

        let mut rows = Vec::with_capacity(num_rows as usize);
        for y in 0..num_rows {
            let cells = frame.row_cells(y);
            let row_dirty = frame.row_dirty(y);
            let selection = frame.row_selection(y);

            // Capture grapheme clusters for multi-codepoint cells.
            let mut graphemes = Vec::new();
            if let Some(ref cells_data) = cells {
                for (col, cell) in cells_data.iter().enumerate() {
                    if cell.grapheme_len > 0
                        && let Some(cps) = frame.cell_grapheme(y, col as u16)
                    {
                        graphemes.push((col as u16, cps));
                    }
                }
            }

            rows.push(RowSnapshot {
                cells,
                dirty: row_dirty,
                selection,
                graphemes,
            });
        }

        // frame drops here → clears dirty flags

        RenderSnapshot {
            dirty,
            colors,
            cursor,
            num_rows,
            num_cols,
            palette,
            rows,
            scrollbar,
            is_alternate_screen,
        }
    }
}
