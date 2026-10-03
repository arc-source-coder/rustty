use windows::Win32::Globalization::GetUserDefaultLocaleName;
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_AXIS_TAG_ITALIC, DWRITE_FONT_AXIS_TAG_SLANT,
    DWRITE_FONT_AXIS_TAG_WEIGHT, DWRITE_FONT_AXIS_VALUE, DWRITE_FONT_PROPERTY,
    DWRITE_FONT_PROPERTY_ID, DWRITE_FONT_PROPERTY_ID_TYPOGRAPHIC_FACE_NAME,
    DWRITE_FONT_PROPERTY_ID_TYPOGRAPHIC_FAMILY_NAME,
    DWRITE_FONT_PROPERTY_ID_WEIGHT_STRETCH_STYLE_FACE_NAME,
    DWRITE_FONT_PROPERTY_ID_WEIGHT_STRETCH_STYLE_FAMILY_NAME,
    DWRITE_FONT_PROPERTY_ID_WIN32_FAMILY_NAME, DWRITE_READING_DIRECTION,
    DWRITE_READING_DIRECTION_LEFT_TO_RIGHT, DWriteCreateFactory, IDWriteFactory7,
    IDWriteFontFallback1, IDWriteFontSet2, IDWriteNumberSubstitution, IDWriteTextAnalysisSource,
    IDWriteTextAnalysisSource_Impl,
};
use windows::Win32::System::SystemServices::LOCALE_NAME_MAX_LENGTH;
use windows::core::{HSTRING, Interface as _, OutRef, PCWSTR, implement};

use crate::backend::dwrite::descriptor::Descriptor;
use crate::backend::dwrite::face::Face;
use crate::collection::FontEntry;
use crate::types::{FontDescriptor, FontError};

pub struct DirectWrite {
    fonts: IDWriteFontSet2,
    fallback: IDWriteFontFallback1,
    locale: [u16; LOCALE_NAME_MAX_LENGTH as usize],
}

impl DirectWrite {
    pub fn new() -> Result<Self, FontError> {
        let mut locale_utf16 = [0; LOCALE_NAME_MAX_LENGTH as usize];
        unsafe { GetUserDefaultLocaleName(&mut locale_utf16) };

        let factory: IDWriteFactory7 = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
        let fonts: IDWriteFontSet2 = unsafe { factory.GetSystemFontSet(false)? };
        let fallback: IDWriteFontFallback1 = unsafe { factory.GetSystemFontFallback()? }.cast()?;

        Ok(Self {
            fonts,
            fallback,
            locale: locale_utf16,
        })
    }

    pub fn discover(&self, font: &FontDescriptor) -> Result<Option<FontEntry>, FontError> {
        let descriptor = Descriptor::from(font);
        let family = HSTRING::from(font.family.as_ref());

        let property = |id, value: &HSTRING| DWRITE_FONT_PROPERTY {
            propertyId: id,
            propertyValue: PCWSTR(value.as_ptr()),
            localeName: PCWSTR::null(),
        };

        const FAMILY_PROPERTIES: [DWRITE_FONT_PROPERTY_ID; 3] = [
            DWRITE_FONT_PROPERTY_ID_TYPOGRAPHIC_FAMILY_NAME,
            DWRITE_FONT_PROPERTY_ID_WEIGHT_STRETCH_STYLE_FAMILY_NAME,
            DWRITE_FONT_PROPERTY_ID_WIN32_FAMILY_NAME,
        ];

        let properties = FAMILY_PROPERTIES.map(|id| property(id, &family));
        let mut fonts = unsafe { self.fonts.GetFilteredFonts3(Some(&properties), true)? };

        if let Some(style) = font.style.as_deref() {
            let style = HSTRING::from(style.trim());

            const FACE_PROPERTIES: [DWRITE_FONT_PROPERTY_ID; 2] = [
                DWRITE_FONT_PROPERTY_ID_TYPOGRAPHIC_FACE_NAME,
                DWRITE_FONT_PROPERTY_ID_WEIGHT_STRETCH_STYLE_FACE_NAME,
            ];
            let face_properties = FACE_PROPERTIES.map(|id| property(id, &style));

            fonts = unsafe { fonts.GetFilteredFonts3(Some(&face_properties), true)? };
        }

        if !descriptor.axes.is_empty() {
            fonts = unsafe { fonts.GetMatchingFonts(None, &descriptor.axes)? };
        }

        if unsafe { fonts.GetFontCount() } == 0 {
            return Ok(None);
        }

        Ok(Some(FontEntry {
            face: Face::new(unsafe { fonts.CreateFontFace(0)? }),
            fallback: false,
        }))
    }

    pub fn discover_fallback(&self, cp: u32) -> Result<Option<FontEntry>, FontError> {
        let codepoint = char::from_u32(cp).ok_or(FontError::InvalidCodepoint)?;

        let mut text: [u16; 2] = [0; 2];
        let text_len = codepoint.encode_utf16(&mut text).len() as u32;

        let analysis: IDWriteTextAnalysisSource = FallbackAnalysisSource {
            locale: self.locale,
            text,
            text_len,
        }
        .into();

        const DEFAULT_AXES: [DWRITE_FONT_AXIS_VALUE; 3] = [
            DWRITE_FONT_AXIS_VALUE {
                axisTag: DWRITE_FONT_AXIS_TAG_WEIGHT,
                value: 400.0,
            },
            DWRITE_FONT_AXIS_VALUE {
                axisTag: DWRITE_FONT_AXIS_TAG_ITALIC,
                value: 0.0,
            },
            DWRITE_FONT_AXIS_VALUE {
                axisTag: DWRITE_FONT_AXIS_TAG_SLANT,
                value: 0.0,
            },
        ];

        let mut mapped_face = None;
        let mut mapped_length: u32 = 0;

        // This is deliberately ignored.
        // Windows Terminal expects MapCharacters to always return 1.0.
        let mut dwrite_scale: f32 = 1.0;

        unsafe {
            self.fallback.MapCharacters(
                &analysis,
                0,
                text_len,
                None,
                PCWSTR::null(),
                &DEFAULT_AXES,
                &raw mut mapped_length,
                &raw mut dwrite_scale,
                &raw mut mapped_face,
            )?;
        };

        // Match the Windows Terminal assertion
        debug_assert_eq!(dwrite_scale, 1.0);

        Ok(mapped_face.map(|face| FontEntry {
            face: Face::new(face),
            fallback: true,
        }))
    }
}

#[implement(IDWriteTextAnalysisSource)]
struct FallbackAnalysisSource {
    locale: [u16; LOCALE_NAME_MAX_LENGTH as usize],
    text: [u16; 2],
    text_len: u32,
}

#[allow(non_snake_case)]
impl IDWriteTextAnalysisSource_Impl for FallbackAnalysisSource_Impl {
    fn GetTextAtPosition(
        &self,
        position: u32,
        text: *mut *mut u16,
        length: *mut u32,
    ) -> windows::core::Result<()> {
        unsafe {
            if position >= self.text_len {
                *text = std::ptr::null_mut();
                *length = 0;
            } else {
                *text = self.text.as_ptr().add(position as usize).cast_mut();
                *length = self.text_len - position;
            }
        }

        Ok(())
    }

    fn GetTextBeforePosition(
        &self,
        position: u32,
        text: *mut *mut u16,
        length: *mut u32,
    ) -> windows::core::Result<()> {
        unsafe {
            if position == 0 || position > self.text_len {
                *text = std::ptr::null_mut();
                *length = 0;
            } else {
                *text = self.text.as_ptr().cast_mut();
                *length = position;
            }
        }
        Ok(())
    }

    fn GetParagraphReadingDirection(&self) -> DWRITE_READING_DIRECTION {
        DWRITE_READING_DIRECTION_LEFT_TO_RIGHT
    }

    fn GetLocaleName(
        &self,
        position: u32,
        length: *mut u32,
        locale: *mut *mut u16,
    ) -> windows::core::Result<()> {
        unsafe { *length = self.text_len - position.min(self.text_len) };
        unsafe { *locale = self.locale.as_ptr().cast_mut() };
        Ok(())
    }

    fn GetNumberSubstitution(
        &self,
        _position: u32,
        length: *mut u32,
        substitution: OutRef<IDWriteNumberSubstitution>,
    ) -> windows::core::Result<()> {
        unsafe { *length = 0 };
        let _ = substitution.write(None);
        Ok(())
    }
}
