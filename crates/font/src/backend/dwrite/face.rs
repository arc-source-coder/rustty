use std::num::NonZeroU32;

use anyhow::Result;
use harfbuzz::HbFont;

use crate::metrics::FaceMetrics;
use crate::types::{FontError, FontSize};

use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_METRICS1, DWRITE_GLYPH_IMAGE_FORMATS, DWRITE_GLYPH_IMAGE_FORMATS_COLR,
    DWRITE_GLYPH_IMAGE_FORMATS_JPEG, DWRITE_GLYPH_IMAGE_FORMATS_PNG,
    DWRITE_GLYPH_IMAGE_FORMATS_PREMULTIPLIED_B8G8R8A8, DWRITE_GLYPH_IMAGE_FORMATS_SVG,
    DWRITE_GLYPH_IMAGE_FORMATS_TIFF, DWRITE_GLYPH_METRICS, IDWriteFontFace5,
};

/// A DirectWrite face with a lazily initialized, size-specific HarfBuzz font.
pub struct Face {
    /// Point size and DPI used to initialize this face's HarfBuzz font.
    /// Default-valued until `load` succeeds.
    pub size: FontSize,
    /// DirectWrite Face for this font
    ///
    /// We use a IDWriteFontFace5 here because it costs ~10KB more than
    /// a "deferred" IDWriteFont3 but provides much more accurate data.
    pub face: IDWriteFontFace5,
    /// HarfBuzz font derived from `self.face`; `None` until `load` succeeds.
    pub hb_font: Option<HbFont>,
}

impl Face {
    pub fn new(face: IDWriteFontFace5) -> Self {
        Self { size: FontSize::default(), face, hb_font: None }
    }

    /// Initialize the HarfBuzz font and size once. Later calls leave both
    /// unchanged; a size change requires a face from a different font grid.
    pub fn load(&mut self, size: FontSize) -> Result<(), FontError> {
        if self.hb_font.is_none() {
            std::hint::cold_path();

            let mut hb_font = HbFont::from(&self.face)?;

            // Convert scale to 26.6 FP format to pass to Harfbuzz
            let scale = (size.pixels() * 64.0).round() as i32;
            hb_font.set_scale(scale, scale);

            self.size = size;
            self.hb_font = Some(hb_font);
        }
        Ok(())
    }

    /// Get the glyph index for the given Unicode code point.
    pub fn glyph_index(&self, codepoint: u32) -> Option<NonZeroU32> {
        let mut gid: [u16; 1] = [0];
        let cps = [codepoint];

        unsafe {
            let (ptr, len) = (cps.as_ptr(), cps.len() as u32);
            self.face.GetGlyphIndices(ptr, len, gid.as_mut_ptr()).ok()?;
        }

        NonZeroU32::new(u32::from(gid[0]))
    }

    /// Returns true if the given glyph ID is colorized.
    pub fn is_color_glyph(&self, gid: NonZeroU32) -> bool {
        // Zed has a comment saying this does not work for ❤
        // TODO: Test with ❤
        let image_formats = unsafe { self.face.GetGlyphImageFormats(gid.get() as u16, 1, 4096) };
        let color_formats: DWRITE_GLYPH_IMAGE_FORMATS = DWRITE_GLYPH_IMAGE_FORMATS_PNG
            | DWRITE_GLYPH_IMAGE_FORMATS_JPEG
            | DWRITE_GLYPH_IMAGE_FORMATS_TIFF
            | DWRITE_GLYPH_IMAGE_FORMATS_COLR
            | DWRITE_GLYPH_IMAGE_FORMATS_PREMULTIPLIED_B8G8R8A8
            | DWRITE_GLYPH_IMAGE_FORMATS_SVG;

        (image_formats.unwrap_or_default() & color_formats).0 != 0
    }

    pub fn get_metrics(&self) -> Result<FaceMetrics, FontError> {
        if self.size == FontSize::default() {
            return Err(FontError::FaceNotInitialized);
        }

        let mut metrics = DWRITE_FONT_METRICS1::default();
        // SAFETY: `metrics` is a valid out-parameter for this COM call.
        unsafe { self.face.GetMetrics(&raw mut metrics) };

        if metrics.Base.designUnitsPerEm == 0 {
            return Err(FontError::InvalidMetrics);
        }

        let px_per_em = f64::from(self.size.pixels());
        let px_per_unit = px_per_em / f64::from(metrics.Base.designUnitsPerEm);

        let ascent = f64::from(metrics.Base.ascent) * px_per_unit;
        // DirectWrite's descent means positive = down
        // Normalize to positive = up
        let descent = -(f64::from(metrics.Base.descent) * px_per_unit);
        let line_gap = f64::from(metrics.Base.lineGap) * px_per_unit;

        let underline_position = f64::from(metrics.Base.underlinePosition) * px_per_unit;
        let underline_thickness = f64::from(metrics.Base.underlineThickness) * px_per_unit;

        let strikethrough_position = f64::from(metrics.Base.strikethroughPosition) * px_per_unit;
        let strikethrough_thickness = f64::from(metrics.Base.strikethroughThickness) * px_per_unit;

        let cap_height = f64::from(metrics.Base.capHeight) * px_per_unit;

        const ASCII_START: u32 = 0x20;
        const ASCII_END: u32 = 0x7e;
        const ASCII_COUNT: usize = (ASCII_END - ASCII_START + 1) as usize;

        let codepoints: [u32; ASCII_COUNT] = std::array::from_fn(|i| ASCII_START + i as u32);
        let mut glyph_indices: [u16; ASCII_COUNT] = [0; ASCII_COUNT];

        unsafe {
            self.face.GetGlyphIndices(
                codepoints.as_ptr(),
                ASCII_COUNT as u32,
                glyph_indices.as_mut_ptr(),
            )?;
        };

        let mut glyph_metrics = [DWRITE_GLYPH_METRICS::default(); ASCII_COUNT];
        unsafe {
            self.face.GetDesignGlyphMetrics(
                glyph_indices.as_ptr(),
                ASCII_COUNT as u32,
                glyph_metrics.as_mut_ptr(),
                false,
            )?;
        }

        let mut max_advance: f64 = 0.0;

        for (&glyph, metric) in glyph_indices.iter().zip(&glyph_metrics) {
            if glyph == 0 {
                continue;
            }
            let advance = f64::from(metric.advanceWidth) * px_per_unit;
            max_advance = f64::max(max_advance, advance);
        }

        if max_advance <= 0.0 {
            return Err(FontError::InvalidMetrics);
        }

        Ok(FaceMetrics {
            cell_width: max_advance,

            ascent,
            descent,
            line_gap,

            underline_position,
            underline_thickness,

            strikethrough_position,
            strikethrough_thickness,

            cap_height,
        })
    }
}
