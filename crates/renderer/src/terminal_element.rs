use std::cmp::Ordering;
use std::sync::{Arc, Mutex};
use std::{cell::Cell, rc::Rc};

use crate::color::{PaletteCache, color_rgb_to_hsla, palette_hash};
use crate::cursor::{CursorLayout, build_cursor};
use crate::text_runs::{BgRect, PositionedTextRun, build_row_runs};
use ghostty_vt::{DirtyState, Terminal};
use gpui::{
    App, Bounds, DefiniteLength, Element, ElementId, Entity, GlobalElementId, Hitbox,
    HitboxBehavior, Hsla, InspectorElementId, IntoElement, LayoutId, Length, PathBuilder, Pixels,
    Point, SharedString, Size, Style, TextAlign, TextRun, Window, fill, hsla, point, px, size,
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
    /// Retained to keep the hitbox alive for GPUI's hit-test ordering.
    _hitbox: Hitbox,
    grid: GridDimensions,
    background_color: Hsla,
    /// Shared with TerminalElementState — Arc::clone is O(1).
    row_text_runs: Arc<Vec<Vec<PositionedTextRun>>>,
    bg_rects: Vec<BgRect>,
    /// Per-row selection range: (row, start_col, end_col).
    selection_rects: Vec<(u16, u16, u16)>,
    cursor: Option<CursorLayout>,
}

/// State persisted across frames via with_element_state.
struct TerminalElementState {
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
    last_default_fg: Option<Hsla>,
    last_default_bg: Option<Hsla>,
    /// Simple palette hash (invalidate on palette change).
    last_palette_hash: u64,
}

/// The GPUI Element that renders the terminal surface.
pub struct TerminalElement {
    session: Entity<TerminalSession>,
    terminal: Arc<Mutex<Terminal>>,
    element_id: ElementId,
    /// Written during prepaint so `TerminalView` can read the surface bounds for
    /// mouse-coordinate conversion and for publishing scroll info to the scrollbar.
    bounds_out: Rc<Cell<Option<Bounds<Pixels>>>>,
}

impl TerminalElement {
    pub fn new(
        session: Entity<TerminalSession>,
        terminal: Arc<Mutex<Terminal>>,
        element_id: ElementId,
        bounds_out: Rc<Cell<Option<Bounds<Pixels>>>>,
    ) -> Self {
        Self {
            session,
            terminal,
            element_id,
            bounds_out,
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

/// Per-row selection segment for path-based rendering.
#[derive(Debug, Clone, Copy)]
struct SelectionLine {
    start_x: Pixels,
    end_x: Pixels,
    y: Pixels,
}

/// Paint terminal selection using a continuous path with rounded corners.
/// Adapted from Zed's HighlightedRange approach for multi-line selection rendering.
fn paint_selection_path(
    selection_rects: &[(u16, u16, u16)], // (row, start_col, end_col)
    origin: Point<Pixels>,
    metrics: &CellMetrics,
    corner_radius: Pixels,
    color: Hsla,
    window: &mut Window,
) {
    // Convert selection rects to SelectionLine segments.
    // Rows are already in order from the snapshot, so no sorting needed.
    let lines: Vec<SelectionLine> = selection_rects
        .iter()
        .map(|&(row, start_col, end_col)| {
            let start_x = origin.x + start_col as f32 * metrics.cell_width;
            let end_x = origin.x + (end_col + 1) as f32 * metrics.cell_width;
            let y = origin.y + row as f32 * metrics.line_height;
            SelectionLine { start_x, end_x, y }
        })
        .collect();

    if lines.is_empty() {
        return;
    }

    // Group lines into contiguous ranges (handles multi-line selections with gaps).
    // Selections from the snapshot are per-row, so we group contiguous rows.
    let mut groups: Vec<Vec<SelectionLine>> = Vec::new();
    let mut current_group: Vec<SelectionLine> = vec![lines[0]];
    let threshold = f32::from(metrics.line_height) * 1.5;

    for line in lines.iter().skip(1) {
        let prev = current_group.last().unwrap();
        if f32::from(line.y - prev.y) <= threshold {
            current_group.push(*line);
        } else {
            groups.push(current_group);
            current_group = vec![*line];
        }
    }
    groups.push(current_group);

    for group in groups {
        paint_selection_group(&group, metrics.line_height, corner_radius, color, window);
    }
}

/// Paint a contiguous group of selection lines as a single path.
fn paint_selection_group(
    lines: &[SelectionLine],
    line_height: Pixels,
    corner_radius: Pixels,
    color: Hsla,
    window: &mut Window,
) {
    if lines.is_empty() {
        return;
    }

    let first_line = &lines[0];
    let last_line = &lines[lines.len() - 1];

    let first_top_left = point(first_line.start_x, first_line.y);
    let first_top_right = point(first_line.end_x, first_line.y);

    let curve_height = point(Pixels::ZERO, corner_radius);
    let curve_width = |start_x: Pixels, end_x: Pixels| {
        let max = (end_x - start_x) / 2.;
        point(max.min(corner_radius), Pixels::ZERO)
    };

    let top_curve_width = curve_width(first_line.start_x, first_line.end_x);
    let mut builder = PathBuilder::fill();

    builder.move_to(first_top_right - top_curve_width);
    builder.curve_to(first_top_right + curve_height, first_top_right);

    // Build the right edge going down
    let mut iter = lines.iter().peekable();
    while let Some(line) = iter.next() {
        let bottom_right = point(line.end_x, line.y + line_height);

        if let Some(next_line) = iter.peek() {
            let next_top_right = point(next_line.end_x, next_line.y);

            match next_top_right.x.partial_cmp(&bottom_right.x).unwrap() {
                Ordering::Equal => {
                    builder.line_to(bottom_right);
                }
                Ordering::Less => {
                    let cw = curve_width(next_top_right.x, bottom_right.x);
                    builder.line_to(bottom_right - curve_height);
                    builder.curve_to(bottom_right - cw, bottom_right);
                    builder.line_to(next_top_right + cw);
                    builder.curve_to(next_top_right + curve_height, next_top_right);
                }
                Ordering::Greater => {
                    let cw = curve_width(bottom_right.x, next_top_right.x);
                    builder.line_to(bottom_right - curve_height);
                    builder.curve_to(bottom_right + cw, bottom_right);
                    builder.line_to(next_top_right - cw);
                    builder.curve_to(next_top_right + curve_height, next_top_right);
                }
            }
        } else {
            // Last line - curve the bottom-right corner
            let cw = curve_width(line.start_x, line.end_x);
            builder.line_to(bottom_right - curve_height);
            builder.curve_to(bottom_right - cw, bottom_right);

            // Bottom edge
            let bottom_left = point(line.start_x, bottom_right.y);
            builder.line_to(bottom_left + cw);
            builder.curve_to(bottom_left - curve_height, bottom_left);
        }
    }

    // Build the left edge going up
    if first_line.start_x > last_line.start_x {
        // Selection narrows at the bottom - add inner corner
        let cw = curve_width(last_line.start_x, first_line.start_x);
        let second_top_left = point(last_line.start_x, first_line.y + line_height);
        builder.line_to(second_top_left + curve_height);
        builder.curve_to(second_top_left + cw, second_top_left);
        let first_bottom_left = point(first_line.start_x, second_top_left.y);
        builder.line_to(first_bottom_left - cw);
        builder.curve_to(first_bottom_left - curve_height, first_bottom_left);
    }

    // Close the path
    builder.line_to(first_top_left + curve_height);
    builder.curve_to(first_top_left + top_curve_width, first_top_left);
    builder.line_to(first_top_right - top_curve_width);

    if let Ok(path) = builder.build() {
        window.paint_path(path, color);
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
        self.bounds_out.set(Some(bounds));
        let render_config = {
            let entity = self.session.read(cx).render_config().clone();
            entity.read(cx).clone()
        };

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

            // Compute cell dimensions for CSI size reports.
            let cell_w = f32::from(metrics.cell_width) as u16;
            let cell_h = f32::from(metrics.line_height) as u16;

            // Snapshot render data: single lock scope.
            let snapshot = terminal::RenderSnapshot::capture(
                &mut terminal.lock().expect("terminal mutex poisoned"),
            );

            // Notify the PTY to resize.
            // IO thread will resize terminal after ConPTY reflows.
            session.update(cx, |s, _cx| {
                s.request_resize(grid.cols, grid.rows, cell_w, cell_h);
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
                cursor_color,
                default_bg,
                &snapshot,
                &base_font,
                window,
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
                _hitbox: hitbox,
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
        let selection_color = hsla(0.58, 0.70, 0.17, 0.25);
        let corner_radius = px(0.15 * f32::from(metrics.line_height));

        if !layout.selection_rects.is_empty() {
            paint_selection_path(
                &layout.selection_rects,
                origin,
                metrics,
                corner_radius,
                selection_color,
                window,
            );
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
        if let Some(mut cursor) = layout.cursor.take() {
            cursor.paint(origin, window, cx);
        }
    }
}

impl IntoElement for TerminalElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}
