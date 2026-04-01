use anyhow::{Result, anyhow};
#[cfg(target_os = "windows")]
use rapidhash::{HashMapExt, RapidHashMap};

#[cfg(target_os = "windows")]
use std::mem::ManuallyDrop;
#[cfg(target_os = "windows")]
use std::sync::RwLock;
#[cfg(target_os = "windows")]
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_GLYPH_IMAGE_FORMATS, DWRITE_GLYPH_IMAGE_FORMATS_COLR,
    DWRITE_GLYPH_IMAGE_FORMATS_JPEG, DWRITE_GLYPH_IMAGE_FORMATS_PNG,
    DWRITE_GLYPH_IMAGE_FORMATS_PREMULTIPLIED_B8G8R8A8, DWRITE_GLYPH_IMAGE_FORMATS_SVG,
    DWRITE_GLYPH_IMAGE_FORMATS_TIFF, DWRITE_GLYPH_OFFSET, DWRITE_GLYPH_RUN,
    DWRITE_MEASURING_MODE_NATURAL, DWriteCreateFactory, IDWriteFactory2, IDWriteFactory4,
    IDWriteFontFace, IDWriteFontFace2, IDWriteFontFace4,
};
#[cfg(target_os = "windows")]
use windows_core::Interface;
#[cfg(target_os = "windows")]
use windows_numerics::Vector2;

use crate::types::Presentation;
use crate::types::{FontIndex, Style};

pub struct Collection {
    /// Ghostty equivalent field name: `faces`.
    /// Reference: `font/Collection.zig`.
    faces: [Vec<FaceEntry>; 4],
    /// Transitional DWrite-only pointer->index dedupe map.
    /// Ghostty does not need this exact map because it adds faces through
    /// collection-building flow, not per-shape `MapCharacters` callbacks.
    ///
    /// TODO(ghostty-parity): remove this map after we complete the Ghostty-style
    /// collection lifecycle migration:
    /// 1) fallback/discovery adds faces via resolver lifecycle (not shaping flow),
    /// 2) collection storage moves to pointer-stable Ghostty-like entries
    ///    (EnumArray + SegmentedList + EntryOrAlias semantics),
    /// 3) shaping path consumes pre-owned `FontIndex` only.
    dwrite_face_index_map: [Vec<(usize, u16)>; 4],
    #[cfg(target_os = "windows")]
    dwrite_factory4: Option<IDWriteFactory4>,
}

struct FaceEntry {
    #[cfg(target_os = "windows")]
    face2: IDWriteFontFace2,
    #[cfg(target_os = "windows")]
    face: IDWriteFontFace,
    #[cfg(target_os = "windows")]
    face4: Option<IDWriteFontFace4>,
    #[cfg(target_os = "windows")]
    color_glyph_cache: RwLock<RapidHashMap<u16, bool>>,
}

impl Collection {
    pub fn new() -> Self {
        #[cfg(target_os = "windows")]
        let dwrite_factory4 = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) }
            .ok()
            .and_then(|factory: IDWriteFactory2| factory.cast::<IDWriteFactory4>().ok());

        Self {
            faces: std::array::from_fn(|_| Vec::new()),
            dwrite_face_index_map: std::array::from_fn(|_| Vec::new()),
            #[cfg(target_os = "windows")]
            dwrite_factory4,
        }
    }

    #[cfg(target_os = "windows")]
    pub fn get_or_insert_dwrite_face(
        &mut self,
        style: Style,
        face: &IDWriteFontFace2,
    ) -> Result<FontIndex> {
        let style_idx = style as usize;
        let raw = face.as_raw().addr();
        for &(ptr, idx) in &self.dwrite_face_index_map[style_idx] {
            if ptr == raw {
                return Ok(FontIndex::new(style, idx));
            }
        }

        let idx = self.faces[style_idx].len();
        if idx > FontIndex::MAX_FACES_PER_STYLE as usize {
            return Err(anyhow!("font collection exhausted style bucket: {style:?}"));
        }
        let idx_u16 = idx as u16;
        let face_base = face.cast::<IDWriteFontFace>()?;
        self.faces[style_idx].push(FaceEntry {
            face2: face.clone(),
            face: face_base,
            face4: face.cast::<IDWriteFontFace4>().ok(),
            color_glyph_cache: RwLock::new(RapidHashMap::with_capacity(64)),
        });
        self.dwrite_face_index_map[style_idx].push((raw, idx_u16));
        Ok(FontIndex::new(style, idx_u16))
    }

    pub fn get_index(
        &self,
        codepoint: u32,
        style: Style,
        presentation: Option<Presentation>,
    ) -> Option<FontIndex> {
        let style_idx = style as usize;
        for idx in 0..self.faces[style_idx].len() {
            let index = FontIndex::new(style, idx as u16);
            if self.has_codepoint(index, codepoint, presentation) {
                return Some(index);
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
        let style_idx = index.style() as usize;
        let entry = match self.faces[style_idx].get(index.index() as usize) {
            Some(entry) => entry,
            None => return false,
        };
        entry.query_has_codepoint(codepoint, presentation, self.dwrite_factory4.as_ref())
    }

    #[cfg(target_os = "windows")]
    pub(crate) fn face_for_index(&self, index: FontIndex) -> Option<IDWriteFontFace2> {
        Some(
            self.faces[index.style() as usize]
                .get(index.index() as usize)?
                .face2
                .clone(),
        )
    }
}

impl Default for Collection {
    fn default() -> Self {
        Self::new()
    }
}

impl FaceEntry {
    fn query_has_codepoint(
        &self,
        codepoint: u32,
        presentation: Option<Presentation>,
        factory4: Option<&IDWriteFactory4>,
    ) -> bool {
        #[cfg(target_os = "windows")]
        {
            let mut glyph = [0u16; 1];
            let cps = [codepoint];
            // SAFETY: one-element arrays are valid for DWrite call.
            let ok = unsafe {
                self.face2
                    .GetGlyphIndices(cps.as_ptr(), cps.len() as u32, glyph.as_mut_ptr())
                    .is_ok()
            };
            if !ok || glyph[0] == 0 {
                return false;
            }
            let Some(presentation) = presentation else {
                return true;
            };

            let is_color = glyph_is_color(self, glyph[0], factory4);
            return match presentation {
                Presentation::Text => !is_color,
                Presentation::Emoji => is_color,
            };
        }

        #[allow(unreachable_code)]
        false
    }
}

#[cfg(target_os = "windows")]
fn glyph_is_color(entry: &FaceEntry, glyph_id: u16, factory4: Option<&IDWriteFactory4>) -> bool {
    if let Some(&cached) = entry
        .color_glyph_cache
        .read()
        .expect("color glyph cache poisoned")
        .get(&glyph_id)
    {
        return cached;
    }

    let image_formats = entry
        .face4
        .as_ref()
        .and_then(|face4| unsafe { face4.GetGlyphImageFormats(glyph_id, 1, 4096) }.ok())
        .unwrap_or(DWRITE_GLYPH_IMAGE_FORMATS(0));
    let color_formats = DWRITE_GLYPH_IMAGE_FORMATS_PNG
        | DWRITE_GLYPH_IMAGE_FORMATS_JPEG
        | DWRITE_GLYPH_IMAGE_FORMATS_TIFF
        | DWRITE_GLYPH_IMAGE_FORMATS_COLR
        | DWRITE_GLYPH_IMAGE_FORMATS_PREMULTIPLIED_B8G8R8A8
        | DWRITE_GLYPH_IMAGE_FORMATS_SVG;
    let is_color = if (image_formats & color_formats).0 != 0 {
        true
    } else if let Some(factory4) = factory4 {
        glyph_has_color_run(factory4, &entry.face, glyph_id)
    } else {
        // Coarse fallback if per-glyph image formats are unavailable on the platform.
        unsafe { entry.face2.IsColorFont() }.as_bool()
    };

    entry
        .color_glyph_cache
        .write()
        .expect("color glyph cache poisoned")
        .insert(glyph_id, is_color);
    is_color
}

#[cfg(target_os = "windows")]
fn glyph_has_color_run(factory4: &IDWriteFactory4, face: &IDWriteFontFace, glyph_id: u16) -> bool {
    let glyph_indices = [glyph_id];
    let advances = [0.0f32];
    let offsets = [DWRITE_GLYPH_OFFSET::default()];
    let glyph_run = DWRITE_GLYPH_RUN {
        fontFace: ManuallyDrop::new(Some(face.clone())),
        fontEmSize: 14.0,
        glyphCount: 1,
        glyphIndices: glyph_indices.as_ptr(),
        glyphAdvances: advances.as_ptr(),
        glyphOffsets: offsets.as_ptr(),
        isSideways: false.into(),
        bidiLevel: 0,
    };

    unsafe {
        factory4.TranslateColorGlyphRun(
            Vector2 { X: 0.0, Y: 0.0 },
            &glyph_run,
            None,
            DWRITE_GLYPH_IMAGE_FORMATS_COLR
                | DWRITE_GLYPH_IMAGE_FORMATS_SVG
                | DWRITE_GLYPH_IMAGE_FORMATS_PNG
                | DWRITE_GLYPH_IMAGE_FORMATS_JPEG
                | DWRITE_GLYPH_IMAGE_FORMATS_TIFF
                | DWRITE_GLYPH_IMAGE_FORMATS_PREMULTIPLIED_B8G8R8A8,
            DWRITE_MEASURING_MODE_NATURAL,
            None,
            0,
        )
    }
    .is_ok()
}
