use anyhow::Result;
use ghostty::sprite::SpriteBitmap;

use std::mem::ManuallyDrop;
use std::ops::ControlFlow;
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_COLOR_F};
use windows::Win32::Graphics::Direct2D::{
    D2D1_COLOR_BITMAP_GLYPH_SNAP_OPTION_DEFAULT, D2D1_DEVICE_CONTEXT_OPTIONS_NONE,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE, D2D1_UNIT_MODE_PIXELS,
    D2D1CreateFactory, ID2D1Device4, ID2D1DeviceContext4, ID2D1Factory5, ID2D1SolidColorBrush,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_COLOR_F, DWRITE_FACTORY_TYPE_SHARED, DWRITE_GLYPH_IMAGE_FORMATS_CFF,
    DWRITE_GLYPH_IMAGE_FORMATS_COLR, DWRITE_GLYPH_IMAGE_FORMATS_JPEG,
    DWRITE_GLYPH_IMAGE_FORMATS_NONE, DWRITE_GLYPH_IMAGE_FORMATS_PNG,
    DWRITE_GLYPH_IMAGE_FORMATS_PREMULTIPLIED_B8G8R8A8, DWRITE_GLYPH_IMAGE_FORMATS_SVG,
    DWRITE_GLYPH_IMAGE_FORMATS_TIFF, DWRITE_GLYPH_IMAGE_FORMATS_TRUETYPE, DWRITE_GLYPH_RUN,
    DWRITE_GRID_FIT_MODE_DEFAULT, DWRITE_MEASURING_MODE_NATURAL, DWRITE_PIXEL_GEOMETRY_FLAT,
    DWRITE_RENDERING_MODE1_NATURAL_SYMMETRIC, DWriteCreateFactory, IDWriteColorGlyphRunEnumerator1,
    IDWriteFactory7, IDWriteFontFace, IDWriteRenderingParams1, IDWriteRenderingParams3,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::core::Interface;
use windows_numerics::Vector2;

use crate::atlas_allocator::AtlasFullError;

use crate::font::atlas::{Atlas, AtlasFormat, AtlasOptions, AtlasResources, AtlasStatus};
use crate::font::types::{Glyph, RenderOptions, TextRenderingParams};
use crate::font::utils;

use crate::backend::d3d11::GpuContext;
use font::backend::dwrite::face::Face;

const WHITE: D2D1_COLOR_F = D2D1_COLOR_F {
    r: 1.0,
    g: 1.0,
    b: 1.0,
    a: 1.0,
};

pub struct Options {
    pub x_dpi: u16,
    pub y_dpi: u16,
}

pub struct D2D {
    gpu: GpuContext,
    ctx: ID2D1DeviceContext4,
    dwrite_factory: IDWriteFactory7,

    atlas_grayscale: Atlas,
    atlas_color: Atlas,

    brush: ID2D1SolidColorBrush,
    text_rendering_params: TextRenderingParams,

    current_target: Option<AtlasFormat>,
    draw_started: bool,
}

impl D2D {
    const INITIAL_ATLAS_SIDE: u16 = 512;

    pub fn new(gpu: &GpuContext, options: Options) -> Result<Self> {
        let factory: IDWriteFactory7 = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
        let d2d_factory: ID2D1Factory5 =
            unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)? };

        let dxgi_device: IDXGIDevice = gpu.device.cast()?;
        let d2d_device: ID2D1Device4 = unsafe { d2d_factory.CreateDevice(Some(&dxgi_device))? };

        // Disable D2D's internal glyph caches since we have our own
        unsafe { d2d_device.SetMaximumTextureMemory(0) };
        unsafe { d2d_device.SetMaximumColorGlyphCacheMemory(0) };

        let ctx: ID2D1DeviceContext4 =
            unsafe { d2d_device.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)? };

        unsafe { ctx.SetUnitMode(D2D1_UNIT_MODE_PIXELS) };
        unsafe { ctx.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE) };

        let mut options = AtlasOptions {
            side: Self::INITIAL_ATLAS_SIDE,
            x_dpi: options.x_dpi,
            y_dpi: options.y_dpi,
            format: AtlasFormat::Grayscale,
        };
        let atlas_grayscale = Atlas::new(&gpu, &ctx, options)?;

        options.format = AtlasFormat::Bgra;
        let atlas_color = Atlas::new(&gpu, &ctx, options)?;

        let mut text_rendering_params = TextRenderingParams::default();

        let default_params = unsafe { factory.CreateRenderingParams()? };
        let params_1: IDWriteRenderingParams1 = default_params.cast()?;

        let gamma = unsafe { default_params.GetGamma() };
        text_rendering_params.gamma_ratios = utils::get_gamma_correction_ratios(gamma);

        text_rendering_params.grayscale_enhanced_contrast =
            unsafe { params_1.GetGrayscaleEnhancedContrast() };

        // Disable D2D/DirectWrite gamma correction since we apply it in the shader
        let params: IDWriteRenderingParams3 = unsafe {
            factory.CreateCustomRenderingParams(
                1.0, // Gamma
                0.0, // EnhancedContrast
                0.0, // GrayscaleEnhancedContrast
                0.0, // ClearTypeLevel
                DWRITE_PIXEL_GEOMETRY_FLAT,
                DWRITE_RENDERING_MODE1_NATURAL_SYMMETRIC,
                DWRITE_GRID_FIT_MODE_DEFAULT,
            )?
        };
        unsafe { ctx.SetTextRenderingParams(&params) };

        let color = WHITE;
        let brush = unsafe { ctx.CreateSolidColorBrush(&raw const color, None)? };

        Ok(Self {
            atlas_grayscale,
            atlas_color,
            gpu: gpu.clone(),
            ctx,
            dwrite_factory: factory,
            text_rendering_params,
            brush,
            current_target: None,
            draw_started: false,
        })
    }

    pub fn prepare(&mut self) {
        if !self.draw_started {
            unsafe { self.ctx.BeginDraw() };
            self.draw_started = true;
        }
    }

    /// End pending D2D drawing before the backend samples the atlas textures.
    pub fn finalize(&mut self) -> Result<()> {
        if self.draw_started {
            self.draw_started = false;
            unsafe { self.ctx.EndDraw(None, None)? };
        }
        Ok(())
    }

    #[inline]
    pub fn text_rendering_params(&self) -> TextRenderingParams {
        self.text_rendering_params
    }

    #[inline]
    pub fn atlases(&self) -> (&AtlasResources, &AtlasResources) {
        (&self.atlas_grayscale.resources, &self.atlas_color.resources)
    }

    /// Upload a sprite into the grayscale atlas.
    /// `Break` means the atlas was cleared: invalidate its cached glyphs and
    /// retained quads, then retry the frame. `Continue` returns the glyph metadata.
    pub fn upload_sprite(
        &mut self,
        bitmap: &SpriteBitmap<'_>,
    ) -> Result<ControlFlow<AtlasFormat, Glyph>> {
        if bitmap.width == 0 || bitmap.height == 0 {
            return Ok(ControlFlow::Continue(Glyph::default()));
        }

        let (width, height) = (bitmap.width as u16, bitmap.height as u16);
        let region = match self.atlas_grayscale.reserve(width, height) {
            Ok(region) => region,
            Err(AtlasFullError) => {
                self.finalize()?;

                let atlas = &mut self.atlas_grayscale;
                if let AtlasStatus::Cleared(atlas) = atlas.grow(&self.gpu, &self.ctx)? {
                    self.current_target = None;
                    return Ok(ControlFlow::Break(atlas));
                }
                self.current_target = None;

                let region = self.atlas_grayscale.reserve(width, height)?;
                region
            }
        };

        self.atlas_grayscale
            .upload(region, bitmap.pixels, bitmap.stride)?;

        let glyph = Glyph {
            height: region.height,
            width: region.width,
            offset_x: bitmap.offset_x,
            offset_y: bitmap.offset_y,
            atlas_x: region.x,
            atlas_y: region.y,
            atlas: AtlasFormat::Grayscale,
        };

        Ok(ControlFlow::Continue(glyph))
    }

    /// Draw a face glyph into its atlas, batching D2D work until `finalize`.
    /// `Break` means the atlas was cleared: invalidate its cached glyphs and
    /// retained quads, then retry the frame. An empty glyph returns `Continue`.
    pub fn rasterize_and_upload(
        &mut self,
        gid: u32,
        face: &Face,
        options: RenderOptions,
    ) -> Result<ControlFlow<AtlasFormat, Glyph>> {
        // For any valid font, Harfbuzz will produce a index <= u16::MAX
        let glyph_idx = u16::try_from(gid)?;

        let font_face: &IDWriteFontFace = (&face.face).into();
        let glyph_run = DWRITE_GLYPH_RUN {
            // SAFETY: borrow the interface pointer without acquiring a COM reference.
            // `face` outlives this local run; ManuallyDrop prevents releasing its reference.
            // Do not clone this run or let it escape the lifetime of `face`.
            fontFace: ManuallyDrop::new(Some(unsafe { std::ptr::read(font_face) })),
            fontEmSize: face.size.pixels(),
            glyphCount: 1,
            glyphIndices: &raw const glyph_idx,
            glyphAdvances: std::ptr::null(),
            glyphOffsets: std::ptr::null(),
            isSideways: false.into(),
            bidiLevel: 0,
        };

        let (bounds, is_color_glyph) = self.calculate_bounds(&glyph_run)?;
        if bounds.left >= bounds.right || bounds.top >= bounds.bottom {
            return Ok(ControlFlow::Continue(Glyph::default()));
        }

        let bounds_left = bounds.left.round_ties_even() as i32;
        let bounds_top = bounds.top.round_ties_even() as i32;
        let bounds_right = bounds.right.round_ties_even() as i32;
        let bounds_bottom = bounds.bottom.round_ties_even() as i32;

        // Find the relative Y position of the baseline inside the cell.
        // `cell_baseline` is the distance from bottom to the baseline.
        let relative_baseline_y =
            options.metrics.cell_height as i32 - options.metrics.cell_baseline as i32;
        // D2D bounds are relative to baseline. It is added to get the
        // distance of the top of this glyph from the top of the cell.
        let offset_y = relative_baseline_y + bounds_top;

        let width = (bounds_right - bounds_left).max(0) as u16;
        let height = (bounds_bottom - bounds_top).max(0) as u16;

        self.prepare();
        let (atlas_format, atlas) = match is_color_glyph {
            true => (AtlasFormat::Bgra, &mut self.atlas_color),
            false => (AtlasFormat::Grayscale, &mut self.atlas_grayscale),
        };

        let region = match atlas.reserve(width, height) {
            Ok(region) => region,
            Err(AtlasFullError) => {
                self.draw_started = false;
                unsafe { self.ctx.EndDraw(None, None)? }

                if let AtlasStatus::Cleared(atlas) = atlas.grow(&self.gpu, &self.ctx)? {
                    self.current_target = None;
                    return Ok(ControlFlow::Break(atlas));
                }
                self.current_target = None;

                let region = atlas.reserve(width, height)?;

                unsafe { self.ctx.BeginDraw() };
                self.draw_started = true;

                region
            }
        };

        if self.current_target != Some(atlas_format) {
            unsafe { self.ctx.SetTarget(atlas.resources.bitmap()) };
            self.current_target = Some(atlas_format);
        }

        let origin = Vector2 {
            X: (region.x as i32 - bounds_left) as f32,
            Y: (region.y as i32 - bounds_top) as f32,
        };

        match is_color_glyph {
            false => unsafe {
                // Grayscale glyphs can be drawn directly
                self.ctx.DrawGlyphRun(
                    origin,
                    &raw const glyph_run,
                    None,
                    &self.brush,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
            },
            true => {
                let result = self.draw_color_glyph_run(origin, &glyph_run);
                unsafe { self.brush.SetColor(&WHITE) };
                result?
            }
        }

        let glyph = Glyph {
            height: region.height,
            width: region.width,
            offset_x: bounds_left,
            offset_y,
            atlas_x: region.x,
            atlas_y: region.y,
            atlas: atlas_format,
        };

        Ok(ControlFlow::Continue(glyph))
    }

    fn calculate_bounds(&self, glyph_run: &DWRITE_GLYPH_RUN) -> Result<(D2D_RECT_F, bool)> {
        const DEFAULT_BASELINE: Vector2 = Vector2 { X: 0.0, Y: 0.0 };
        const EMPTY_GLYPH_BOUNDS: D2D_RECT_F = D2D_RECT_F {
            left: f32::MAX,
            top: f32::MAX,
            right: f32::MIN,
            bottom: f32::MIN,
        };

        let mut bounds: D2D_RECT_F = EMPTY_GLYPH_BOUNDS;
        let Ok(enumerator) = self.create_color_enumerator(glyph_run, DEFAULT_BASELINE) else {
            // Return early since this is not a color glyph.
            bounds = unsafe {
                self.ctx.GetGlyphRunWorldBounds(
                    DEFAULT_BASELINE,
                    glyph_run,
                    DWRITE_MEASURING_MODE_NATURAL,
                )?
            };
            return Ok((bounds, false));
        };

        // This is a color glyph that may have multiple layers.
        while unsafe { enumerator.MoveNext()? }.as_bool() {
            let color_glyph_run_ptr = unsafe { enumerator.GetCurrentRun()? };
            if color_glyph_run_ptr.is_null() {
                continue;
            }
            let glyph_run = unsafe { &*color_glyph_run_ptr };

            let baseline = Vector2 {
                X: glyph_run.Base.baselineOriginX,
                Y: glyph_run.Base.baselineOriginY,
            };
            let color_glyph_bounds = unsafe {
                self.ctx.GetGlyphRunWorldBounds(
                    baseline,
                    &raw const glyph_run.Base.glyphRun,
                    DWRITE_MEASURING_MODE_NATURAL,
                )?
            };
            // Empty layers are ignored
            if color_glyph_bounds.top < color_glyph_bounds.bottom {
                bounds.left = bounds.left.min(color_glyph_bounds.left);
                bounds.top = bounds.top.min(color_glyph_bounds.top);
                bounds.right = bounds.right.max(color_glyph_bounds.right);
                bounds.bottom = bounds.bottom.max(color_glyph_bounds.bottom);
            }
        }
        Ok((bounds, true))
    }

    fn create_color_enumerator(
        &self,
        glyph_run: &DWRITE_GLYPH_RUN,
        baseline: Vector2,
    ) -> Result<IDWriteColorGlyphRunEnumerator1> {
        Ok(unsafe {
            self.dwrite_factory.TranslateColorGlyphRun(
                baseline,
                glyph_run,
                None,
                DWRITE_GLYPH_IMAGE_FORMATS_TRUETYPE
                    | DWRITE_GLYPH_IMAGE_FORMATS_CFF
                    | DWRITE_GLYPH_IMAGE_FORMATS_COLR
                    | DWRITE_GLYPH_IMAGE_FORMATS_SVG
                    | DWRITE_GLYPH_IMAGE_FORMATS_PNG
                    | DWRITE_GLYPH_IMAGE_FORMATS_JPEG
                    | DWRITE_GLYPH_IMAGE_FORMATS_TIFF
                    | DWRITE_GLYPH_IMAGE_FORMATS_PREMULTIPLIED_B8G8R8A8,
                DWRITE_MEASURING_MODE_NATURAL,
                None,
                0,
            )?
        })
    }

    fn draw_color_glyph_run(&self, origin: Vector2, glyph_run: &DWRITE_GLYPH_RUN) -> Result<()> {
        let enumerator = self.create_color_enumerator(glyph_run, origin)?;
        while unsafe { enumerator.MoveNext()? }.as_bool() {
            let color_glyph_run_ptr = unsafe { enumerator.GetCurrentRun()? };
            if color_glyph_run_ptr.is_null() {
                continue;
            }
            let color_glyph_run = unsafe { &*color_glyph_run_ptr };

            // 0xFFFF is DWRITE_NO_PALETTE_INDEX. If a specific
            // color is not provided, the default brush is used.
            if color_glyph_run.Base.paletteIndex != 0xFFFF {
                let color_ref = &color_glyph_run.Base.runColor;
                // Safety: DWRITE_COLOR_F and D2D1_COLOR_F have the same layout
                let color = color_ref as *const DWRITE_COLOR_F as *const D2D1_COLOR_F;
                unsafe { self.brush.SetColor(color) };
            } else {
                unsafe { self.brush.SetColor(&WHITE) };
            }

            let baseline = Vector2 {
                X: color_glyph_run.Base.baselineOriginX,
                Y: color_glyph_run.Base.baselineOriginY,
            };

            match color_glyph_run.glyphImageFormat {
                DWRITE_GLYPH_IMAGE_FORMATS_NONE => continue,
                DWRITE_GLYPH_IMAGE_FORMATS_PNG
                | DWRITE_GLYPH_IMAGE_FORMATS_JPEG
                | DWRITE_GLYPH_IMAGE_FORMATS_TIFF
                | DWRITE_GLYPH_IMAGE_FORMATS_PREMULTIPLIED_B8G8R8A8 => unsafe {
                    self.ctx.DrawColorBitmapGlyphRun(
                        color_glyph_run.glyphImageFormat,
                        baseline,
                        &color_glyph_run.Base.glyphRun,
                        color_glyph_run.measuringMode,
                        D2D1_COLOR_BITMAP_GLYPH_SNAP_OPTION_DEFAULT,
                    );
                },
                DWRITE_GLYPH_IMAGE_FORMATS_SVG => unsafe {
                    self.ctx.DrawSvgGlyphRun(
                        baseline,
                        &color_glyph_run.Base.glyphRun,
                        &self.brush,
                        None,
                        0,
                        color_glyph_run.measuringMode,
                    );
                },
                _ => unsafe {
                    self.ctx.DrawGlyphRun(
                        baseline,
                        &color_glyph_run.Base.glyphRun,
                        Some(color_glyph_run.Base.glyphRunDescription),
                        &self.brush,
                        color_glyph_run.measuringMode,
                    );
                },
            }
        }
        Ok(())
    }
}
