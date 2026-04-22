use anyhow::Result;
use font::cache::glyph_cache::{CachedGlyph, GlyphAtlasKind, GlyphKey, GlyphRenderOptions};
use font::cache::shaped_run_cache::ShapedRunCache;
use font::shaper::Shaper;
use font::shaper::run_iter::{RowCells, RunOptions};
use font::shared_grid::GridMetrics;
use font::shared_grid_set::SharedGridPtr;
use font::types::{Cell, FontFeatureSpec, ShapeOptions, Style, TextRun};
use ghostty::{CellStyle, ColorRGB, CursorState, DirtyState, RawCell, RenderFrame};

/// Bright palette offset — Ghostty's `color.Name.bright_black` == 8.
const BRIGHT_PALETTE_OFFSET: usize = 8;

/// Ghostty default (`bold-color = null`) does not force bold→bright remapping.
///
/// TODO(renderer-config): wire Ghostty-compatible `bold-color` support
/// (`null` | `bright` | fixed color) and remove this static toggle.
const ENABLE_BOLD_IS_BRIGHT: bool = false;

use super::shared_grid_ptr::shared_grid_ref;
use super::terminal_renderer::{RendererCellMetrics, RendererTextConfig};
use super::types::{DirtyRect, QuadInstance, RenderBatch};

/// Ghostty: `renderer.GridSize`
#[derive(Clone, Copy, Default, Eq, PartialEq)]
pub(crate) struct GridSize {
    pub rows: u16,
    pub columns: u16,
}

/// Ghostty: `ArrayListCollection(CellText)` — owns per-row Vec allocations.
///
/// Layout: lists[0] = cursor-first, lists[1..=rows] = text rows,
///         lists[rows+1] = cursor-last.
///
/// Ghostty reference:
///   `src/datastruct/array_list_collection.zig`
///   `src/renderer/cell.zig` — `Contents.fg_rows`
pub(crate) struct FgRows {
    pub lists: Vec<Vec<QuadInstance>>,
}

impl FgRows {
    fn new() -> Self {
        Self { lists: Vec::new() }
    }

    /// Resize to `rows + 2` lists (cursor-first + N rows + cursor-last).
    ///
    /// Ghostty: `Contents.resize` → `ArrayListCollection.init(rows + 2, cols * 3)`.
    /// Pre-allocate each row list with capacity `cols * 3` to match Ghostty's
    /// sizing heuristic (glyph + underline + strikethrough per column).
    ///
    /// Note: Ghostty says "appendAssumeCapacity MUST NOT be used since it is
    /// possible to exceed this with combining glyphs" — we use `push()` which
    /// handles reallocation automatically.
    fn resize(&mut self, rows: usize, cols: usize) {
        let count = rows + 2;
        self.lists.clear();
        self.lists.reserve(count);
        // Cursor-first lane (capacity 1, matching Ghostty)
        self.lists.push(Vec::with_capacity(1));
        // Text row lanes
        for _ in 0..rows {
            self.lists.push(Vec::with_capacity(cols * 3));
        }
        // Cursor-last lane (capacity 1, matching Ghostty)
        self.lists.push(Vec::with_capacity(1));
    }

    /// Ghostty: `ArrayListCollection.reset` — clear all lists, retain capacity.
    fn reset(&mut self) {
        for list in &mut self.lists {
            list.clear();
        }
    }
}

/// Ghostty: `cell.zig::Contents`
///
/// Row-owned persistent cell contents for the terminal grid.
/// Dirty rows are cleared and rebuilt in-place. Backends upload
/// directly from per-row lists (Ghostty `syncFromArrayLists` style).
pub(crate) struct Contents {
    pub size: GridSize,
    /// Flat array of background colors: `bg_cells[row * cols + col]`.
    /// Ghostty: `Contents.bg_cells: []CellBg`
    pub bg_cells: Vec<u32>,
    /// Per-row foreground instance lists with cursor lanes.
    /// Ghostty: `Contents.fg_rows: ArrayListCollection(CellText)`
    pub fg_rows: FgRows,
    /// Total foreground instance count across all lanes.
    ///
    /// We maintain this incrementally while mutating row/cursor lanes so we
    /// can avoid re-scanning all lists each frame just to compute draw count.
    /// Ghostty gets this count from its upload/sync path; this field is the
    /// Rust equivalent optimization for the same cost profile.
    pub fg_count: usize,
    pub bg_generation: u64,
}

impl Contents {
    pub(crate) fn new() -> Self {
        Self {
            size: GridSize::default(),
            bg_cells: Vec::new(),
            fg_rows: FgRows::new(),
            fg_count: 0,
            bg_generation: 0,
        }
    }

    /// Ghostty: `Contents.resize`
    pub(crate) fn resize(&mut self, size: GridSize) {
        self.size = size;
        let cell_count = size.rows as usize * size.columns as usize;
        self.bg_cells.resize(cell_count, 0);
        self.fg_rows
            .resize(size.rows as usize, size.columns as usize);
        self.fg_count = 0;
    }

    /// Ghostty: `Contents.reset`
    pub(crate) fn reset(&mut self) {
        // Zero all background cells
        self.bg_cells.fill(0);
        self.fg_rows.reset();
        self.fg_count = 0;
    }

    /// Ghostty: `Contents.clear(y)` — clear row y's bg slice + fg list.
    pub(crate) fn clear(&mut self, y: u16) {
        let cols = self.size.columns as usize;
        let start = y as usize * cols;
        // Bounds check: if row is out of range, no-op (fail-soft)
        if let Some(slice) = self.bg_cells.get_mut(start..start + cols) {
            slice.fill(0);
        }
        // fg_rows index: y + 1 (index 0 is cursor-first)
        if let Some(list) = self.fg_rows.lists.get_mut(y as usize + 1) {
            let removed = list.len();
            debug_assert!(self.fg_count >= removed, "fg_count underflow in clear");
            self.fg_count -= removed;
            list.clear();
        }
    }

    /// Ghostty: `Contents.bgCell(row, col)` — mutable ref to one bg cell.
    #[inline]
    pub(crate) fn bg_cell(&mut self, row: u16, col: u16) -> &mut u32 {
        let idx = row as usize * self.size.columns as usize + col as usize;
        &mut self.bg_cells[idx]
    }

    /// Ghostty: `Contents.add(.text, cell)` — append to row y's fg list.
    #[inline]
    pub(crate) fn add(&mut self, y: u16, instance: QuadInstance) {
        self.fg_rows.lists[y as usize + 1].push(instance);
        self.fg_count += 1;
    }

    /// Ghostty: `Contents.setCursor`
    ///
    /// Block cursors go in cursor-first (drawn before text).
    /// Bar/underline/hollow go in cursor-last (drawn after text).
    pub(crate) fn set_cursor(&mut self, cell: Option<QuadInstance>, block: bool) {
        if self.size.rows == 0 {
            return;
        }
        let rows = self.size.rows as usize;
        // Clear both cursor lanes
        let removed = self.fg_rows.lists[0].len() + self.fg_rows.lists[rows + 1].len();
        debug_assert!(self.fg_count >= removed, "fg_count underflow in set_cursor");
        self.fg_count -= removed;
        self.fg_rows.lists[0].clear();
        self.fg_rows.lists[rows + 1].clear();

        let Some(cell) = cell else { return };
        if block {
            self.fg_rows.lists[0].push(cell);
            self.fg_count += 1;
        } else {
            self.fg_rows.lists[rows + 1].push(cell);
            self.fg_count += 1;
        }
    }

    /// Ghostty parity helper: lane lists in render order.
    ///
    /// Order: cursor-first → row 0..N-1 → cursor-last.
    #[inline]
    pub(crate) fn fg_lists(&self) -> &[Vec<QuadInstance>] {
        &self.fg_rows.lists
    }
}

#[derive(Clone, Copy)]
pub(crate) struct CellMetrics {
    pub cell_width: f32,
    pub line_height: f32,
    pub baseline: f32,
}

pub(crate) fn cell_metrics_from_grid(grid_metrics: GridMetrics, font_size: f32) -> CellMetrics {
    if grid_metrics.cell_width > 0.0 && grid_metrics.cell_height > 0.0 {
        return CellMetrics {
            cell_width: grid_metrics.cell_width,
            line_height: grid_metrics.cell_height,
            baseline: grid_metrics.baseline.clamp(0.0, grid_metrics.cell_height),
        };
    }

    CellMetrics {
        cell_width: font_size * 0.6,
        line_height: font_size * 1.3,
        baseline: font_size,
    }
}

pub(crate) fn build_shape_options(
    config: &RendererTextConfig,
    cell_metrics: &CellMetrics,
    locale: &str,
    feature_spec: &FontFeatureSpec,
) -> ShapeOptions {
    ShapeOptions {
        locale: locale.to_string(),
        font_size: config.font_size.as_f32(),
        cell_width: cell_metrics.cell_width,
        variant: Style::Normal,
        features: feature_spec.clone(),
    }
}

pub(crate) fn ui_metrics(cell_metrics: CellMetrics) -> RendererCellMetrics {
    RendererCellMetrics {
        cell_width: cell_metrics.cell_width,
        line_height: cell_metrics.line_height,
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_batch(
    config: &RendererTextConfig,
    shared_grid: SharedGridPtr,
    shaper: &mut Shaper,
    shaper_cache: &mut ShapedRunCache,
    contents: &mut Contents,
    cell_metrics: &CellMetrics,
    frame: &RenderFrame,
    out: &mut RenderBatch,
) -> Result<()> {
    let rows = frame.rows() as usize;
    let cols = frame.cols();
    let dirty = frame.dirty();
    let colors = frame.colors();
    let palette: &[ColorRGB; 256] = &colors.palette;
    let default_fg = colors.foreground;
    let default_bg = colors.background;
    let cursor = frame.cursor();
    let scale_factor = config.scale_factor.max(1.0);

    out.clear_color = default_bg.to_float4();
    out.dirty_rects.clear();
    out.grid_cols = cols;
    out.grid_rows = rows as u16;
    out.cell_size =
        [cell_metrics.cell_width * scale_factor, cell_metrics.line_height * scale_factor];

    // Ghostty: grid_size_diff → resize
    let new_size = GridSize {
        rows: rows as u16,
        columns: cols,
    };
    let size_changed = contents.size != new_size;
    if size_changed {
        contents.resize(new_size);
    }

    let full_rebuild = size_changed || dirty == DirtyState::Full;

    if full_rebuild {
        // Ghostty: self.cells.reset()
        contents.reset();

        for y in 0..rows {
            rebuild_row(
                y as u16,
                frame,
                palette,
                cursor,
                contents,
                shaper,
                shaper_cache,
                shared_grid,
                config,
                cell_metrics,
                default_fg,
                default_bg,
            )?;
            push_row_dirty_rect(out, y as u16, cols, *cell_metrics, scale_factor);
        }
    } else {
        for y in 0..rows {
            let y_u16 = y as u16;
            let row_dirty = dirty == DirtyState::Partial && frame.row_dirty(y_u16);
            if !row_dirty {
                continue;
            }

            // Ghostty: self.cells.clear(y) then self.rebuildRow(y, ...)
            contents.clear(y_u16);
            rebuild_row(
                y_u16,
                frame,
                palette,
                cursor,
                contents,
                shaper,
                shaper_cache,
                shared_grid,
                config,
                cell_metrics,
                default_fg,
                default_bg,
            )?;
            push_row_dirty_rect(out, y_u16, cols, *cell_metrics, scale_factor);
        }
    }

    // Cursor
    let cursor_color_opt = colors.cursor_color();
    let cursor_quad = cursor_instance(
        frame,
        cursor,
        colors.foreground,
        cursor_color_opt,
        cell_metrics,
        config,
    );
    let is_block = cursor.style == 1;
    contents.set_cursor(cursor_quad, is_block);

    // Record fg count; backend uploads directly from lane slices.
    out.instance_count = contents.fg_count;

    if !out.dirty_rects.is_empty() {
        contents.bg_generation = contents.bg_generation.wrapping_add(1);
    }
    out.bg_generation = contents.bg_generation;

    Ok(())
}

/// Ghostty: `rebuildRow`
///
/// Free function for borrow splitting across renderer fields.
#[allow(clippy::too_many_arguments)]
fn rebuild_row(
    y: u16,
    frame: &RenderFrame,
    palette: &[ColorRGB; 256],
    cursor: CursorState,
    contents: &mut Contents,
    shaper: &mut Shaper,
    shaper_cache: &mut ShapedRunCache,
    shared_grid: SharedGridPtr,
    config: &RendererTextConfig,
    cell_metrics: &CellMetrics,
    default_fg: ColorRGB,
    default_bg: ColorRGB,
) -> Result<()> {
    let Some(raw_cells) = frame.row_raw(y) else {
        return Ok(());
    };
    let cols = raw_cells.len();

    // Styles are needed throughout row processing, so fetch once.
    // Graphemes are row-level SoA and zero-copy, so one fetch per rebuilt row
    // is acceptable and keeps the control flow simple.
    let styles = frame.row_styles(y).unwrap_or(&[]);
    let graphemes = frame.row_graphemes(y).unwrap_or(&[]);

    let cursor_x = (cursor.visible != 0 && cursor.in_viewport != 0 && cursor.y == y)
        .then_some(cursor.x as usize);
    let selection = frame.row_selection(y).map(|(start, end)| [start, end]);

    let row_cells = RowCells {
        raw_cells,
        styles,
        graphemes,
    };
    let run_opts = RunOptions {
        grid: shared_grid_ref(shared_grid),
        cells: row_cells,
        selection,
        cursor_x,
    };

    let scale_factor = config.scale_factor.max(1.0);
    let baseline_y = y as f32 * cell_metrics.line_height + cell_metrics.baseline;

    let mut run_iter = shaper.run_iterator(run_opts);

    // Ghostty pattern: iterate cells, lazily advance run iterator and shape
    // For simplicity, we iterate runs then emit cells (equivalent result)
    let mut shaper_run: Option<TextRun> = run_iter.next();
    let mut shaper_cells: Option<&[Cell]> = None;
    let mut shaper_cells_i: usize = 0;

    for x in 0..cols {
        let raw = raw_cells[x];
        let base = resolve_cell_color_base(raw, styles, x, palette, default_fg);
        let bg_style = base.bg_style;
        let fg_style = base.fg_style;

        // Ghostty: selection color override (generic.zig rebuildRow L2751-2916).
        // For spacer_tail cells, check selection using x-1 (the wide char's head
        // column) so the entire wide character is selected as a unit.
        let selected = if let Some(sel) = selection {
            let x_compare = if raw.wide() == 2 {
                x.saturating_sub(1)
            } else {
                x
            };
            x_compare >= sel[0] as usize && x_compare <= sel[1] as usize
        } else {
            false
        };

        let is_inverse = base.is_inverse;

        // Ghostty: final bg after inversion and selection.
        let bg = if selected {
            // TODO: make configurable via RendererTextConfig once the config
            // system exists (Ghostty: config.selection_background/foreground).
            default_fg
        } else if is_inverse != is_covering(raw.codepoint()) {
            fg_style
        } else {
            bg_style.unwrap_or(default_bg)
        };

        // Ghostty: final fg after inversion and selection.
        let fg = if selected {
            // TODO: make configurable via RendererTextConfig once the config
            // system exists (Ghostty: config.selection_foreground/background).
            default_bg
        } else if is_inverse {
            bg_style.unwrap_or(default_bg)
        } else {
            fg_style
        };

        // Ghostty: invisible makes fg match bg.
        let fg = if base.is_invisible { bg } else { fg };

        // Ghostty: bg_alpha — selected and inverse cells are fully opaque,
        // cells with no explicit bg get alpha=0 (clear color shows through).
        let bg_alpha: u8 = if selected || is_inverse {
            255
        } else if bg_style.is_some() {
            255
        } else {
            0
        };

        // Ghostty: fg alpha — faint text uses reduced opacity.
        // TODO: make faint_opacity configurable (Ghostty: config.faint_opacity).
        let fg_alpha: u8 = if base.is_faint {
            178 // ~0.7 * 255
        } else {
            255
        };

        *contents.bg_cell(y, x as u16) = bg.to_rgba_u32_with_alpha(bg_alpha);

        if base.is_invisible {
            continue;
        }

        let fg_u32 = fg.to_rgba_u32_with_alpha(fg_alpha);

        // --- Lazy run shaping (matching Ghostty rebuildRow) ---
        // Advance run iterator when current run's shaped cells are exhausted
        if shaper_cells.is_some_and(|c| shaper_cells_i >= c.len()) {
            shaper_run = run_iter.next();
            shaper_cells = None;
            shaper_cells_i = 0;
        }

        if let Some(run) = shaper_run {
            // Shape on demand (cache check first)
            if shaper_cells.is_none() {
                shaper_cells = Some(if let Some(cached) = shaper_cache.get(run.hash) {
                    cached
                } else {
                    let shaped = shaper.shape_with_grid(run, shared_grid_ref(shared_grid));
                    let Ok(shaped) = shaped else {
                        shaper_cache.put(run.hash, &[]);
                        shaper_cells = Some(&[]);
                        continue;
                    };
                    shaper_cache.put(run.hash, shaped.cells);
                    shaped.cells
                });
            }

            if let Some(cells) = shaper_cells {
                // Emit all shaped cells that match column x
                while shaper_cells_i < cells.len()
                    && (usize::from(run.offset) + usize::from(cells[shaper_cells_i].x)) == x
                {
                    add_glyph(
                        y,
                        raw_cells,
                        styles,
                        palette,
                        default_fg,
                        default_bg,
                        run,
                        &cells[shaper_cells_i],
                        fg_u32,
                        contents,
                        shared_grid,
                        cell_metrics,
                        baseline_y,
                        scale_factor,
                    )?;
                    shaper_cells_i += 1;
                }
            }
        }
    }

    Ok(())
}

/// Ghostty: `addGlyph`
///
/// For multi-cell ligature glyphs that span columns with different foreground
/// colors, the emitted quad is split horizontally at cell boundaries so each
/// segment carries the correct per-cell color (WT-style overlap splitting).
#[allow(clippy::too_many_arguments)]
fn add_glyph(
    y: u16,
    raw_cells: &[RawCell],
    styles: &[CellStyle],
    palette: &[ColorRGB; 256],
    default_fg: ColorRGB,
    default_bg: ColorRGB,
    run: TextRun,
    cell: &Cell,
    fg: u32,
    contents: &mut Contents,
    shared_grid: SharedGridPtr,
    cell_metrics: &CellMetrics,
    baseline_y: f32,
    scale_factor: f32,
) -> Result<()> {
    let col = usize::from(run.offset) + usize::from(cell.x);
    if col >= raw_cells.len() {
        return Ok(());
    }
    let raw = raw_cells[col];
    if !raw.has_text() || raw.codepoint() == 0 {
        return Ok(());
    }

    let mut options = GlyphRenderOptions::default();
    options = options.with_cell_width(if raw.wide() == 1 { 2 } else { 1 });
    let key = GlyphKey::new(run.font_index, cell.glyph_index, options);
    let cached = resolve_glyph_cached(shared_grid, key)?;
    if cached.width == 0 || cached.height == 0 {
        return Ok(());
    }

    let pen_x = (f32::from(run.offset + cell.x) * cell_metrics.cell_width
        + f32::from(cell.x_offset))
        * scale_factor;
    let pen_y = (baseline_y + f32::from(cell.y_offset)) * scale_factor;
    let origin = [pen_x + cached.offset_x as f32, pen_y + cached.offset_y as f32];
    let size = [cached.width as f32, cached.height as f32];

    if cached.atlas_kind == Some(GlyphAtlasKind::Color) {
        let mut instance = QuadInstance::color_glyph_rect(origin, size);
        instance.set_texcoord(
            cached.atlas_x.min(u16::MAX as u32) as u16,
            cached.atlas_y.min(u16::MAX as u32) as u16,
        );
        contents.add(y, instance);
        return Ok(());
    }

    let cell_width_px = (cell_metrics.cell_width * scale_factor).round();

    // Fast path: most glyphs stay as a single quad. WT caches the overlap-split
    // decision with the glyph entry, so we only pay the per-row color walk for
    // ligature-like overhangs that actually need it.
    if !cached.overlap_split {
        let mut instance = QuadInstance::glyph_rect(origin, size, fg);
        instance.set_texcoord(
            cached.atlas_x.min(u16::MAX as u32) as u16,
            cached.atlas_y.min(u16::MAX as u32) as u16,
        );
        contents.add(y, instance);
        return Ok(());
    }

    // Ligature-like overhang: split at cell edges so per-cell foreground color
    // changes still apply across the shared glyph bitmap.
    overlap_split_glyph(
        y,
        raw_cells,
        styles,
        palette,
        default_fg,
        default_bg,
        origin,
        size,
        &cached,
        cell_width_px,
        contents,
    );
    Ok(())
}

/// WT-style overlap splitting for multi-cell ligature glyphs.
///
/// Splits a single glyph quad into per-cell-color segments wherever the
/// foreground color changes across the covered columns.
///
/// WT reference: `BackendD3D::_drawTextOverlapSplit` in
/// `opensrc/repos/microsoft/terminal/src/renderer/atlas/BackendD3D.cpp`.
#[cold]
#[allow(clippy::too_many_arguments)]
fn overlap_split_glyph(
    y: u16,
    raw_cells: &[RawCell],
    styles: &[CellStyle],
    palette: &[ColorRGB; 256],
    default_fg: ColorRGB,
    default_bg: ColorRGB,
    origin: [f32; 2],
    size: [f32; 2],
    cached: &CachedGlyph,
    cell_width_px: f32,
    contents: &mut Contents,
) {
    let tex_x = cached.atlas_x.min(u16::MAX as u32) as u16;
    let tex_y = cached.atlas_y.min(u16::MAX as u32) as u16;
    let glyph_left = origin[0];
    let glyph_right = origin[0] + size[0];
    let Some(bounds) =
        overlap_split_bounds(glyph_left, glyph_right, cell_width_px, raw_cells.len())
    else {
        return;
    };

    let mut span_left = bounds.left_px;
    let mut span_fg = resolve_cell_fg_u32(
        raw_cells[bounds.first_col],
        styles,
        bounds.first_col,
        palette,
        default_fg,
        default_bg,
    );
    let mut column = bounds.first_col + 1;
    let mut clip_left = ((bounds.first_col + 1) as f32) * cell_width_px;

    while clip_left < bounds.right_px && column < raw_cells.len() {
        let neighbor_fg = resolve_cell_fg_u32(
            raw_cells[column],
            styles,
            column,
            palette,
            default_fg,
            default_bg,
        );

        if neighbor_fg != span_fg {
            // WT splits at logical cell edges, not at glyph-local bitmap offsets.
            let seg_width = clip_left - span_left;
            let tex_offset = (span_left - glyph_left).round() as u16;
            let mut inst =
                QuadInstance::glyph_rect([span_left, origin[1]], [seg_width, size[1]], span_fg);
            inst.set_texcoord(tex_x.saturating_add(tex_offset), tex_y);
            contents.add(y, inst);

            span_left = clip_left;
            span_fg = neighbor_fg;
        }

        column += 1;
        clip_left += cell_width_px;
    }

    // Emit the final (or only) segment.
    let seg_width = bounds.right_px - span_left;
    let tex_offset = (span_left - glyph_left).round() as u16;
    let mut inst = QuadInstance::glyph_rect([span_left, origin[1]], [seg_width, size[1]], span_fg);
    inst.set_texcoord(tex_x.saturating_add(tex_offset), tex_y);
    contents.add(y, inst);
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct OverlapSplitBounds {
    first_col: usize,
    left_px: f32,
    right_px: f32,
}

#[inline]
fn overlap_split_bounds(
    glyph_left: f32,
    glyph_right: f32,
    cell_width_px: f32,
    col_count: usize,
) -> Option<OverlapSplitBounds> {
    if cell_width_px <= 0.0 {
        return None;
    }

    let left_px = glyph_left.max(0.0);
    let right_px = glyph_right.min(col_count as f32 * cell_width_px);
    if left_px >= right_px {
        return None;
    }

    Some(OverlapSplitBounds {
        first_col: (left_px / cell_width_px).floor() as usize,
        left_px,
        right_px,
    })
}

/// Resolve the effective foreground color for a single cell column as GPU-ready u32.
///
/// Mirrors the fg resolution in `rebuild_row` but without selection/cursor
/// overrides (those split runs at boundaries and don't affect ligature spans).
#[inline]
fn resolve_cell_fg_u32(
    raw: RawCell,
    styles: &[CellStyle],
    col: usize,
    palette: &[ColorRGB; 256],
    default_fg: ColorRGB,
    default_bg: ColorRGB,
) -> u32 {
    let base = resolve_cell_color_base(raw, styles, col, palette, default_fg);
    let fg = if base.is_inverse {
        // When inverse, rendered fg is the bg color.
        base.bg_style.unwrap_or(default_bg)
    } else {
        base.fg_style
    };
    // TODO: make faint_opacity configurable (Ghostty: config.faint_opacity).
    let alpha: u8 = if base.is_faint { 178 } else { 255 };
    fg.to_rgba_u32_with_alpha(alpha)
}

#[derive(Clone, Copy)]
struct CellColorBase {
    bg_style: Option<ColorRGB>,
    fg_style: ColorRGB,
    is_inverse: bool,
    is_faint: bool,
    is_invisible: bool,
}

#[inline]
fn resolve_cell_color_base(
    raw: RawCell,
    styles: &[CellStyle],
    col: usize,
    palette: &[ColorRGB; 256],
    default_fg: ColorRGB,
) -> CellColorBase {
    let style = cell_style_at(raw, styles, col);
    CellColorBase {
        bg_style: resolve_bg_style(raw, style, palette),
        fg_style: resolve_fg_style(style, palette, default_fg),
        is_inverse: style.is_some_and(CellStyle::is_inverse),
        is_faint: style.is_some_and(CellStyle::is_faint),
        is_invisible: style.is_some_and(CellStyle::is_invisible),
    }
}

#[inline]
fn cell_style_at(raw: RawCell, styles: &[CellStyle], col: usize) -> Option<&CellStyle> {
    // Ghostty: style lookup is only needed when style_id is non-default.
    if raw.style_id() != 0 {
        styles.get(col)
    } else {
        None
    }
}

fn resolve_glyph_cached(shared_grid: SharedGridPtr, key: GlyphKey) -> Result<CachedGlyph> {
    shared_grid_ref(shared_grid).render_glyph(key)
}

pub(crate) fn cursor_instance(
    frame: &RenderFrame,
    cursor: CursorState,
    default_fg: ColorRGB,
    cursor_color: Option<ColorRGB>,
    cell_metrics: &CellMetrics,
    config: &RendererTextConfig,
) -> Option<QuadInstance> {
    if cursor.visible == 0 || cursor.in_viewport == 0 {
        return None;
    }

    if cursor.y >= frame.rows() || cursor.x >= frame.cols() {
        return None;
    }

    // TODO(renderer-config): mirror Ghostty cursor color/text config
    // (cursor-color + cursor-text) once renderer config plumbing lands.
    let color_u32 = cursor_color.unwrap_or(default_fg).to_rgba_u32();

    let mut x = cursor.x as f32 * cell_metrics.cell_width;
    let y = cursor.y as f32 * cell_metrics.line_height;
    let mut w = cell_metrics.cell_width;
    let mut h = cell_metrics.line_height;
    let scale_factor = config.scale_factor.max(1.0);

    if cursor.wide_tail != 0 {
        x = (cursor.x.saturating_sub(1) as f32) * cell_metrics.cell_width;
        w = cell_metrics.cell_width * 2.0;
    }

    // Ghostty FFI mapping (render.zig): 0=bar, 1=block, 2=underline, 3=block_hollow.
    match cursor.style {
        0 => {
            w = cell_metrics.cell_width.mul_add(0.12, 0.0).max(1.0);
        }
        2 => {
            h = cell_metrics.line_height.mul_add(0.1, 0.0).max(1.0);
            let y_bottom = y + cell_metrics.line_height;
            return Some(QuadInstance::solid_rect(
                [x * scale_factor, (y_bottom - h).max(y) * scale_factor],
                [w * scale_factor, h * scale_factor],
                color_u32,
            ));
        }
        1 | 3 => {}
        _ => return None,
    }

    Some(QuadInstance::solid_rect(
        [x * scale_factor, y * scale_factor],
        [w * scale_factor, h * scale_factor],
        color_u32,
    ))
}

/// Ghostty: `style.fg()` — resolve the foreground color for a cell.
///
/// Handles bold-is-bright: when the style has the bold flag and the color is
/// a standard palette index (0–7), remap to the bright variant (8–15).
///
/// Ghostty behavior is config-driven (`bold-color`). We currently keep
/// bold-is-bright disabled to match Ghostty defaults.
#[inline]
fn resolve_fg_style(
    style: Option<&CellStyle>,
    palette: &[ColorRGB; 256],
    default: ColorRGB,
) -> ColorRGB {
    let Some(style) = style else {
        return default;
    };
    let is_bold = style.is_bold();
    match style.fg.tag {
        // palette
        1 => {
            let idx = style.fg.r as usize;
            if ENABLE_BOLD_IS_BRIGHT && is_bold && idx < BRIGHT_PALETTE_OFFSET {
                palette[idx + BRIGHT_PALETTE_OFFSET]
            } else {
                palette[idx]
            }
        }
        // rgb
        2 => ColorRGB::new(style.fg.r, style.fg.g, style.fg.b),
        // none — use default
        _ => default,
    }
}

/// Ghostty: `style.bg()` — resolve the background color for a cell.
///
/// Returns `None` when no bg is set (neither on the cell nor the style),
/// which lets the caller distinguish "use terminal default with alpha=0"
/// from "explicit bg color with alpha=255".
#[inline]
fn resolve_bg_style(
    raw: RawCell,
    style: Option<&CellStyle>,
    palette: &[ColorRGB; 256],
) -> Option<ColorRGB> {
    // Ghostty: cell content_tag overrides style bg (bg_color_palette / bg_color_rgb).
    match raw.content_tag() {
        2 => return Some(palette[raw.bg_palette_index() as usize]),
        3 => {
            let (r, g, b) = raw.bg_rgb();
            return Some(ColorRGB::new(r, g, b));
        }
        _ => {}
    }
    // Fall through to style bg.
    let style = style?;
    match style.bg.tag {
        1 => Some(palette[style.bg.r as usize]),
        2 => Some(ColorRGB::new(style.bg.r, style.bg.g, style.bg.b)),
        _ => None,
    }
}

#[inline(always)]
fn is_covering(cp: u32) -> bool {
    // Ghostty renderer/cell.zig: isCovering (currently U+2588 FULL BLOCK).
    cp == 0x2588
}

fn push_row_dirty_rect(
    out: &mut RenderBatch,
    row: u16,
    cols: u16,
    metrics: CellMetrics,
    scale_factor: f32,
) {
    let top = (row as f32 * metrics.line_height * scale_factor).floor() as i32;
    let bottom = (((row + 1) as f32) * metrics.line_height * scale_factor).ceil() as i32;
    let right = (cols as f32 * metrics.cell_width * scale_factor).ceil() as i32;
    push_dirty_rect_coalesced(
        &mut out.dirty_rects,
        DirtyRect {
            left: 0,
            top,
            right,
            bottom,
        },
    );
}

fn push_dirty_rect_coalesced(rects: &mut Vec<DirtyRect>, rect: DirtyRect) {
    if let Some(last) = rects.last_mut()
        && last.left == rect.left
        && last.right == rect.right
        && rect.top <= last.bottom
        && rect.bottom >= last.top
    {
        last.top = last.top.min(rect.top);
        last.bottom = last.bottom.max(rect.bottom);
        return;
    }
    rects.push(rect);
}

#[cfg(test)]
mod tests {
    use super::overlap_split_bounds;

    #[test]
    fn overlap_split_bounds_anchor_to_cell_edges() {
        let bounds = overlap_split_bounds(4.0, 22.0, 10.0, 4).expect("visible overlap split");
        assert_eq!(bounds.first_col, 0);
        assert_eq!(bounds.left_px, 4.0);
        assert_eq!(bounds.right_px, 22.0);
    }
}
