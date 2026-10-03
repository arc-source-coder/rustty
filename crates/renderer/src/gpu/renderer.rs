use anyhow::{Result, anyhow};

use font::cache::cache_table::CacheTable;
use font::metrics::FontMetrics;
use font::shared_grid::SharedGrid;
use font::types::{Cell, FontIndex, ShapeOptions, SpecialFont, TextRun};

use font::shaper::Shaper;
use font::shaper::run_iterator::{RunIterator, RunOptions};
use ghostty::sprite::{Sprite, SpriteRasterizer};
use ghostty::{
    BoldColor, CellStyle, CellView, Color, CursorVisualStyle, Dirty, OptionalCursorViewport,
    OptionalSelection, RawCell, RenderColors, RenderCursor, RenderState, ScrollbarInfo, Terminal,
    UnderlineStyle, Width, ZigMultiArrayList,
};
use gpui::ExternalSurfaceState;
use rustc_hash::FxHashMap;
use utils::asserts::{assert, unreachable};

use std::collections::hash_map::Entry;
use std::ops::ControlFlow;
use std::sync::Arc;
use windows::Win32::Graphics::Dxgi::IDXGISwapChain2;

#[cfg(target_os = "windows")]
use crate::backend::d3d11::D3D11 as Backend;
use crate::backend::d3d11::{BackendOptions, GpuContext};
use crate::font::atlas::{AtlasFormat, AtlasResources};

#[cfg(target_os = "windows")]
use crate::font::d2d::D2D as FontBackend;

use crate::font::d2d::Options;
use crate::font::types::{Glyph, GlyphKey, RenderOptions, TextRenderingParams};
use crate::gpu::RendererUiUpdate;
use crate::types::{DamageTracker, DerivedConfig, DirtyRect, FrameOutcome, GridSize, QuadInstance};

// TODO: Doc comments
pub struct Renderer {
    backend: Backend,
    terminal: Arc<Terminal>,
    grid: Arc<SharedGrid>,
    config: DerivedConfig,
    state: RendererState,
    shaper: Shaper,
    /// Cache used to map run hashes to shaped cell arrays.
    ///
    /// Ghostty caches an owned `[]font.shape.Cell`. Its 256 buckets with
    /// 8 entries each balance frequently used runs against collision churn.
    ///
    /// Ghostty reference: `zig/ghostty/src/font/shaper/Cache.zig`
    shaper_cache: CacheTable<Box<[Cell]>, 256, 8>,
    contents: Contents,
    rasterizer: Rasterizer,
    force_full_rebuild: bool,
}

type UpdateResult<T> = Result<T, ghostty::RenderUpdateError>;

struct RendererState {
    metrics: FontMetrics,
    last_scrollbar: Option<ScrollbarInfo>,
    ui_tx: async_channel::Sender<RendererUiUpdate>,
    surface_occluded: bool,
    focused: bool,
    cursor_blinking: bool,
}

trait RenderCursorExt {
    fn effective_style(&self, focused: bool, blink_visible: bool) -> Option<CursorVisualStyle>;
}

impl RenderCursorExt for RenderCursor {
    fn effective_style(&self, focused: bool, blink_visible: bool) -> Option<CursorVisualStyle> {
        if self.viewport.into_option().is_none() || !self.visible {
            return None;
        }
        if !focused {
            return Some(CursorVisualStyle::BlockHollow);
        }
        if self.blinking && !blink_visible {
            return None;
        }

        Some(self.visual_style)
    }
}

impl Renderer {
    pub fn new(
        config: DerivedConfig,
        terminal: Arc<Terminal>,
        grid: Arc<SharedGrid>,
        ui_tx: async_channel::Sender<RendererUiUpdate>,
        focused: bool,
    ) -> Result<Self> {
        let mut backend = Backend::new(BackendOptions {
            background_opacity: config.background_opacity,
        })?;

        let shaper = Shaper::new(ShapeOptions {
            features: config.features.as_slice(),
        })?;
        let rasterizer = Rasterizer::new(backend.gpu_context(), &grid)?;
        backend.set_text_rendering_params(rasterizer.text_rendering_params());

        let state = RendererState {
            metrics: grid.metrics.clone(),
            last_scrollbar: None,
            surface_occluded: false,
            focused,
            cursor_blinking: false,
            ui_tx,
        };

        backend.set_cell_size(
            state.metrics.cell_width as f32,
            state.metrics.cell_height as f32,
        );

        Ok(Self {
            backend,
            terminal,
            grid,
            state,
            config,
            shaper,
            shaper_cache: CacheTable::new(),
            contents: Contents::default(),
            rasterizer,
            force_full_rebuild: false,
        })
    }

    #[inline]
    pub fn swap_chain(&self) -> &IDXGISwapChain2 {
        self.backend.swap_chain()
    }

    pub fn set_font_grid(&mut self, grid: Arc<SharedGrid>) -> Result<()> {
        let rasterizer = Rasterizer::new(self.backend.gpu_context(), &grid)?;
        self.backend
            .set_text_rendering_params(rasterizer.text_rendering_params());

        self.state.metrics = grid.metrics.clone();
        self.grid = grid;

        self.rasterizer = rasterizer;

        self.shaper_cache.clear();
        self.contents.reset();

        self.backend.set_cell_size(
            self.state.metrics.cell_width as f32,
            self.state.metrics.cell_height as f32,
        );
        self.force_full_rebuild = true;

        Ok(())
    }

    #[inline]
    pub fn set_focused(&mut self, focused: bool) {
        self.state.focused = focused;
    }

    #[inline]
    pub fn cursor_blink_active(&self) -> bool {
        self.state.focused && self.state.cursor_blinking && !self.state.surface_occluded
    }

    pub fn apply_surface_state(&mut self, state: ExternalSurfaceState) -> Result<bool> {
        let became_visible = self.state.surface_occluded && !state.occluded;
        self.state.surface_occluded = state.occluded;

        if state.occluded {
            return Ok(false);
        }

        let needs_redraw = self.backend.apply_surface_state(state)?;

        Ok(became_visible || needs_redraw)
    }

    pub fn update_frame(&mut self, state: &mut RenderState, blink_visible: bool) -> Result<()> {
        let (pending, scrollbar) = self.terminal.with_lock(|terminal| -> UpdateResult<_> {
            let pending = terminal.begin_update(state)?;
            let scrollbar = terminal.scrollbar_info();
            Ok((pending, scrollbar))
        })?;
        let frame = pending.finish();

        if self.state.last_scrollbar != Some(scrollbar) {
            self.state.last_scrollbar = Some(scrollbar);
            let msg = RendererUiUpdate::Scrollbar(scrollbar);
            // TODO: courier
            self.state.ui_tx.try_send(msg).ok();
        }

        let (rows, columns) = frame.dimensions();

        let new_size = GridSize { rows, columns };
        let size_changed = self.contents.size != new_size;
        if size_changed {
            self.backend.set_grid_size(new_size);
            self.contents.resize(new_size);
        }

        let dirty = frame.dirty();
        let cell_height = self.state.metrics.cell_height;
        self.contents.damage.begin_frame(cell_height);

        let colors = frame.colors();
        let background = colors.background.to_bytes();
        let alpha = self.config.background_opacity;
        let background = [
            f32::from(background[0]) / 255.0 * alpha,
            f32::from(background[1]) / 255.0 * alpha,
            f32::from(background[2]) / 255.0 * alpha,
            alpha,
        ];
        let background_changed = self.backend.set_background(background);

        let mut full_rebuild =
            size_changed || background_changed || dirty == Dirty::Full || self.force_full_rebuild;
        if full_rebuild {
            self.contents.reset();
        }

        let row_data = frame.render_rows();
        let cursor = frame.render_cursor();
        self.state.cursor_blinking =
            cursor.visible && cursor.blinking && cursor.viewport.into_option().is_some();
        let cursor_style = cursor.effective_style(self.state.focused, blink_visible);

        // A full rebuild clears the retained CPU background buffer before rows
        // are rebuilt. Default cells also resolve to zero, so comparisons alone
        // cannot observe that old explicit backgrounds were removed.
        let mut bg_changed = full_rebuild;

        let mut reset_grayscale_atlas = false;
        let mut reset_color_atlas = false;
        'attempt: loop {
            let mut reset = None;

            let frame_rows = row_data
                .dirty_rows()
                .iter()
                .zip(row_data.selections())
                .zip(row_data.cell_multi_array_lists())
                .enumerate();

            for (y, ((&row_dirty, &selection), cell_multi_array_list)) in frame_rows {
                if !full_rebuild {
                    if dirty != Dirty::Partial || !row_dirty {
                        continue;
                    }
                    // Ghostty: self.cells.clear(y) then self.rebuildRow(y, ...)
                    self.contents.clear(y as u16);
                }
                let status = self.rebuild_row(
                    y,
                    &mut bg_changed,
                    cell_multi_array_list,
                    colors,
                    selection,
                    cursor.viewport,
                )?;
                if let ControlFlow::Break(atlas) = status {
                    reset = Some(atlas);
                    break;
                }
            }

            let cursor_instance = match reset {
                None => match self.build_cursor(cursor, cursor_style, colors)? {
                    ControlFlow::Continue(cursor) => cursor,
                    ControlFlow::Break(atlas) => {
                        reset = Some(atlas);
                        None
                    }
                },
                Some(_) => None,
            };
            self.rasterizer.finalize()?;

            let Some(atlas) = reset else {
                self.contents.set_cursor(cursor_instance, cursor_style);
                self.force_full_rebuild = false;
                break 'attempt;
            };
            std::hint::cold_path();

            let already_reset = match atlas {
                AtlasFormat::Grayscale => std::mem::replace(&mut reset_grayscale_atlas, true),
                AtlasFormat::Bgra => std::mem::replace(&mut reset_color_atlas, true),
            };

            self.contents.reset();
            self.contents.damage.begin_frame(cell_height);

            full_rebuild = true;
            bg_changed = true;

            if already_reset {
                return Err(anyhow!("atlas exceeded capacity"));
            }
        }

        if bg_changed {
            self.contents.bg_generation = self.contents.bg_generation.wrapping_add(1);
        }

        frame.mark_clean();
        Ok(())
    }

    fn build_cursor(
        &mut self,
        cursor: &RenderCursor,
        style: Option<CursorVisualStyle>,
        colors: &RenderColors,
    ) -> Result<ControlFlow<AtlasFormat, Option<QuadInstance>>> {
        let text_color = colors.background.with_alpha(255);

        let Some(style) = style else {
            self.backend.set_cursor(None, text_color);
            return Ok(ControlFlow::Continue(None));
        };
        let Some(viewport) = cursor.viewport.into_option() else {
            self.backend.set_cursor(None, text_color);
            return Ok(ControlFlow::Continue(None));
        };

        let x = viewport.x.saturating_sub(u16::from(viewport.wide_tail));
        let grid_width = u8::from(viewport.wide_tail || cursor.cell.width() == Width::Wide) + 1;
        let metrics = &self.state.metrics;
        let origin = [
            (u32::from(x) * metrics.cell_width) as i16,
            (u32::from(viewport.y) * metrics.cell_height) as i16,
        ];
        let cell_size = [
            metrics.cell_width as u16 * u16::from(grid_width),
            metrics.cell_height as u16,
        ];
        let color = colors
            .cursor_color()
            .unwrap_or(colors.foreground)
            .with_alpha(255);

        if style == CursorVisualStyle::Block {
            let text_rect = [
                f32::from(origin[0]),
                f32::from(origin[1]),
                f32::from(origin[0]) + f32::from(cell_size[0]),
                f32::from(origin[1]) + f32::from(cell_size[1]),
            ];
            self.backend.set_cursor(Some(text_rect), text_color);
            return Ok(ControlFlow::Continue(Some(QuadInstance::solid_rect(
                origin, cell_size, color,
            ))));
        }

        let sprite = match style {
            CursorVisualStyle::BlockHollow => Sprite::CursorHollowRect,
            CursorVisualStyle::Bar => Sprite::CursorBar,
            CursorVisualStyle::Underline => Sprite::CursorUnderline,
            CursorVisualStyle::Block => unreachable(),
        };

        let key = GlyphKey::new(FontIndex::SPRITE, sprite as u32, grid_width);
        let glyph = match self.rasterizer.render_glyph(key, metrics, &self.grid)? {
            ControlFlow::Continue(glyph) => glyph,
            ControlFlow::Break(atlas) => return Ok(ControlFlow::Break(atlas)),
        };
        self.backend.set_cursor(None, text_color);
        if glyph.width == 0 || glyph.height == 0 {
            return Ok(ControlFlow::Continue(None));
        }

        let position = [
            (i32::from(origin[0]) + glyph.offset_x) as i16,
            (i32::from(origin[1]) + glyph.offset_y) as i16,
        ];
        let mut instance = QuadInstance::glyph_rect(position, [glyph.width, glyph.height], color);
        instance.set_texcoord(glyph.atlas_x, glyph.atlas_y);
        Ok(ControlFlow::Continue(Some(instance)))
    }

    fn rebuild_row(
        &mut self,
        y: usize,
        bg_changed: &mut bool,
        cell_mal: &ZigMultiArrayList,
        colors: &RenderColors,
        selection: OptionalSelection,
        viewport: OptionalCursorViewport,
    ) -> Result<ControlFlow<AtlasFormat>> {
        let cells = CellView::from(cell_mal);
        let selection = selection.into_option();

        let cells_raw = cells.raw_cells();
        let cell_styles = cells.styles();
        let cells_len = cells_raw.len().min(self.contents.size.columns as usize);
        assert(cells_len == self.contents.size.columns as usize);

        let run_iterator_options = RunOptions {
            cells: &cells,
            grid: &self.grid,
            cursor_x: 'cursor_x: {
                let Some(vp) = viewport.into_option() else {
                    break 'cursor_x None;
                };
                if vp.y != y as u16 {
                    break 'cursor_x None;
                }
                Some(vp.x as usize)
            },
            selection,
        };

        let mut run_iterator = RunIterator::new(run_iterator_options);
        let mut shaper_run: Option<TextRun> = run_iterator.next(&mut self.shaper)?;
        let mut shaper_cells: Option<&[Cell]> = None;
        let mut shaper_cells_i: usize = 0;

        let mut row_contents = self.contents.begin_row(y, cells_len);
        let cell_iterator = cells_raw[0..cells_len]
            .iter()
            .zip(cell_styles[0..cells_len].iter())
            .zip(row_contents.background.iter_mut())
            .enumerate();

        let mut row_bg_changed = false;
        for (x, ((&cell, style), current_bg)) in cell_iterator {
            let style = match cell.has_styling() {
                true => unsafe { style.assume_init_ref() },
                false => CellStyle::DEFAULT,
            };
            let (bg, fg) = Self::resolve_colors(x, cell, style, colors, selection);

            if *current_bg != bg {
                *current_bg = bg;
                row_bg_changed = true;
            }

            // If the cell is invisible, skip foreground elements
            if style.is_invisible() {
                continue;
            }

            let alpha = fg[3];
            let underline = match style.underline_style() {
                UnderlineStyle::None => None,
                UnderlineStyle::Single => Some(Sprite::Underline),
                UnderlineStyle::Double => Some(Sprite::UnderlineDouble),
                UnderlineStyle::Curly => Some(Sprite::UnderlineCurly),
                UnderlineStyle::Dotted => Some(Sprite::UnderlineDotted),
                UnderlineStyle::Dashed => Some(Sprite::UnderlineDashed),
            };
            if let Some(sprite) = underline {
                let color = match style.underline.color() {
                    Color::None => fg,
                    Color::Palette(index) => colors.palette[index as usize].with_alpha(alpha),
                    Color::Rgb(color) => color.with_alpha(alpha),
                };
                let key = GlyphKey::new(FontIndex::SPRITE, sprite as u32, 1);
                if let ControlFlow::Break(atlas) = self.rasterizer.add_glyph(
                    x as u32,
                    y as u32,
                    key,
                    [0; 2],
                    color,
                    &self.grid,
                    &self.state.metrics,
                    &mut row_contents.foreground,
                )? {
                    return Ok(ControlFlow::Break(atlas));
                }
            }

            if style.is_overline()
                && let ControlFlow::Break(atlas) = self.rasterizer.add_glyph(
                    x as u32,
                    y as u32,
                    GlyphKey::new(FontIndex::SPRITE, Sprite::Overline as u32, 1),
                    [0; 2],
                    fg,
                    &self.grid,
                    &self.state.metrics,
                    &mut row_contents.foreground,
                )?
            {
                return Ok(ControlFlow::Break(atlas));
            }

            if shaper_cells.is_some_and(|cells| shaper_cells_i >= cells.len()) {
                shaper_run = run_iterator.next(&mut self.shaper)?;
                shaper_cells = None;
                shaper_cells_i = 0;
            }

            if let Some(run) = shaper_run.as_ref() {
                'glyphs: {
                    let shaped_cells = match shaper_cells {
                        Some(shaper_cells) => shaper_cells,
                        None => match self.shaper_cache.get(run.hash) {
                            Some(shaped) => {
                                shaper_cells = Some(shaped);
                                shaped
                            }
                            None => {
                                // Shape the new cells
                                let new_cells = self.shaper.shape(run)?;
                                // Cache the new shaped run
                                let _evicted =
                                    self.shaper_cache.put(run.hash, Box::from(new_cells));
                                shaper_cells = Some(new_cells);
                                new_cells
                            }
                        },
                    };
                    let shaped_len = shaped_cells.len();
                    if shaped_len == 0 {
                        break 'glyphs;
                    }

                    // X position is assumed to be monotonically increasing.
                    assert(run.offset + shaped_cells[shaper_cells_i].x >= x as u16);

                    // Note: This code assumes that runs are disjoint (A single cell
                    // will never be present in more than one shaper run).

                    // Process all glyphs for this cell
                    while shaper_cells_i < shaped_len
                        && run.offset + shaped_cells[shaper_cells_i].x == x as u16
                    {
                        let shaper_cell = &shaped_cells[shaper_cells_i];
                        let status = self.rasterizer.add_glyph(
                            x as u32,
                            y as u32,
                            GlyphKey::new(
                                run.font_index,
                                shaper_cell.glyph_index,
                                cell.grid_width(),
                            ),
                            [shaper_cell.x_offset, shaper_cell.y_offset],
                            fg,
                            &self.grid,
                            &self.state.metrics,
                            &mut row_contents.foreground,
                        )?;
                        if let ControlFlow::Break(atlas) = status {
                            return Ok(ControlFlow::Break(atlas));
                        }
                        shaper_cells_i += 1;
                    }
                }
            }

            if style.is_strikethrough()
                && let ControlFlow::Break(atlas) = self.rasterizer.add_glyph(
                    x as u32,
                    y as u32,
                    GlyphKey::new(FontIndex::SPRITE, Sprite::Strikethrough as u32, 1),
                    [0; 2],
                    fg,
                    &self.grid,
                    &self.state.metrics,
                    &mut row_contents.foreground,
                )?
            {
                return Ok(ControlFlow::Break(atlas));
            }
        }

        *bg_changed |= row_bg_changed;
        self.contents.damage.finish_row();

        Ok(ControlFlow::Continue(()))
    }

    // Add doc comments
    // Returns a premultiplied background and straight foreground color.
    fn resolve_colors(
        x: usize,
        cell: RawCell,
        style: &CellStyle,
        colors: &RenderColors,
        selection: Option<[u16; 2]>,
    ) -> ([u8; 4], [u8; 4]) {
        // TODO: When adding search, this bool should be turned into an enum.
        let selected: bool = 'selected: {
            let mut x_compare = x;
            if cell.width() == Width::SpacerTail {
                x_compare = x_compare.saturating_sub(1);
            }
            if let Some([sel_start, sel_end]) = selection {
                // x_compare is now the logical column if the character was wide
                if x_compare >= sel_start as usize && x_compare <= sel_end as usize {
                    break 'selected true;
                }
            }
            false
        };

        // Colors based on cell style (SGR)
        let bg_style = style.bg(&cell, &colors.palette);
        let fg_style = style.fg(colors.foreground, &colors.palette, BoldColor::None);

        let bg = 'bg: {
            if selected {
                // TODO: Make this configurable
                break 'bg colors.foreground;
            }
            // Check if the codepoint is a covering character
            let is_covering = cell.codepoint() == 0x2588; // U+2588 FULL BLOCK
            if is_covering ^ style.is_inverse() {
                // If the cell is either a covering character or has
                // inverse style, but not both, invert the color.
                break 'bg fg_style;
            }
            bg_style.unwrap_or(colors.background)
        };

        let fg = 'fg: {
            if selected {
                // TODO: Make this configurable
                break 'fg colors.background;
            }
            match style.is_inverse() {
                // Use the original background color.
                // `bg` has been inverted by this point.
                true => bg_style.unwrap_or(colors.background),
                false => fg_style,
            }
        };

        // TODO: Make this configurable.
        let fg_alpha: u8 = if style.is_faint() { 128 } else { 255 };
        // Ghostty-based logic for background alpha
        let bg_alpha = 'bg_alpha: {
            const DEFAULT_BG_ALPHA: u8 = 255;

            // Selected cells and reversed celld should be opaque
            if selected || style.is_inverse() {
                break 'bg_alpha DEFAULT_BG_ALPHA;
            }

            // TODO: Add configurable opacity here

            // Cells with an explicit bg color should be fully opaque
            if bg_style.is_some() {
                break 'bg_alpha DEFAULT_BG_ALPHA;
            }

            // Don't draw a background for this cell.
            // Let the default background color show through.
            0
        };

        let resolved_fg = fg.with_alpha(fg_alpha);

        let resolved_bg = match bg_alpha {
            0 => [0; 4],
            255 => bg.with_alpha(255),
            a => {
                let [r, g, b] = bg.to_bytes();
                let scale = |c| ((u16::from(c) * u16::from(a) + 127) / 255) as u8;
                [scale(r), scale(g), scale(b), a]
            }
        };

        (resolved_bg, resolved_fg)
    }

    pub fn wait_for_frame(&self) {
        self.backend.wait_for_frame()
    }

    pub fn draw_frame(&mut self) -> Result<FrameOutcome> {
        self.backend.prepare();

        self.backend.sync_instances(
            self.contents.fg_rows.lists.as_slice(),
            self.contents.fg_count as u32,
        )?;
        self.backend.sync_background(
            self.contents.bg_cells.as_slice(),
            self.contents.bg_generation,
        )?;

        // `self.rasterizer.atlases()` is a workaround since Direct2D needs to own its atlas.
        // Ownership of the atlases should move into the backend when replacing D2D.
        self.backend
            .draw(self.rasterizer.atlases(), self.contents.fg_count);

        Ok(self.backend.present(self.contents.damage.rect())?)
    }
}

struct Rasterizer {
    font_backend: FontBackend,
    sprite_rasterizer: SpriteRasterizer,
    glyph_cache: FxHashMap<GlyphKey, Glyph>,
}

impl Rasterizer {
    fn new(gpu: &GpuContext, grid: &SharedGrid) -> Result<Self> {
        let (x_dpi, y_dpi) = grid.dpi();
        Ok(Self {
            font_backend: FontBackend::new(gpu, Options { x_dpi, y_dpi })?,
            sprite_rasterizer: SpriteRasterizer::new(grid.metrics.sprite_metrics())?,
            glyph_cache: FxHashMap::default(),
        })
    }

    fn text_rendering_params(&self) -> TextRenderingParams {
        self.font_backend.text_rendering_params()
    }

    fn finalize(&mut self) -> Result<()> {
        self.font_backend.finalize()
    }

    fn atlases(&self) -> (&AtlasResources, &AtlasResources) {
        self.font_backend.atlases()
    }

    fn render_glyph(
        &mut self,
        key: GlyphKey,
        metrics: &FontMetrics,
        grid: &SharedGrid,
    ) -> Result<ControlFlow<AtlasFormat, Glyph>> {
        let glyph = match self.glyph_cache.entry(key) {
            Entry::Occupied(entry) => *entry.get(),
            Entry::Vacant(entry) => {
                let font_index = key.font_index();
                let glyph_index = key.glyph_index();
                let options = RenderOptions {
                    metrics,
                    grid_width: key.grid_width(),
                };

                let result = match font_index.special() {
                    Some(SpecialFont::Sprite) => {
                        let bitmap = self
                            .sprite_rasterizer
                            .rasterize(glyph_index, options.grid_width)?;
                        self.font_backend.upload_sprite(&bitmap)?
                    }
                    None => grid.with_face(font_index, |face| {
                        self.font_backend
                            .rasterize_and_upload(glyph_index, face, options)
                    })??,
                };

                match result {
                    ControlFlow::Continue(glyph) => *entry.insert(glyph),
                    ControlFlow::Break(atlas) => {
                        self.glyph_cache.retain(|_, glyph| glyph.atlas != atlas);
                        return Ok(ControlFlow::Break(atlas));
                    }
                }
            }
        };

        Ok(ControlFlow::Continue(glyph))
    }

    fn add_glyph(
        &mut self,
        x: u32,
        y: u32,
        key: GlyphKey,
        shaper_offset: [i16; 2],
        fg_color: [u8; 4],
        grid: &SharedGrid,
        metrics: &FontMetrics,
        fg_row: &mut ForegroundRow<'_>,
    ) -> Result<ControlFlow<AtlasFormat>> {
        let glyph = match self.render_glyph(key, metrics, grid)? {
            ControlFlow::Continue(glyph) => glyph,
            ControlFlow::Break(atlas) => return Ok(ControlFlow::Break(atlas)),
        };
        if glyph.width == 0 || glyph.height == 0 {
            return Ok(ControlFlow::Continue(()));
        }

        let pos_x = (x * metrics.cell_width) as i32 + glyph.offset_x + shaper_offset[0] as i32;
        // Shaper offset is subtracted because it is from font/shaper space.
        // Glyph offsets are in screen space (Postive Y -> move down).
        let pos_y = (y * metrics.cell_height) as i32 + glyph.offset_y - shaper_offset[1] as i32;

        let position = [pos_x as i16, pos_y as i16];
        let glyph_size = [glyph.width, glyph.height];
        let mut instance = match glyph.atlas {
            AtlasFormat::Grayscale => QuadInstance::glyph_rect(position, glyph_size, fg_color),
            AtlasFormat::Bgra => QuadInstance::color_glyph_rect(position, glyph_size),
        };
        instance.set_texcoord(glyph.atlas_x, glyph.atlas_y);
        fg_row.push(instance);
        Ok(ControlFlow::Continue(()))
    }
}

/// Ghostty: `cell.zig::Contents`
///
/// Row-owned persistent cell contents for the terminal grid.
/// Dirty rows are cleared and rebuilt in-place. Backends upload
/// directly from per-row lists (Ghostty `syncFromArrayLists` style).
#[derive(Default)]
struct Contents {
    pub size: GridSize,
    damage: DamageTracker,
    /// Flat array of background colors: Indexed by `bg_cells[row * cols + col]`.
    pub bg_cells: Vec<[u8; 4]>,
    /// Per-row foreground instance lists with cursor lanes.
    pub fg_rows: FgRows,
    /// Total foreground instance count across all lanes.
    ///
    /// This is calculated incrementally while mutating row/cursor lanes to
    /// avoid re-scanning all lists each frame just to compute draw count.
    pub fg_count: usize,
    pub bg_generation: u64,
}

impl Contents {
    /// Ghostty: `Contents.resize`
    pub fn resize(&mut self, size: GridSize) {
        self.size = size;
        let (rows, cols) = (usize::from(size.rows), usize::from(size.columns));
        // Resize background cells to cell count
        self.bg_cells.resize(rows * cols, [0; 4]);
        self.fg_rows.resize(rows, cols);
        self.damage.resize(rows);
        self.fg_count = 0;
    }

    /// Ghostty: `Contents.reset`
    pub fn reset(&mut self) {
        // Zero all background cells
        self.bg_cells.fill([0; 4]);
        self.fg_rows.reset();
        self.fg_count = 0;
    }

    /// Ghostty: `Contents.clear(y)` — clear row y's foreground list.
    ///
    /// `rebuild_row` overwrites every background cell. Retaining the prior values
    /// lets it detect unchanged row backgrounds and avoid a full background-buffer
    /// upload when only foreground content changed.
    pub fn clear(&mut self, y: u16) {
        // fg_rows index: y + 1 (index 0 is cursor-first)
        let list = &mut self.fg_rows.lists[usize::from(y) + 1];

        let removed = list.len();
        // Assert no fg_count underflow
        assert(self.fg_count >= removed);
        self.fg_count -= removed;
        list.clear();
    }

    #[inline]
    pub fn begin_row(&mut self, y: usize, cells_len: usize) -> RowContents<'_> {
        let start = y * self.size.columns as usize;
        let end = start + cells_len;

        RowContents {
            background: &mut self.bg_cells[start..end],
            foreground: ForegroundRow {
                // Lane 0 is cursor-before-text; Terminal row lanes start at 1.
                instances: &mut self.fg_rows.lists[y + 1],
                instance_count: &mut self.fg_count,
                damage_bounds: self.damage.begin_row(y),
            },
        }
    }

    /// Ghostty: `Contents.setCursor`
    ///
    /// Block cursors go in cursor-first (drawn before text).
    /// Bar/underline/hollow go in cursor-last (drawn after text).
    pub fn set_cursor(&mut self, cell: Option<QuadInstance>, style: Option<CursorVisualStyle>) {
        if self.size.rows == 0 {
            return;
        }

        let rows = self.size.rows as usize;
        for instance in self.fg_rows.lists[0]
            .iter()
            .chain(self.fg_rows.lists[rows + 1].iter())
        {
            self.damage.include(instance);
        }

        let removed = self.fg_rows.lists[0].len() + self.fg_rows.lists[rows + 1].len();
        assert(self.fg_count >= removed);
        self.fg_count -= removed;
        self.fg_rows.lists[0].clear();
        self.fg_rows.lists[rows + 1].clear();

        let (Some(cell), Some(style)) = (cell, style) else {
            return;
        };
        self.damage.include(&cell);
        let lane = match style {
            CursorVisualStyle::Block => 0,
            CursorVisualStyle::BlockHollow
            | CursorVisualStyle::Bar
            | CursorVisualStyle::Underline => rows + 1,
        };
        self.fg_rows.lists[lane].push(cell);
        self.fg_count += 1;
    }
}

struct RowContents<'a> {
    background: &'a mut [[u8; 4]],
    foreground: ForegroundRow<'a>,
}

struct ForegroundRow<'a> {
    instances: &'a mut Vec<QuadInstance>,
    instance_count: &'a mut usize,
    damage_bounds: &'a mut DirtyRect,
}

impl ForegroundRow<'_> {
    #[inline]
    pub fn push(&mut self, instance: QuadInstance) {
        self.damage_bounds.include(&instance);
        self.instances.push(instance);
        *self.instance_count += 1;
    }
}

/// Ghostty: `ArrayListCollection(CellText)` — owns per-row Vec allocations.
/// Layout: lists[0] = cursor-first, lists[1..=rows] = text rows, lists[rows+1] = cursor-last.
///
/// Ghostty reference:
///   `src/datastruct/array_list_collection.zig`
///   `src/renderer/cell.zig` — `Contents.fg_rows`
#[derive(Default)]
struct FgRows {
    pub lists: Vec<Vec<QuadInstance>>,
}

impl FgRows {
    /// Resize to `rows + 2` lists (cursor-first + N rows + cursor-last).
    /// Ghostty: `Contents.resize` → `ArrayListCollection.init(rows + 2, cols * 3)`.
    /// Retains surviving row allocations and reserves at least `cols * 3` per text
    /// row, matching Ghostty's heuristic (glyph + underline + strikethrough per column).
    fn resize(&mut self, rows: usize, cols: usize) {
        // The trailing cursor lane moves when the row count changes. Preserve
        // its allocation separately so it never becomes a text row.
        let mut cursor_last = self.lists.pop().unwrap_or_else(|| Vec::with_capacity(1));
        cursor_last.clear();

        if self.lists.is_empty() {
            self.lists.push(Vec::with_capacity(1));
        }
        self.lists.resize_with(rows + 1, Vec::new);
        self.lists[0].clear();

        for row in &mut self.lists[1..] {
            row.clear();
            row.reserve(cols * 3);
        }
        self.lists.push(cursor_last);
    }

    /// Ghostty: `ArrayListCollection.reset` — clear all lists, retain capacity.
    fn reset(&mut self) {
        for list in &mut self.lists {
            list.clear();
        }
    }
}
