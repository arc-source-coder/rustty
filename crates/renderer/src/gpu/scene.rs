use std::mem::ManuallyDrop;

use anyhow::{Result, anyhow};
use font::atlas::Region;
use font::backend::dwrite::metrics::extract_metrics;
use font::cache::glyph_cache::{CachedGlyph, GlyphAtlasKind, GlyphKey, GlyphRenderOptions};
use font::cache::shaped_run_cache::ShapedRunCache;
use font::shaper::Shaper;
use font::shaper::run_iter::{RowCells, RunOptions};
use font::shared_grid::{GridMetrics, SharedGrid};
use font::shared_grid_set::SharedGridPtr;
use font::types::{Cell, FontFeatureSpec, FontIndex, ShapeOptions, Style, TextRun};
use ghostty_vt::{CellStyle, ColorRGB, CursorState, DirtyState, RawCell, RenderFrame};
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_COLOR_F, DWRITE_COLOR_GLYPH_RUN1, DWRITE_FONT_METRICS, DWRITE_GLYPH_IMAGE_DATA,
    DWRITE_GLYPH_IMAGE_FORMATS, DWRITE_GLYPH_IMAGE_FORMATS_CFF, DWRITE_GLYPH_IMAGE_FORMATS_COLR,
    DWRITE_GLYPH_IMAGE_FORMATS_JPEG, DWRITE_GLYPH_IMAGE_FORMATS_PNG,
    DWRITE_GLYPH_IMAGE_FORMATS_PREMULTIPLIED_B8G8R8A8, DWRITE_GLYPH_IMAGE_FORMATS_SVG,
    DWRITE_GLYPH_IMAGE_FORMATS_TIFF, DWRITE_GLYPH_IMAGE_FORMATS_TRUETYPE, DWRITE_GLYPH_METRICS,
    DWRITE_GLYPH_OFFSET, DWRITE_GLYPH_RUN, DWRITE_GRID_FIT_MODE_DEFAULT,
    DWRITE_MEASURING_MODE_NATURAL, DWRITE_OUTLINE_THRESHOLD_ANTIALIASED,
    DWRITE_RENDERING_MODE_NATURAL_SYMMETRIC, DWRITE_RENDERING_MODE_OUTLINE,
    DWRITE_TEXT_ANTIALIAS_MODE_GRAYSCALE, DWRITE_TEXTURE_ALIASED_1x1, IDWriteFactory2,
    IDWriteFactory4, IDWriteFontFace, IDWriteFontFace2, IDWriteFontFace4, IDWriteGlyphRunAnalysis,
};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICImagingFactory,
    WICBitmapDitherTypeNone, WICBitmapPaletteTypeMedianCut, WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::core::Interface;
use windows_numerics::Vector2;

use super::shared_grid_ptr::shared_grid_ref;
use super::terminal_renderer::{RendererCellMetrics, RendererTextConfig};
use super::types::{DirtyRect, QuadInstance, RenderBatch};

const GLYPH_PADDING: u32 = 1;

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

pub(crate) fn measure_renderer_cell_metrics(
    shared_grid: &SharedGrid,
    dwrite_factory: &IDWriteFactory2,
    config: &RendererTextConfig,
) -> CellMetrics {
    measure_cell_metrics(shared_grid, dwrite_factory, config)
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

pub(crate) fn build_batch(
    config: &RendererTextConfig,
    shared_grid: SharedGridPtr,
    shaper: &mut Shaper,
    shaper_cache: &mut ShapedRunCache,
    contents: &mut Contents,
    cell_metrics: &CellMetrics,
    rasterizer: &DWriteGlyphRasterizer,
    frame: &RenderFrame,
    out: &mut RenderBatch,
) -> Result<()> {
    let rows = frame.rows() as usize;
    let cols = frame.cols();
    let dirty = frame.dirty();
    let colors = frame.colors();
    let default_fg = rgb_to_color32(colors.foreground);
    let default_bg = rgb_to_color32(colors.background);
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
                contents,
                shaper,
                shaper_cache,
                shared_grid,
                config,
                cell_metrics,
                rasterizer,
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
                contents,
                shaper,
                shaper_cache,
                shared_grid,
                config,
                cell_metrics,
                rasterizer,
                default_fg,
                default_bg,
            )?;
            push_row_dirty_rect(out, y_u16, cols, *cell_metrics, scale_factor);
        }
    }

    // Cursor
    let cursor = frame.cursor();
    let cursor_quad = cursor_instance(
        frame,
        cursor,
        colors.cursor_color,
        colors.has_cursor_color != 0,
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
fn rebuild_row(
    y: u16,
    frame: &RenderFrame,
    contents: &mut Contents,
    shaper: &mut Shaper,
    shaper_cache: &mut ShapedRunCache,
    shared_grid: SharedGridPtr,
    config: &RendererTextConfig,
    cell_metrics: &CellMetrics,
    rasterizer: &DWriteGlyphRasterizer,
    default_fg: Color32,
    default_bg: Color32,
) -> Result<()> {
    let Some(raw_cells) = frame.row_raw(y) else {
        return Ok(());
    };
    let cols = raw_cells.len();

    // No separate pre-scan loop. Resolve style/grapheme in the main flow.
    //
    // Styles are needed throughout row processing, so fetch once.
    // Graphemes are row-level SoA and zero-copy, so one fetch per rebuilt row
    // is acceptable and keeps the control flow simple.
    let styles = frame.row_styles(y).unwrap_or(&[]);
    let graphemes = frame.row_graphemes(y).unwrap_or(&[]);

    let cursor = frame.cursor();
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

        // Ghostty style access pattern: style is looked up by column when
        // this cell has styling; otherwise no style is applied.
        let style = if raw.style_id() != 0 {
            styles.get(x)
        } else {
            None
        };

        let mut fg = resolve_fg_color(style, frame.palette(), default_fg);
        let mut bg = if raw.is_bg_only() {
            resolve_bg_only(raw, frame.palette(), default_bg)
        } else {
            resolve_bg_color(style, frame.palette(), default_bg)
        };

        if style.is_some_and(CellStyle::is_inverse) {
            std::mem::swap(&mut fg, &mut bg);
        }
        if style.is_some_and(CellStyle::is_invisible) {
            fg = bg;
        }
        if style.is_some_and(CellStyle::is_faint) {
            fg = fg.with_alpha(((fg.a() as f32) * 0.7) as u8);
        }

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
        if selected {
            // Ghostty defaults: selection bg = terminal foreground,
            // selection fg = terminal background.
            // TODO: make configurable via RendererTextConfig once the config
            // system exists (Ghostty: config.selection_background/foreground).
            bg = default_fg;
            fg = default_bg;
            // Ghostty: selected cells are forced fully opaque to ensure
            // visibility even when background transparency is enabled.
            bg = bg.with_alpha(255);
        }

        *contents.bg_cell(y, x as u16) = bg.to_rgba_u32();

        if style.is_some_and(CellStyle::is_invisible) {
            continue;
        }

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
                    let Some(face) = shared_grid_ref(shared_grid).face_for_index(run.font_index)
                    else {
                        shaper_cache.put(run.hash, &[]);
                        shaper_cells = Some(&[]);
                        continue;
                    };
                    let shaped = shaper.shape(run, &face)?;
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
                        frame.palette(),
                        default_fg,
                        run,
                        &cells[shaper_cells_i],
                        fg,
                        contents,
                        shared_grid,
                        config,
                        cell_metrics,
                        rasterizer,
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
/// TODO: Figure out how to cleanup this signature, as well as cleanup /
/// deduplicate `overlap_split_glyph` and `resolve_cell_fg`.
fn add_glyph(
    y: u16,
    raw_cells: &[RawCell],
    styles: &[CellStyle],
    palette: &[ColorRGB; 256],
    default_fg: Color32,
    run: TextRun,
    cell: &Cell,
    fg: Color32,
    contents: &mut Contents,
    shared_grid: SharedGridPtr,
    config: &RendererTextConfig,
    cell_metrics: &CellMetrics,
    rasterizer: &DWriteGlyphRasterizer,
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
    let cached = resolve_glyph_cached(shared_grid, rasterizer, config, key)?;
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
    let glyph_cells = if cell_width_px > 0.0 {
        ((cached.width as f32 / cell_width_px).ceil() as usize).max(1)
    } else {
        1
    };

    // Fast path: single-cell glyph or glyph that doesn't span extra columns.
    if glyph_cells <= 1 {
        let mut instance = QuadInstance::glyph_rect(origin, size, fg.to_rgba_u32());
        instance.set_texcoord(
            cached.atlas_x.min(u16::MAX as u32) as u16,
            cached.atlas_y.min(u16::MAX as u32) as u16,
        );
        contents.add(y, instance);
        return Ok(());
    }

    // Multi-cell glyph: check if any covered column has a different fg color.
    // Only resolve neighboring colors when we actually have a wide glyph.
    overlap_split_glyph(
        y,
        col,
        glyph_cells,
        raw_cells,
        styles,
        palette,
        default_fg,
        fg,
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
fn overlap_split_glyph(
    y: u16,
    col: usize,
    glyph_cells: usize,
    raw_cells: &[RawCell],
    styles: &[CellStyle],
    palette: &[ColorRGB; 256],
    default_fg: Color32,
    first_fg: Color32,
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

    let mut span_left = glyph_left;
    let mut span_fg = first_fg;

    for i in 1..glyph_cells {
        let neighbor = col + i;
        if neighbor >= raw_cells.len() {
            break;
        }

        let cell_boundary = glyph_left + (i as f32) * cell_width_px;
        if cell_boundary >= glyph_right {
            break;
        }

        let neighbor_fg =
            resolve_cell_fg(raw_cells[neighbor], styles, neighbor, palette, default_fg);

        if neighbor_fg != span_fg {
            // Emit the segment up to this boundary.
            let seg_width = cell_boundary - span_left;
            let tex_offset = (span_left - glyph_left).round() as u16;
            let mut inst = QuadInstance::glyph_rect(
                [span_left, origin[1]],
                [seg_width, size[1]],
                span_fg.to_rgba_u32(),
            );
            inst.set_texcoord(tex_x.saturating_add(tex_offset), tex_y);
            contents.add(y, inst);

            span_left = cell_boundary;
            span_fg = neighbor_fg;
        }
    }

    // Emit the final (or only) segment.
    let seg_width = glyph_right - span_left;
    let tex_offset = (span_left - glyph_left).round() as u16;
    let mut inst = QuadInstance::glyph_rect(
        [span_left, origin[1]],
        [seg_width, size[1]],
        span_fg.to_rgba_u32(),
    );
    inst.set_texcoord(tex_x.saturating_add(tex_offset), tex_y);
    contents.add(y, inst);
}

/// Resolve the effective foreground color for a single cell column.
///
/// Mirrors the fg resolution in `rebuild_row` but without selection/cursor
/// overrides (those split runs at boundaries and don't affect ligature spans).
#[inline]
fn resolve_cell_fg(
    raw: RawCell,
    styles: &[CellStyle],
    col: usize,
    palette: &[ColorRGB; 256],
    default_fg: Color32,
) -> Color32 {
    let style = if raw.style_id() != 0 {
        styles.get(col)
    } else {
        None
    };
    let mut fg = resolve_fg_color(style, palette, default_fg);
    if style.is_some_and(CellStyle::is_inverse) {
        // When inverse, fg and bg swap — but for overlap splitting we only
        // care about the rendered fg, which after inversion is the bg color.
        // Use default_fg as a reasonable approximation; full bg resolution
        // would require the bg context which isn't worth the complexity here.
        fg = resolve_bg_color(style, palette, default_fg);
    }
    if style.is_some_and(CellStyle::is_faint) {
        fg = fg.with_alpha(((fg.a() as f32) * 0.7) as u8);
    }
    fg
}

fn resolve_glyph_cached(
    shared_grid: SharedGridPtr,
    rasterizer: &DWriteGlyphRasterizer,
    config: &RendererTextConfig,
    key: GlyphKey,
) -> Result<CachedGlyph> {
    shared_grid_ref(shared_grid).get_or_insert_glyph(key, move |k, _metrics| {
        rasterize_into_atlas(
            shared_grid,
            rasterizer,
            config.font_size.as_f32(),
            config.scale_factor.max(1.0),
            *k,
        )
    })
}

pub(crate) fn cursor_instance(
    frame: &RenderFrame,
    cursor: CursorState,
    cursor_color: ColorRGB,
    has_cursor_color: bool,
    cell_metrics: &CellMetrics,
    config: &RendererTextConfig,
) -> Option<QuadInstance> {
    if cursor.visible == 0 || cursor.in_viewport == 0 {
        return None;
    }

    if cursor.y >= frame.rows() || cursor.x >= frame.cols() {
        return None;
    }

    let color = if has_cursor_color {
        rgb_to_color32(cursor_color)
    } else {
        rgb_to_color32(frame.colors().foreground)
    };

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
                color.to_rgba_u32(),
            ));
        }
        1 | 3 => {}
        _ => return None,
    }

    Some(QuadInstance::solid_rect(
        [x * scale_factor, y * scale_factor],
        [w * scale_factor, h * scale_factor],
        color.to_rgba_u32(),
    ))
}

fn rasterize_into_atlas(
    shared_grid: SharedGridPtr,
    rasterizer: &DWriteGlyphRasterizer,
    font_size: f32,
    scale_factor: f32,
    key: GlyphKey,
) -> Result<CachedGlyph> {
    let Some(face2) = shared_grid_ref(shared_grid).face_for_index(key.font_index()) else {
        return Ok(CachedGlyph::default());
    };
    let raster =
        rasterizer.rasterize(&face2, key.glyph_index() as u16, font_size * scale_factor)?;
    if raster.width == 0 || raster.height == 0 {
        return Ok(CachedGlyph::default());
    }

    // Ghostty reference: `SharedGrid.renderGlyph` atlas grow-first path.
    let atlas_kind = raster.atlas_kind;
    let mut did_reset = false;
    loop {
        let reserve = shared_grid_ref(shared_grid).with_atlas_write(atlas_kind, |atlas| {
            atlas.reserve(raster.width + GLYPH_PADDING, raster.height + GLYPH_PADDING)
        });

        match reserve {
            Ok(region) => {
                let glyph_region = Region {
                    x: region.x,
                    y: region.y,
                    width: raster.width,
                    height: raster.height,
                };
                shared_grid_ref(shared_grid).with_atlas_write(atlas_kind, |atlas| {
                    atlas.set(glyph_region, &raster.pixels);
                });
                return Ok(CachedGlyph {
                    width: raster.width,
                    height: raster.height,
                    offset_x: raster.offset_x,
                    offset_y: raster.offset_y,
                    atlas_x: glyph_region.x,
                    atlas_y: glyph_region.y,
                    atlas_kind: Some(atlas_kind),
                });
            }
            Err(_) => {
                let grew = shared_grid_ref(shared_grid).try_grow_atlas(atlas_kind);
                if grew {
                    continue;
                }
                if did_reset {
                    return Err(anyhow!("atlas allocation failed after hard-limit reset"));
                }
                did_reset = true;
                shared_grid_ref(shared_grid).reset_all_atlases();
                shared_grid_ref(shared_grid).clear_glyph_cache();
            }
        }
    }
}

struct RasterizedGlyph {
    atlas_kind: GlyphAtlasKind,
    width: u32,
    height: u32,
    offset_x: i32,
    offset_y: i32,
    pixels: Vec<u8>,
}

#[derive(Clone)]
pub(crate) struct DWriteGlyphRasterizer {
    factory: IDWriteFactory2,
    factory4: Option<IDWriteFactory4>,
    wic_factory: Option<IWICImagingFactory>,
}

impl DWriteGlyphRasterizer {
    pub(crate) fn new(factory: IDWriteFactory2) -> Self {
        let factory4 = factory.cast::<IDWriteFactory4>().ok();
        let wic_factory =
            unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }.ok();
        Self {
            factory,
            factory4,
            wic_factory,
        }
    }

    fn rasterize(
        &self,
        face2: &IDWriteFontFace2,
        glyph_index: u16,
        font_size: f32,
    ) -> Result<RasterizedGlyph> {
        if let Some(color) = self.rasterize_color(face2, glyph_index, font_size)? {
            return Ok(color);
        }
        self.rasterize_grayscale(face2, glyph_index, font_size)
    }

    /// Ghostty reference: `SharedGrid.renderGlyph` routes color presentation to
    /// a dedicated color atlas. On Windows, WT resolves color layers through
    /// `TranslateColorGlyphRun` and we mirror that pipeline here.
    fn rasterize_color(
        &self,
        face2: &IDWriteFontFace2,
        glyph_index: u16,
        font_size: f32,
    ) -> Result<Option<RasterizedGlyph>> {
        if let Some(bitmap) = self.rasterize_bitmap_color(face2, glyph_index, font_size)? {
            return Ok(Some(bitmap));
        }

        // WT reference:
        // `src/renderer/atlas/Backend.cpp::TranslateColorGlyphRun`.
        let Some(factory4) = &self.factory4 else {
            return Ok(None);
        };

        let face = face2.cast::<IDWriteFontFace>()?;
        let glyph_indices = [glyph_index];
        let advances = [0.0f32];
        let offsets = [DWRITE_GLYPH_OFFSET::default()];
        let glyph_run = DWRITE_GLYPH_RUN {
            fontFace: ManuallyDrop::new(Some(face.clone())),
            fontEmSize: font_size,
            glyphCount: 1,
            glyphIndices: glyph_indices.as_ptr(),
            glyphAdvances: advances.as_ptr(),
            glyphOffsets: offsets.as_ptr(),
            isSideways: false.into(),
            bidiLevel: 0,
        };

        let desired_formats = DWRITE_GLYPH_IMAGE_FORMATS_TRUETYPE
            | DWRITE_GLYPH_IMAGE_FORMATS_CFF
            | DWRITE_GLYPH_IMAGE_FORMATS_COLR
            | DWRITE_GLYPH_IMAGE_FORMATS_SVG
            | DWRITE_GLYPH_IMAGE_FORMATS_PNG
            | DWRITE_GLYPH_IMAGE_FORMATS_JPEG
            | DWRITE_GLYPH_IMAGE_FORMATS_TIFF
            | DWRITE_GLYPH_IMAGE_FORMATS_PREMULTIPLIED_B8G8R8A8;

        let enumerator = unsafe {
            factory4.TranslateColorGlyphRun(
                Vector2 { X: 0.0, Y: 0.0 },
                &glyph_run,
                None,
                desired_formats,
                DWRITE_MEASURING_MODE_NATURAL,
                None,
                0,
            )
        };
        let Ok(enumerator) = enumerator else {
            return Ok(None);
        };

        let mut composed = ColorCompose::default();
        let mut found_intrinsic = false;
        while unsafe { enumerator.MoveNext()? }.as_bool() {
            let run_ptr = unsafe { enumerator.GetCurrentRun()? };
            if run_ptr.is_null() {
                continue;
            }
            let run = unsafe { &*run_ptr };
            found_intrinsic = true;
            let format = run.glyphImageFormat;
            if !glyph_image_formats_any(
                format,
                DWRITE_GLYPH_IMAGE_FORMATS_TRUETYPE
                    | DWRITE_GLYPH_IMAGE_FORMATS_CFF
                    | DWRITE_GLYPH_IMAGE_FORMATS_COLR,
            ) {
                // Unsupported CPU-raster formats (SVG/bitmap) are skipped here.
                // WT draws these through D2D (`ColorGlyphRunDraw`); we still keep
                // grayscale fallback if we cannot produce intrinsic color pixels.
                continue;
            }

            if let Some(layer) = self.rasterize_color_outline_layer(run)? {
                composed.blend(&layer, run.Base.runColor);
            }
        }

        if !found_intrinsic || composed.width == 0 || composed.height == 0 {
            return Ok(None);
        }

        Ok(Some(RasterizedGlyph {
            atlas_kind: GlyphAtlasKind::Color,
            width: composed.width,
            height: composed.height,
            offset_x: composed.left,
            offset_y: composed.top,
            pixels: composed.pixels,
        }))
    }

    fn rasterize_bitmap_color(
        &self,
        face2: &IDWriteFontFace2,
        glyph_index: u16,
        font_size: f32,
    ) -> Result<Option<RasterizedGlyph>> {
        let Some(face4) = face2.cast::<IDWriteFontFace4>().ok() else {
            return Ok(None);
        };
        let ppem = font_size.round().max(1.0) as u32;
        let formats = unsafe { face4.GetGlyphImageFormats(glyph_index, ppem, ppem) }?;

        for format in [
            DWRITE_GLYPH_IMAGE_FORMATS_PREMULTIPLIED_B8G8R8A8,
            DWRITE_GLYPH_IMAGE_FORMATS_PNG,
            DWRITE_GLYPH_IMAGE_FORMATS_JPEG,
            DWRITE_GLYPH_IMAGE_FORMATS_TIFF,
        ] {
            if !glyph_image_formats_any(formats, format) {
                continue;
            }
            if let Some(glyph) =
                self.rasterize_bitmap_color_format(&face4, glyph_index, ppem, format)?
            {
                return Ok(Some(glyph));
            }
        }

        Ok(None)
    }

    fn rasterize_bitmap_color_format(
        &self,
        face4: &IDWriteFontFace4,
        glyph_index: u16,
        ppem: u32,
        format: DWRITE_GLYPH_IMAGE_FORMATS,
    ) -> Result<Option<RasterizedGlyph>> {
        let mut data = DWRITE_GLYPH_IMAGE_DATA::default();
        let mut context = std::ptr::null_mut();
        let hr =
            unsafe { face4.GetGlyphImageData(glyph_index, ppem, format, &mut data, &mut context) };
        if hr.is_err() {
            return Ok(None);
        }

        let release = GlyphImageLease {
            face4: face4.clone(),
            context,
        };

        if data.imageData.is_null() || data.imageDataSize == 0 {
            return Ok(None);
        }

        let pixels = unsafe {
            std::slice::from_raw_parts(data.imageData as *const u8, data.imageDataSize as usize)
        };
        let (decoded_pixels, width, height) =
            if format == DWRITE_GLYPH_IMAGE_FORMATS_PREMULTIPLIED_B8G8R8A8 {
                let width = data.pixelSize.width;
                let height = data.pixelSize.height;
                if width == 0 || height == 0 {
                    return Ok(None);
                }
                let expected_len = width as usize * height as usize * 4;
                if pixels.len() < expected_len {
                    return Ok(None);
                }
                (pixels[..expected_len].to_vec(), width, height)
            } else {
                let Some((decoded_pixels, width, height)) = self.decode_wic_bitmap(pixels)? else {
                    return Ok(None);
                };
                (decoded_pixels, width, height)
            };

        let _lease = release;
        Ok(Some(RasterizedGlyph {
            atlas_kind: GlyphAtlasKind::Color,
            width,
            height,
            offset_x: -data.horizontalLeftOrigin.x,
            offset_y: -data.horizontalLeftOrigin.y,
            pixels: decoded_pixels,
        }))
    }

    fn decode_wic_bitmap(&self, bytes: &[u8]) -> Result<Option<(Vec<u8>, u32, u32)>> {
        let Some(factory) = &self.wic_factory else {
            return Ok(None);
        };
        if bytes.is_empty() {
            return Ok(None);
        }

        let stream = unsafe { factory.CreateStream()? };
        unsafe { stream.InitializeFromMemory(bytes)? };
        let decoder = unsafe {
            factory.CreateDecoderFromStream(
                &stream,
                std::ptr::null(),
                WICDecodeMetadataCacheOnDemand,
            )?
        };
        let frame = unsafe { decoder.GetFrame(0)? };
        let converter = unsafe { factory.CreateFormatConverter()? };
        unsafe {
            converter.Initialize(
                &frame,
                &GUID_WICPixelFormat32bppPBGRA,
                WICBitmapDitherTypeNone,
                None,
                0.0,
                WICBitmapPaletteTypeMedianCut,
            )?;
        }

        let mut width = 0;
        let mut height = 0;
        unsafe {
            converter.GetSize(&mut width, &mut height)?;
        }
        if width == 0 || height == 0 {
            return Ok(None);
        }

        let stride = width * 4;
        let mut pixels = vec![0u8; stride as usize * height as usize];
        unsafe {
            converter.CopyPixels(std::ptr::null(), stride, &mut pixels)?;
        }
        Ok(Some((pixels, width, height)))
    }

    fn rasterize_grayscale(
        &self,
        face2: &IDWriteFontFace2,
        glyph_index: u16,
        font_size: f32,
    ) -> Result<RasterizedGlyph> {
        let glyph_analysis = self.create_analysis(face2, glyph_index, font_size)?;
        let bounds = unsafe { glyph_analysis.GetAlphaTextureBounds(DWRITE_TEXTURE_ALIASED_1x1) }?;
        let width = (bounds.right - bounds.left).max(0) as u32;
        let height = (bounds.bottom - bounds.top).max(0) as u32;
        if width == 0 || height == 0 {
            return Ok(RasterizedGlyph {
                atlas_kind: GlyphAtlasKind::Grayscale,
                width: 0,
                height: 0,
                offset_x: 0,
                offset_y: 0,
                pixels: Vec::new(),
            });
        }

        let mut pixels = vec![0u8; (width * height) as usize];
        unsafe {
            glyph_analysis.CreateAlphaTexture(
                DWRITE_TEXTURE_ALIASED_1x1,
                &RECT {
                    left: bounds.left,
                    top: bounds.top,
                    right: bounds.right,
                    bottom: bounds.bottom,
                },
                &mut pixels,
            )?;
        }

        Ok(RasterizedGlyph {
            atlas_kind: GlyphAtlasKind::Grayscale,
            width,
            height,
            offset_x: bounds.left,
            offset_y: bounds.top,
            pixels,
        })
    }

    fn rasterize_color_outline_layer(
        &self,
        run: &DWRITE_COLOR_GLYPH_RUN1,
    ) -> Result<Option<ColorLayer>> {
        let glyph_analysis = self.create_analysis_from_run(&run.Base.glyphRun)?;
        let bounds = unsafe { glyph_analysis.GetAlphaTextureBounds(DWRITE_TEXTURE_ALIASED_1x1) }?;
        let width = (bounds.right - bounds.left).max(0) as u32;
        let height = (bounds.bottom - bounds.top).max(0) as u32;
        if width == 0 || height == 0 {
            return Ok(None);
        }

        let mut coverage = vec![0u8; (width * height) as usize];
        unsafe {
            glyph_analysis.CreateAlphaTexture(
                DWRITE_TEXTURE_ALIASED_1x1,
                &RECT {
                    left: bounds.left,
                    top: bounds.top,
                    right: bounds.right,
                    bottom: bounds.bottom,
                },
                &mut coverage,
            )?;
        }

        Ok(Some(ColorLayer {
            left: bounds.left,
            top: bounds.top,
            width,
            height,
            coverage,
        }))
    }

    fn create_analysis(
        &self,
        face2: &IDWriteFontFace2,
        glyph_index: u16,
        font_size: f32,
    ) -> Result<IDWriteGlyphRunAnalysis> {
        let face = face2.cast::<IDWriteFontFace>()?;
        let glyph_indices = [glyph_index];
        let advances = [0.0f32];
        let offsets = [DWRITE_GLYPH_OFFSET::default()];
        let glyph_run = DWRITE_GLYPH_RUN {
            fontFace: ManuallyDrop::new(Some(face.clone())),
            fontEmSize: font_size,
            glyphCount: 1,
            glyphIndices: glyph_indices.as_ptr(),
            glyphAdvances: advances.as_ptr(),
            glyphOffsets: offsets.as_ptr(),
            isSideways: false.into(),
            bidiLevel: 0,
        };

        let mut rendering_mode = DWRITE_RENDERING_MODE_NATURAL_SYMMETRIC;
        let mut grid_fit_mode = DWRITE_GRID_FIT_MODE_DEFAULT;
        unsafe {
            face2.GetRecommendedRenderingMode(
                font_size,
                96.0,
                96.0,
                None,
                false,
                DWRITE_OUTLINE_THRESHOLD_ANTIALIASED,
                DWRITE_MEASURING_MODE_NATURAL,
                None,
                &mut rendering_mode,
                &mut grid_fit_mode,
            )?;
        }
        if rendering_mode == DWRITE_RENDERING_MODE_OUTLINE {
            rendering_mode = DWRITE_RENDERING_MODE_NATURAL_SYMMETRIC;
        }

        Ok(unsafe {
            self.factory.CreateGlyphRunAnalysis(
                &glyph_run,
                None,
                rendering_mode,
                DWRITE_MEASURING_MODE_NATURAL,
                grid_fit_mode,
                DWRITE_TEXT_ANTIALIAS_MODE_GRAYSCALE,
                0.0,
                0.0,
            )?
        })
    }

    fn create_analysis_from_run(
        &self,
        glyph_run: &DWRITE_GLYPH_RUN,
    ) -> Result<IDWriteGlyphRunAnalysis> {
        Ok(unsafe {
            self.factory.CreateGlyphRunAnalysis(
                glyph_run,
                None,
                DWRITE_RENDERING_MODE_NATURAL_SYMMETRIC,
                DWRITE_MEASURING_MODE_NATURAL,
                DWRITE_GRID_FIT_MODE_DEFAULT,
                DWRITE_TEXT_ANTIALIAS_MODE_GRAYSCALE,
                0.0,
                0.0,
            )?
        })
    }
}

#[derive(Default)]
struct ColorCompose {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

struct ColorLayer {
    left: i32,
    top: i32,
    width: u32,
    height: u32,
    coverage: Vec<u8>,
}

impl ColorCompose {
    fn blend(&mut self, layer: &ColorLayer, run_color: DWRITE_COLOR_F) {
        self.ensure_bounds(
            layer.left,
            layer.top,
            layer.left + layer.width as i32,
            layer.top + layer.height as i32,
        );
        if self.width == 0 || self.height == 0 {
            return;
        }

        let a = (run_color.a.clamp(0.0, 1.0) * 255.0).round() as u8;
        let r = (run_color.r.clamp(0.0, 1.0) * 255.0).round() as u8;
        let g = (run_color.g.clamp(0.0, 1.0) * 255.0).round() as u8;
        let b = (run_color.b.clamp(0.0, 1.0) * 255.0).round() as u8;

        let start_x = (layer.left - self.left) as usize;
        let start_y = (layer.top - self.top) as usize;
        let dst_stride = self.width as usize * 4;
        let src_stride = layer.width as usize;

        for y in 0..layer.height as usize {
            let src_row = &layer.coverage[y * src_stride..(y + 1) * src_stride];
            let dst_row_start = (start_y + y) * dst_stride + start_x * 4;
            for (x, &coverage) in src_row.iter().enumerate() {
                if coverage == 0 || a == 0 {
                    continue;
                }
                let src_a = (u16::from(coverage) * u16::from(a) + 127) / 255;
                if src_a == 0 {
                    continue;
                }
                let src_b = ((u16::from(b) * src_a) + 127) / 255;
                let src_g = ((u16::from(g) * src_a) + 127) / 255;
                let src_r = ((u16::from(r) * src_a) + 127) / 255;

                let idx = dst_row_start + x * 4;
                let dst_b = u16::from(self.pixels[idx]);
                let dst_g = u16::from(self.pixels[idx + 1]);
                let dst_r = u16::from(self.pixels[idx + 2]);
                let dst_a = u16::from(self.pixels[idx + 3]);
                let inv_src_a = 255 - src_a;

                let out_b = src_b + ((dst_b * inv_src_a + 127) / 255);
                let out_g = src_g + ((dst_g * inv_src_a + 127) / 255);
                let out_r = src_r + ((dst_r * inv_src_a + 127) / 255);
                let out_a = src_a + ((dst_a * inv_src_a + 127) / 255);

                self.pixels[idx] = out_b.min(255) as u8;
                self.pixels[idx + 1] = out_g.min(255) as u8;
                self.pixels[idx + 2] = out_r.min(255) as u8;
                self.pixels[idx + 3] = out_a.min(255) as u8;
            }
        }
    }

    fn ensure_bounds(&mut self, left: i32, top: i32, right: i32, bottom: i32) {
        if right <= left || bottom <= top {
            return;
        }
        if self.width == 0 || self.height == 0 {
            self.left = left;
            self.top = top;
            self.right = right;
            self.bottom = bottom;
            self.recreate(0, 0);
            return;
        }
        let new_left = self.left.min(left);
        let new_top = self.top.min(top);
        let new_right = self.right.max(right);
        let new_bottom = self.bottom.max(bottom);
        if new_left == self.left
            && new_top == self.top
            && new_right == self.right
            && new_bottom == self.bottom
        {
            return;
        }
        let old_left = self.left;
        let old_top = self.top;
        let old_width = self.width as usize;
        let old_height = self.height as usize;
        let old_pixels = std::mem::take(&mut self.pixels);

        self.left = new_left;
        self.top = new_top;
        self.right = new_right;
        self.bottom = new_bottom;
        self.recreate(old_width, old_height);
        if old_width == 0 || old_height == 0 {
            return;
        }

        let copy_x = (old_left - self.left) as usize;
        let copy_y = (old_top - self.top) as usize;
        let new_stride = self.width as usize * 4;
        let old_stride = old_width * 4;
        for row in 0..old_height {
            let dst = (copy_y + row) * new_stride + copy_x * 4;
            let src = row * old_stride;
            self.pixels[dst..dst + old_stride].copy_from_slice(&old_pixels[src..src + old_stride]);
        }
    }

    fn recreate(&mut self, _old_width: usize, _old_height: usize) {
        self.width = (self.right - self.left).max(0) as u32;
        self.height = (self.bottom - self.top).max(0) as u32;
        self.pixels = vec![0u8; self.width as usize * self.height as usize * 4];
    }
}

struct GlyphImageLease {
    face4: IDWriteFontFace4,
    context: *mut core::ffi::c_void,
}

impl Drop for GlyphImageLease {
    fn drop(&mut self) {
        if self.context.is_null() {
            return;
        }
        unsafe {
            self.face4.ReleaseGlyphImageData(self.context);
        }
    }
}

#[inline]
fn glyph_image_formats_any(
    value: DWRITE_GLYPH_IMAGE_FORMATS,
    mask: DWRITE_GLYPH_IMAGE_FORMATS,
) -> bool {
    (value.0 & mask.0) != 0
}

fn measure_cell_metrics(
    shared_grid: &SharedGrid,
    dwrite_factory: &IDWriteFactory2,
    config: &RendererTextConfig,
) -> CellMetrics {
    if config.cell_width.as_f32() > 0.0 && config.line_height.as_f32() > 0.0 {
        return CellMetrics {
            cell_width: config.cell_width.as_f32(),
            line_height: config.line_height.as_f32(),
            baseline: config.baseline.as_f32().max(0.0),
        };
    }

    let grid_metrics = shared_grid.metrics();
    if let Some(metrics) =
        measure_primary_face_metrics(shared_grid, dwrite_factory, config, grid_metrics)
    {
        return metrics;
    }

    if grid_metrics.cell_width > 0 && grid_metrics.cell_height > 0 {
        let line_height = grid_metrics.cell_height as f32;
        return CellMetrics {
            cell_width: grid_metrics.cell_width as f32,
            line_height,
            baseline: config.baseline.as_f32().max(line_height * 0.8),
        };
    }

    CellMetrics {
        cell_width: config.font_size.as_f32() * 0.6,
        line_height: config.font_size.as_f32() * 1.3,
        baseline: config.font_size.as_f32(),
    }
}

fn measure_primary_face_metrics(
    shared_grid: &SharedGrid,
    dwrite_factory: &IDWriteFactory2,
    config: &RendererTextConfig,
    _grid_metrics: GridMetrics,
) -> Option<CellMetrics> {
    let face2 = shared_grid.face_for_index(FontIndex::new(Style::Normal, 0))?;
    let face = face2.cast::<IDWriteFontFace>().ok()?;
    let measured =
        measure_face_dimensions(&face, &face2, dwrite_factory, config.font_size.as_f32()).ok()?;
    if measured.face_height <= 0.0 {
        return None;
    }

    let cell_width = if config.cell_width.as_f32() > 0.0 {
        config.cell_width.as_f32()
    } else {
        measured.advance_width.round().max(1.0)
    };
    let line_height = if config.line_height.as_f32() > 0.0 {
        config.line_height.as_f32().max(1.0)
    } else {
        measured.face_height.round().max(1.0)
    };
    let baseline = if config.baseline.as_f32() > 0.0 {
        config.baseline.as_f32().clamp(0.0, line_height)
    } else {
        (measured.ascent + (measured.line_gap + line_height - measured.face_height) / 2.0)
            .round()
            .clamp(0.0, line_height)
    };

    Some(CellMetrics {
        cell_width,
        line_height,
        baseline,
    })
}

#[derive(Clone, Copy)]
struct FaceDimensions {
    advance_width: f32,
    ascent: f32,
    line_gap: f32,
    face_height: f32,
}

fn measure_face_dimensions(
    face: &IDWriteFontFace,
    _face2: &IDWriteFontFace2,
    _dwrite_factory: &IDWriteFactory2,
    font_size: f32,
) -> Result<FaceDimensions> {
    let metrics = extract_metrics(face, font_size)?;
    let advance_width = measure_zero_advance_width(face, font_size)?
        .unwrap_or(font_size * 0.5)
        .max(1.0);
    Ok(FaceDimensions {
        advance_width,
        ascent: metrics.ascent,
        line_gap: metrics.line_gap,
        face_height: (metrics.ascent + metrics.descent + metrics.line_gap).max(1.0),
    })
}

fn measure_zero_advance_width(face: &IDWriteFontFace, font_size: f32) -> Result<Option<f32>> {
    let mut raw = DWRITE_FONT_METRICS::default();
    unsafe {
        face.GetMetrics(&mut raw);
    }
    if raw.designUnitsPerEm == 0 {
        return Ok(None);
    }

    let codepoint = ['0' as u32];
    let mut glyph_index = [0u16; 1];
    unsafe {
        face.GetGlyphIndices(codepoint.as_ptr(), 1, glyph_index.as_mut_ptr())?;
    }
    if glyph_index[0] == 0 {
        return Ok(None);
    }

    let mut glyph_metrics = [DWRITE_GLYPH_METRICS::default(); 1];
    unsafe {
        face.GetDesignGlyphMetrics(glyph_index.as_ptr(), 1, glyph_metrics.as_mut_ptr(), false)?;
    }
    let scale = font_size / raw.designUnitsPerEm as f32;
    Ok(Some(glyph_metrics[0].advanceWidth as f32 * scale))
}

pub(crate) fn grid_metrics_from_renderer_config(config: &RendererTextConfig) -> GridMetrics {
    let cell_width = config
        .cell_width
        .as_f32()
        .max(config.font_size.as_f32() * 0.6)
        .round()
        .max(1.0) as u16;
    let cell_height = config
        .line_height
        .as_f32()
        .max(config.font_size.as_f32() * 1.3)
        .round()
        .max(1.0) as u16;
    GridMetrics {
        cell_width,
        cell_height,
    }
}

fn resolve_fg_color(
    style: Option<&CellStyle>,
    palette: &[ColorRGB; 256],
    default: Color32,
) -> Color32 {
    match style {
        None => default,
        Some(style) => match style.fg.tag {
            1 => rgb_to_color32(palette[style.fg.r as usize]),
            2 => Color32::from_rgba(style.fg.r, style.fg.g, style.fg.b, 255),
            _ => default,
        },
    }
}

fn resolve_bg_color(
    style: Option<&CellStyle>,
    palette: &[ColorRGB; 256],
    default: Color32,
) -> Color32 {
    match style {
        None => default,
        Some(style) => match style.bg.tag {
            1 => rgb_to_color32(palette[style.bg.r as usize]),
            2 => Color32::from_rgba(style.bg.r, style.bg.g, style.bg.b, 255),
            _ => default,
        },
    }
}

fn resolve_bg_only(raw: RawCell, palette: &[ColorRGB; 256], default: Color32) -> Color32 {
    match raw.content_tag() {
        2 => rgb_to_color32(palette[raw.bg_palette_index() as usize]),
        3 => {
            let (r, g, b) = raw.bg_rgb();
            Color32::from_rgba(r, g, b, 255)
        }
        _ => default,
    }
}

fn rgb_to_color32(rgb: ColorRGB) -> Color32 {
    Color32::from_rgba(rgb.r, rgb.g, rgb.b, 255)
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

#[derive(Clone, Copy, PartialEq, Eq)]
struct Color32(u32);

impl Color32 {
    fn from_rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self(u32::from_le_bytes([r, g, b, a]))
    }

    fn to_rgba(self) -> [u8; 4] {
        self.0.to_le_bytes()
    }

    fn a(self) -> u8 {
        self.to_rgba()[3]
    }

    fn with_alpha(self, a: u8) -> Self {
        let [r, g, b, _] = self.to_rgba();
        Self::from_rgba(r, g, b, a)
    }

    fn to_float4(self) -> [f32; 4] {
        let [r, g, b, a] = self.to_rgba();
        [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, a as f32 / 255.0]
    }

    fn to_rgba_u32(self) -> u32 {
        self.0
    }
}
