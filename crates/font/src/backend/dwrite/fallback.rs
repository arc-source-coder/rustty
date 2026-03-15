use anyhow::{Result, anyhow};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_NORMAL,
    IDWriteFontCollection, IDWriteFontFace2, IDWriteFontFallback, IDWriteTextAnalysisSource,
};
use windows::core::{HSTRING, Interface, PCWSTR};

#[derive(Clone)]
pub struct FontFallbackContext {
    /// Base family name used as fallback root.
    pub base_family: HSTRING,
    /// Font collection used for fallback mapping.
    pub base_collection: IDWriteFontCollection,
    /// Legacy fallback interface.
    pub fallback: IDWriteFontFallback,
}

#[derive(Clone)]
pub struct MappedFontRun {
    /// UTF-16 span length mapped to a single fallback face.
    pub text_length: u32,
    /// Resolved fallback face for the mapped span.
    pub font_face: Option<IDWriteFontFace2>,
}

impl FontFallbackContext {
    pub fn map_characters(
        &self,
        source: &IDWriteTextAnalysisSource,
        text_position: u32,
        text_length: u32,
    ) -> Result<MappedFontRun> {
        let mut mapped_length = 0u32;
        let mut scale = 1.0f32;

        let mut mapped_font = None;
        unsafe {
            self.fallback.MapCharacters(
                source,
                text_position,
                text_length,
                &self.base_collection,
                PCWSTR(self.base_family.as_ptr()),
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                &mut mapped_length,
                &mut mapped_font,
                &mut scale,
            )?;
        }

        let mapped_face2 = if let Some(font) = mapped_font {
            let face = unsafe { font.CreateFontFace() }?;
            Some(face.cast::<IDWriteFontFace2>()?)
        } else {
            None
        };

        if mapped_length == 0 {
            return Err(anyhow!("MapCharacters returned zero-length mapping"));
        }

        // WT reference: `AtlasEngine::_mapCharacters` currently ignores `scale`
        // and asserts it is always 1 for tested fonts.
        debug_assert!((scale - 1.0).abs() <= f32::EPSILON);

        Ok(MappedFontRun {
            text_length: mapped_length,
            font_face: mapped_face2,
        })
    }
}
