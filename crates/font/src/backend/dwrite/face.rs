use std::mem::ManuallyDrop;

use anyhow::Result;
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

use super::metrics::extract_metrics;
use crate::cache::glyph_cache::GlyphAtlasKind;
use crate::shared_grid::GridMetrics;

#[derive(Clone, Copy, Debug)]
pub struct DWriteGridMetricsConfig {
    pub font_size: f32,
    pub cell_width: f32,
    pub line_height: f32,
    pub baseline: f32,
}

#[derive(Clone, Copy)]
struct FaceDimensions {
    advance_width: f32,
    ascent: f32,
    line_gap: f32,
    face_height: f32,
}

pub fn measure_grid_metrics(
    face2: &IDWriteFontFace2,
    config: &DWriteGridMetricsConfig,
) -> Option<GridMetrics> {
    let face = face2.cast::<IDWriteFontFace>().ok()?;
    let measured = measure_face_dimensions(&face, config.font_size).ok()?;
    if measured.face_height <= 0.0 {
        return None;
    }

    let cell_width = if config.cell_width > 0.0 {
        config.cell_width
    } else {
        measured.advance_width.round().max(1.0)
    };
    let cell_height = if config.line_height > 0.0 {
        config.line_height.max(1.0)
    } else {
        measured.face_height.round().max(1.0)
    };
    let baseline = if config.baseline > 0.0 {
        config.baseline.clamp(0.0, cell_height)
    } else {
        (measured.ascent + (measured.line_gap + cell_height - measured.face_height) / 2.0)
            .round()
            .clamp(0.0, cell_height)
    };

    Some(GridMetrics {
        cell_width,
        cell_height,
        baseline,
    })
}

fn measure_face_dimensions(face: &IDWriteFontFace, font_size: f32) -> Result<FaceDimensions> {
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

pub(crate) struct RasterizedGlyph {
    pub atlas_kind: GlyphAtlasKind,
    pub width: u32,
    pub height: u32,
    pub offset_x: i32,
    pub offset_y: i32,
    pub pixels: Vec<u8>,
}

#[derive(Clone)]
pub struct DWriteGlyphRasterizer {
    factory: IDWriteFactory2,
    factory4: Option<IDWriteFactory4>,
    wic_factory: Option<IWICImagingFactory>,
}

impl DWriteGlyphRasterizer {
    pub fn new(factory: IDWriteFactory2) -> Self {
        let factory4 = factory.cast::<IDWriteFactory4>().ok();
        let wic_factory =
            unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }.ok();
        Self {
            factory,
            factory4,
            wic_factory,
        }
    }

    pub(crate) fn rasterize(
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

    fn rasterize_color(
        &self,
        face2: &IDWriteFontFace2,
        glyph_index: u16,
        font_size: f32,
    ) -> Result<Option<RasterizedGlyph>> {
        if let Some(bitmap) = self.rasterize_bitmap_color(face2, glyph_index, font_size)? {
            return Ok(Some(bitmap));
        }

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

// SAFETY: these factories are immutable COM interfaces used only through
// SharedGrid's serialized glyph-render path. The renderer never aliases the
// rasterizer directly, so cross-thread use is synchronized at the font-system
// boundary.
unsafe impl Send for DWriteGlyphRasterizer {}
unsafe impl Sync for DWriteGlyphRasterizer {}

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
            self.recreate();
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
        self.recreate();
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

    fn recreate(&mut self) {
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
