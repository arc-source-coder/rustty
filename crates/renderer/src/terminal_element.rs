use std::sync::{Arc, Mutex};

use crate::color::{PaletteCache, color_rgb_to_hsla, palette_hash};
use crate::cursor::{CursorLayout, build_cursor};
use crate::text_runs::{BgRect, PositionedTextRun, build_row_runs};
use ghostty_vt::{DirtyState, Terminal};
use gpui::{
    App, Bounds, DefiniteLength, Element, ElementId, Entity, GlobalElementId, Hitbox,
    HitboxBehavior, InspectorElementId, IntoElement, LayoutId, Length, Pixels, SharedString, Size,
    Style, TextAlign, TextRun, Window, fill, point, px, size,
};
use terminal::TerminalSession;

/// Computed cell dimensions and grid metrics for layout.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellMetrics {
    pub cell_width: Pixels,
    pub line_height: Pixels,
    pub font_size: Pixels,
}

/// Grid dimensions derived from bounds + cell metrics.
#[derive(Debug, Clone, Copy)]
pub struct GridDimensions {
    pub cols: u16,
    pub rows: u16,
    pub metrics: CellMetrics,
}

/// Cached result of `measure_cell`, keyed by font identity.
/// Stored as GPUI element state so it survives across frames.
struct CellMetricsCache {
    font_family: Arc<str>,
    font_size: Pixels,
    metrics: CellMetrics,
}

/// The layout state produced by prepaint, consumed by paint.
pub struct LayoutState {
    pub hitbox: Hitbox,
    pub grid: GridDimensions,
    pub background_color: gpui::Hsla,
    /// Shared with TerminalElementState — Arc::clone is O(1).
    pub row_text_runs: Arc<Vec<Vec<PositionedTextRun>>>,
    pub bg_rects: Vec<BgRect>,
    /// Per-row selection range: (row, start_col, end_col).
    pub selection_rects: Vec<(u16, u16, u16)>,
    pub cursor: Option<CursorLayout>,
}

/// State persisted across frames via with_element_state.
pub struct TerminalElementState {
    /// Per-row text runs from the last frame. Arc lets LayoutState share
    /// ownership without cloning on clean frames (Arc::clone is O(1)).
    /// Dirty frames use Arc::make_mut to get exclusive mutable access.
    row_text_runs: Arc<Vec<Vec<PositionedTextRun>>>,
    /// Grid dimensions at last render (invalidate on change).
    last_cols: u16,
    last_rows: u16,
    /// Cached font metrics (recomputed only when font config changes).
    cached_metrics: Option<CellMetricsCache>,
    /// Default fg/bg colors at last render (invalidate on change).
    last_default_fg: Option<gpui::Hsla>,
    last_default_bg: Option<gpui::Hsla>,
    /// Simple palette hash (invalidate on palette change).
    last_palette_hash: u64,
}

/// The GPUI Element that renders the terminal surface.
pub struct TerminalElement {
    session: Entity<TerminalSession>,
    terminal: Arc<Mutex<Terminal>>,
    element_id: ElementId,
}

impl TerminalElement {
    pub fn new(
        session: Entity<TerminalSession>,
        terminal: Arc<Mutex<Terminal>>,
        element_id: ElementId,
    ) -> Self {
        Self {
            session,
            terminal,
            element_id,
        }
    }

    /// Measure cell dimensions using the GPUI text system.
    ///
    /// Uses advance('m') for cell width (standard monospace em-width).
    /// Shapes "M" to get ascent + descent for line height.
    ///
    /// Ref: opensrc/repos/MitchForest/rust-terminal/renderer/src/paint.rs
    ///      measure_terminal_cell()
    pub fn measure_cell(font_family: Arc<str>, font_size: Pixels, window: &Window) -> CellMetrics {
        let font = gpui::font(SharedString::new(font_family));
        let text_system = window.text_system();
        let font_id = text_system.resolve_font(&font);
        let cell_width = text_system
            .advance(font_id, font_size, 'm')
            .expect("failed to measure 'm' advance")
            .width;

        let shaped = text_system.shape_line(
            "M".into(),
            font_size,
            &[TextRun {
                len: 1,
                font,
                color: gpui::black(),
                ..Default::default()
            }],
            None,
        );
        let line_height = shaped.ascent + shaped.descent;

        CellMetrics {
            cell_width,
            line_height,
            font_size,
        }
    }

    /// Compute grid dimensions from available bounds and cell metrics.
    fn compute_grid(bounds_size: Size<Pixels>, metrics: CellMetrics) -> GridDimensions {
        let cols = (f32::from(bounds_size.width) / f32::from(metrics.cell_width))
            .floor()
            .max(1.0) as u16;
        let rows = (f32::from(bounds_size.height) / f32::from(metrics.line_height))
            .floor()
            .max(1.0) as u16;
        GridDimensions {
            cols,
            rows,
            metrics,
        }
    }
}

impl Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = LayoutState;

    fn id(&self) -> Option<ElementId> {
        Some(self.element_id.clone())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = Length::Definite(DefiniteLength::Fraction(1.0));
        style.size.height = Length::Definite(DefiniteLength::Fraction(1.0));
        let layout_id = window.request_layout(style, None, cx);
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        let render_config = self.session.read(cx).render_config().clone();

        // Clone handles for the closure (avoids borrowing &self inside closure).
        let session = self.session.clone();
        let terminal = self.terminal.clone();

        let build_layout = |prev_state: Option<TerminalElementState>, window: &mut Window| {
            // Font Family and Cell Handling
            let font_size = px(render_config.font_size);

            let metrics = if let Some(ref prev) = prev_state {
                match prev.cached_metrics {
                    Some(ref cached)
                        if Arc::ptr_eq(&cached.font_family, &render_config.font_family)
                            && cached.font_size == font_size =>
                    {
                        cached.metrics
                    }
                    _ => Self::measure_cell(render_config.font_family.clone(), font_size, window),
                }
            } else {
                Self::measure_cell(render_config.font_family.clone(), font_size, window)
            };

            let font_family = render_config.font_family;
            let new_metrics = CellMetricsCache {
                font_family: Arc::clone(&font_family),
                font_size,
                metrics,
            };

            let grid = Self::compute_grid(bounds.size, metrics);

            // Build resize request (always passed — set_cell_size is cheap,
            // terminal.resize is a no-op if dimensions haven't changed).
            let cell_w = f32::from(metrics.cell_width) as u16;
            let cell_h = f32::from(metrics.line_height) as u16;
            let resize = terminal::ResizeRequest {
                cols: grid.cols,
                rows: grid.rows,
                cell_width: cell_w,
                cell_height: cell_h,
            };

            // Snapshot render data: single lock scope handles resize + snapshot.
            let snapshot = terminal::RenderSnapshot::capture(
                &mut terminal.lock().expect("terminal mutex poisoned"),
                Some(&resize),
            );

            // Update session bookkeeping + notify PTY (no lock needed).
            session.update(cx, |s, _cx| {
                s.apply_resize(&resize);
            });

            let default_fg = color_rgb_to_hsla(snapshot.colors.foreground);
            let default_bg = color_rgb_to_hsla(snapshot.colors.background);
            let background_color = default_bg;
            let palette = PaletteCache::from_raw(&snapshot.palette);
            let base_font = gpui::font(&font_family);

            let num_rows = snapshot.num_rows;
            let dirty = snapshot.dirty;
            let p_hash = palette_hash(&snapshot.palette);

            let size_changed = prev_state
                .as_ref()
                .is_none_or(|s| s.last_cols != grid.cols || s.last_rows != grid.rows);
            let colors_changed = prev_state.as_ref().is_some_and(|s| {
                s.last_default_fg.is_some_and(|c| c != default_fg)
                    || s.last_default_bg.is_some_and(|c| c != default_bg)
            });
            let palette_changed = prev_state
                .as_ref()
                .is_some_and(|s| s.last_palette_hash != p_hash);
            let force_full =
                size_changed || colors_changed || palette_changed || dirty == DirtyState::Full;

            // row_text_runs: Arc-wrapped so clean frames share ownership
            // without copying. Arc::make_mut gives exclusive access on
            // dirty frames (clones only if there are other Arc holders,
            // which there aren't — the previous LayoutState is long gone).
            let mut row_text_runs: Arc<Vec<Vec<PositionedTextRun>>>;
            let mut bg_rects = Vec::new();

            if !force_full && dirty == DirtyState::Clean {
                // Nothing changed — share the cached Arc; no allocation.
                row_text_runs = match prev_state {
                    Some(state) => state.row_text_runs,
                    None => Arc::new(Vec::new()),
                };
                // bg_rects are cheap to rebuild and never worth caching
                // across frames — always produce them fresh.
                for (y, row_snapshot) in snapshot.rows.iter().enumerate() {
                    let runs = build_row_runs(
                        row_snapshot,
                        y as u16,
                        &palette,
                        default_fg,
                        default_bg,
                        &base_font,
                        metrics.font_size,
                    );
                    bg_rects.extend(runs.bg_rects);
                }
            } else {
                // Start from the cached row vec (or a fresh one) and patch
                // only the dirty rows. Arc::make_mut is a no-op clone here
                // because the previous LayoutState no longer holds this Arc.
                row_text_runs = match prev_state {
                    Some(state) if !force_full => state.row_text_runs,
                    _ => {
                        let mut v = Vec::with_capacity(num_rows as usize);
                        v.resize_with(num_rows as usize, Vec::new);
                        Arc::new(v)
                    }
                };

                let rows_mut = Arc::make_mut(&mut row_text_runs);

                for (y, row_snapshot) in snapshot.rows.iter().enumerate() {
                    let runs = build_row_runs(
                        row_snapshot,
                        y as u16,
                        &palette,
                        default_fg,
                        default_bg,
                        &base_font,
                        metrics.font_size,
                    );
                    if (force_full || row_snapshot.dirty) && y < rows_mut.len() {
                        rows_mut[y] = runs.text_runs;
                    }
                    bg_rects.extend(runs.bg_rects);
                }
            }

            // Selection.
            let mut selection_rects = Vec::new();
            for (y, row_snapshot) in snapshot.rows.iter().enumerate() {
                if let Some((start_x, end_x)) = row_snapshot.selection {
                    selection_rects.push((y as u16, start_x, end_x));
                }
            }

            // Cursor.
            let cursor_color = if snapshot.colors.has_cursor_color != 0 {
                color_rgb_to_hsla(snapshot.colors.cursor_color)
            } else {
                default_fg
            };
            let cursor = build_cursor(
                &snapshot.cursor,
                &metrics,
                bounds.origin,
                cursor_color,
                default_bg,
                &snapshot,
                &base_font,
            );

            // Arc::clone is O(1) — state and layout share the same allocation.
            // TerminalElementState carries the Arc into the next frame;
            // LayoutState holds it for the duration of this frame's paint.
            let new_state = TerminalElementState {
                row_text_runs: Arc::clone(&row_text_runs),
                last_cols: grid.cols,
                last_rows: grid.rows,
                cached_metrics: Some(new_metrics),
                last_default_fg: Some(default_fg),
                last_default_bg: Some(default_bg),
                last_palette_hash: p_hash,
            };

            let layout = LayoutState {
                hitbox,
                grid,
                background_color,
                row_text_runs,
                bg_rects,
                selection_rects,
                cursor,
            };
            (layout, new_state)
        };

        if let Some(id) = id {
            window.with_element_state(id, |prev_state, window| build_layout(prev_state, window))
        } else {
            build_layout(None, window).0
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        layout: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let origin = bounds.origin;
        let metrics = &layout.grid.metrics;

        // Layer 1: Default background fill.
        window.paint_quad(fill(bounds, layout.background_color));

        // Layer 2: Non-default background spans.
        for rect in &layout.bg_rects {
            let pos = point(
                origin.x + rect.col as f32 * metrics.cell_width,
                origin.y + rect.row as f32 * metrics.line_height,
            );
            let sz = size(metrics.cell_width * rect.width as f32, metrics.line_height);
            window.paint_quad(fill(Bounds::new(pos, sz), rect.color));
        }

        // Layer 3: Selection overlay (translucent).
        let selection_color = gpui::Hsla {
            h: 0.6,
            s: 0.7,
            l: 0.5,
            a: 0.3,
        };
        for &(row, start_col, end_col) in &layout.selection_rects {
            let width = (end_col - start_col + 1) as f32;
            let pos = point(
                origin.x + start_col as f32 * metrics.cell_width,
                origin.y + row as f32 * metrics.line_height,
            );
            let sel_size = size(metrics.cell_width * width, metrics.line_height);
            window.paint_quad(fill(Bounds::new(pos, sel_size), selection_color));
        }

        // Layer 4: Text runs.
        for row_runs in layout.row_text_runs.iter() {
            for run in row_runs {
                let pos = point(
                    origin.x + run.start_col as f32 * metrics.cell_width,
                    origin.y + run.row as f32 * metrics.line_height,
                );
                if window
                    .text_system()
                    .shape_line(
                        run.text.clone().into(),
                        run.font_size,
                        std::slice::from_ref(&run.style),
                        Some(metrics.cell_width),
                    )
                    .paint(pos, metrics.line_height, TextAlign::Left, None, window, cx)
                    .is_err()
                {
                    log::error!("Paint failed");
                };
            }
        }

        // Layer 5: Cursor overlay.
        if let Some(ref cursor) = layout.cursor {
            cursor.paint(window, cx, layout.grid.metrics.cell_width);
        }
    }
}

impl IntoElement for TerminalElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}
