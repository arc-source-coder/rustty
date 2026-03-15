use crate::collection::Collection;
use crate::types::FontIndex;
use crate::types::Presentation;
use crate::types::Style;
#[cfg(target_os = "windows")]
use anyhow::Result;
use unicode_properties::{EmojiStatus, UnicodeEmoji};

#[cfg(target_os = "windows")]
use crate::backend::dwrite::fallback::FontFallbackContext;
#[cfg(target_os = "windows")]
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_READING_DIRECTION, DWRITE_READING_DIRECTION_LEFT_TO_RIGHT, IDWriteFontFace2,
    IDWriteNumberSubstitution, IDWriteTextAnalysisSource, IDWriteTextAnalysisSource_Impl,
};
#[cfg(target_os = "windows")]
use windows::core::implement;
#[cfg(target_os = "windows")]
use windows_core::OutRef;

#[derive(Clone, Copy, Debug)]
pub struct StyleStatus(u8);

impl StyleStatus {
    pub fn init_fill(enabled: bool) -> Self {
        if enabled { Self(0b1111) } else { Self(0) }
    }

    #[inline]
    pub fn get(self, style: Style) -> bool {
        (self.0 & (1 << (style as u8))) != 0
    }

    #[inline]
    pub fn set(&mut self, style: Style, enabled: bool) {
        let bit = 1 << (style as u8);
        if enabled {
            self.0 |= bit;
        } else {
            self.0 &= !bit;
        }
    }
}

pub struct CodepointResolver {
    pub collection: Collection,
    pub styles: StyleStatus,
    #[cfg(target_os = "windows")]
    dwrite_fallback: Option<DWriteFallbackResolver>,
}

impl CodepointResolver {
    pub fn new(collection: Collection) -> Self {
        Self {
            collection,
            styles: StyleStatus::init_fill(true),
            #[cfg(target_os = "windows")]
            dwrite_fallback: None,
        }
    }

    #[cfg(target_os = "windows")]
    pub fn set_dwrite_fallback(&mut self, fallback: FontFallbackContext, locale: &str) {
        self.dwrite_fallback = Some(DWriteFallbackResolver::new(fallback, locale));
    }

    #[cfg(target_os = "windows")]
    pub fn add_dwrite_face(&mut self, style: Style, face: &IDWriteFontFace2) -> Result<FontIndex> {
        self.collection.get_or_insert_dwrite_face(style, face)
    }

    pub fn get_index(
        &mut self,
        codepoint: u32,
        style: Style,
        presentation: Option<Presentation>,
    ) -> Option<FontIndex> {
        let style = if self.styles.get(style) {
            style
        } else {
            Style::Normal
        };
        let preferred_presentation = presentation.unwrap_or_else(|| {
            if codepoint_default_emoji_presentation(codepoint) {
                Presentation::Emoji
            } else {
                Presentation::Text
            }
        });

        if let Some(idx) = self
            .collection
            .get_index(codepoint, style, Some(preferred_presentation))
        {
            return Some(idx);
        }

        if style != Style::Normal {
            if let Some(idx) =
                self.collection
                    .get_index(codepoint, Style::Normal, Some(preferred_presentation))
            {
                return Some(idx);
            }
        }

        // Ghostty compatibility: after preferred/default presentation probing,
        // allow any presentation so text-only emoji-capable fonts can still
        // satisfy fallback codepoint resolution.
        if let Some(idx) = self.collection.get_index(codepoint, style, None) {
            return Some(idx);
        }

        if style != Style::Normal {
            if let Some(idx) = self.collection.get_index(codepoint, Style::Normal, None) {
                return Some(idx);
            }
        }

        #[cfg(target_os = "windows")]
        {
            // Ghostty compatibility: only perform fallback discovery from regular
            // style resolution to avoid pulling in styled fallback faces.
            if style == Style::Normal
                && let Some(fallback) = self.dwrite_fallback.as_mut()
                && let Some(face) = fallback.resolve_face(codepoint)
                && let Ok(idx) = self.collection.get_or_insert_dwrite_face(style, &face)
            {
                if self
                    .collection
                    .has_codepoint(idx, codepoint, Some(preferred_presentation))
                    || self.collection.has_codepoint(idx, codepoint, None)
                {
                    return Some(idx);
                }
            }
        }

        None
    }

    pub fn has_codepoint(
        &self,
        index: FontIndex,
        codepoint: u32,
        presentation: Option<Presentation>,
    ) -> bool {
        self.collection
            .has_codepoint(index, codepoint, presentation)
    }

    pub fn set_style(&mut self, style: Style, enabled: bool) {
        self.styles.set(style, enabled);
    }
}

fn codepoint_default_emoji_presentation(codepoint: u32) -> bool {
    let Some(ch) = char::from_u32(codepoint) else {
        return false;
    };
    // Ghostty reference: `CodepointResolver.getIndex` uses UCD
    // `is_emoji_presentation` when no explicit VS15/VS16 is present.
    matches!(
        ch.emoji_status(),
        EmojiStatus::EmojiPresentation
            | EmojiStatus::EmojiPresentationAndModifierBase
            | EmojiStatus::EmojiPresentationAndEmojiComponent
            | EmojiStatus::EmojiPresentationAndModifierAndEmojiComponent
    )
}

#[cfg(target_os = "windows")]
struct DWriteFallbackResolver {
    fallback: FontFallbackContext,
    locale_utf16: Vec<u16>,
}

#[cfg(target_os = "windows")]
impl DWriteFallbackResolver {
    fn new(fallback: FontFallbackContext, locale: &str) -> Self {
        let mut locale_utf16 = locale.encode_utf16().collect::<Vec<u16>>();
        locale_utf16.push(0);
        Self {
            fallback,
            locale_utf16,
        }
    }

    fn resolve_face(&mut self, codepoint: u32) -> Option<IDWriteFontFace2> {
        let mut text = [0u16; 2];
        let text_len = if let Some(ch) = char::from_u32(codepoint) {
            ch.encode_utf16(&mut text).len()
        } else {
            return None;
        };

        let source_impl = SingleTextAnalysisSource::new(&self.locale_utf16, &text[..text_len]);
        let source_iface: IDWriteTextAnalysisSource = source_impl.into();
        let mapped = self
            .fallback
            .map_characters(&source_iface, 0, text_len as u32)
            .ok()?;
        mapped.font_face
    }
}

#[cfg(target_os = "windows")]
#[implement(IDWriteTextAnalysisSource)]
struct SingleTextAnalysisSource {
    text_ptr: *const u16,
    text_len: u32,
    locale_ptr: *const u16,
}

#[cfg(target_os = "windows")]
impl SingleTextAnalysisSource {
    fn new(locale: &[u16], text: &[u16]) -> Self {
        Self {
            text_ptr: text.as_ptr(),
            text_len: text.len() as u32,
            locale_ptr: locale.as_ptr(),
        }
    }
}

#[cfg(target_os = "windows")]
#[allow(non_snake_case)]
impl IDWriteTextAnalysisSource_Impl for SingleTextAnalysisSource_Impl {
    fn GetTextAtPosition(
        &self,
        textposition: u32,
        textstring: *mut *mut u16,
        textlength: *mut u32,
    ) -> windows::core::Result<()> {
        let pos = textposition.min(self.text_len) as usize;
        unsafe {
            *textstring = self.text_ptr.add(pos) as *mut u16;
            *textlength = self.text_len - pos as u32;
        }
        Ok(())
    }

    fn GetTextBeforePosition(
        &self,
        textposition: u32,
        textstring: *mut *mut u16,
        textlength: *mut u32,
    ) -> windows::core::Result<()> {
        let pos = textposition.min(self.text_len);
        unsafe {
            *textstring = self.text_ptr as *mut u16;
            *textlength = pos;
        }
        Ok(())
    }

    fn GetParagraphReadingDirection(&self) -> DWRITE_READING_DIRECTION {
        DWRITE_READING_DIRECTION_LEFT_TO_RIGHT
    }

    fn GetLocaleName(
        &self,
        textposition: u32,
        textlength: *mut u32,
        localename: *mut *mut u16,
    ) -> windows::core::Result<()> {
        unsafe {
            *textlength = self.text_len - textposition.min(self.text_len);
            *localename = self.locale_ptr as *mut u16;
        }
        Ok(())
    }

    fn GetNumberSubstitution(
        &self,
        _textposition: u32,
        textlength: *mut u32,
        numbersubstitution: OutRef<IDWriteNumberSubstitution>,
    ) -> windows::core::Result<()> {
        unsafe {
            *textlength = 0;
        }
        let _ = numbersubstitution.write(None);
        Ok(())
    }
}
