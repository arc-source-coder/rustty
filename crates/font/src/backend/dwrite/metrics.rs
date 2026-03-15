use anyhow::{Result, anyhow};

#[cfg(target_os = "windows")]
use windows::Win32::Graphics::DirectWrite::{DWRITE_FONT_METRICS, IDWriteFontFace};

#[derive(Clone, Copy, Debug, Default)]
pub struct FontMetrics {
    /// Distance above baseline in DIPs for the current font size.
    pub ascent: f32,
    /// Distance below baseline in DIPs for the current font size.
    pub descent: f32,
    /// Additional line spacing in DIPs for the current font size.
    pub line_gap: f32,
}

#[cfg(target_os = "windows")]
pub fn extract_metrics(face: &IDWriteFontFace, font_size: f32) -> Result<FontMetrics> {
    let mut raw = DWRITE_FONT_METRICS::default();
    // SAFETY: `raw` is a valid out-parameter for this COM call.
    unsafe {
        face.GetMetrics(&mut raw);
    }
    if raw.designUnitsPerEm == 0 {
        return Err(anyhow!("invalid font metrics: designUnitsPerEm == 0"));
    }

    let scale = font_size / raw.designUnitsPerEm as f32;
    Ok(FontMetrics {
        ascent: raw.ascent as f32 * scale,
        descent: raw.descent as f32 * scale,
        line_gap: raw.lineGap as f32 * scale,
    })
}
