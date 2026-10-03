use anyhow::anyhow;

use crate::metrics::FontMetrics;
use crate::types::{FontError, FontIndex, FontSize, FontStyle, Presentation, PresentationMode};

use utils::asserts::unreachable;

#[cfg(target_os = "windows")]
use crate::backend::dwrite::face::Face;

pub struct FontEntry {
    pub face: Face,
    pub fallback: bool,
}

impl FontEntry {
    pub fn has_codepoint(&self, cp: u32, p_mode: PresentationMode) -> bool {
        let presentation_mode = match p_mode {
            // Fallback should need explicit presentation matching.
            PresentationMode::Default(p) if self.fallback => PresentationMode::Explicit(p),
            PresentationMode::Explicit(p) => PresentationMode::Explicit(p),
            _ => PresentationMode::Any,
        };

        match presentation_mode {
            PresentationMode::Explicit(p) => {
                let Some(glyph_index) = self.face.glyph_index(cp) else {
                    return false;
                };
                let is_color = self.face.is_color_glyph(glyph_index);
                match p {
                    Presentation::Emoji => is_color,
                    Presentation::Text => !is_color,
                }
            }
            PresentationMode::Any => self.face.glyph_index(cp).is_some(),
            // Safety: All modes were collapsed above in the `let presentation_mode =` block.
            PresentationMode::Default(_) => unreachable(),
        }
    }
}

// TODO: Doc comments
/// Ghostty reference: `font/Collection.zig`.
pub struct Collection {
    faces: [Vec<FontEntry>; 4],
    pub size: FontSize,
}

impl Collection {
    pub fn new(size: FontSize) -> Self {
        Self {
            faces: std::array::from_fn(|_| Vec::new()),
            size,
        }
    }

    #[inline]
    pub fn add(&mut self, entry: FontEntry, style: FontStyle) -> anyhow::Result<FontIndex> {
        let style_idx = style as usize;
        let idx = self.faces[style_idx].len();

        if idx > FontIndex::MAX_FACES_PER_STYLE as usize {
            return Err(anyhow!("font collection exhausted style bucket: {style:?}"));
        }

        self.faces[style_idx].push(entry);
        Ok(FontIndex::new(style, idx as u16))
    }

    #[inline]
    fn entry(&self, index: FontIndex) -> Option<&FontEntry> {
        self.faces[index.style() as usize].get(index.index() as usize)
    }

    #[inline]
    pub fn get_index(
        &self,
        cp: u32,
        style: FontStyle,
        p_mode: PresentationMode,
    ) -> Option<FontIndex> {
        let style_idx = style as usize;

        for idx in 0..self.faces[style_idx].len() {
            let index = FontIndex::new(style, idx as u16);
            if self.has_codepoint(index, cp, p_mode) {
                return Some(index);
            }
        }
        None
    }

    #[inline]
    pub fn has_codepoint(&self, idx: FontIndex, cp: u32, p_mode: PresentationMode) -> bool {
        self.entry(idx)
            .is_some_and(|entry| entry.has_codepoint(cp, p_mode))
    }

    #[inline]
    pub fn ensure_loaded(&mut self, idx: FontIndex) -> Result<(), FontError> {
        let Some(entry) = self.faces[idx.style() as usize].get_mut(idx.index() as usize) else {
            return Err(FontError::InvalidIndex);
        };
        // Unlike Ghostty, we deliberately do not do any metric-based fallback size
        // adjustment. This matches Windows Terminal and avoids resizing DirectWrite
        // fallback fonts, which can worsen their appearance.
        entry.face.load(self.size)
    }

    #[inline]
    pub fn get_face(&self, idx: FontIndex) -> Result<&Face, FontError> {
        self.entry(idx)
            .map(|entry| &entry.face)
            .ok_or(FontError::InvalidIndex)
    }

    /// Load the primary regular face and calculate grid metrics from it.
    pub fn update_metrics(&mut self) -> Result<FontMetrics, FontError> {
        self.ensure_loaded(FontIndex::DEFAULT)?;

        let face_metrics = self.get_face(FontIndex::DEFAULT)?.get_metrics()?;
        let font_metrics = FontMetrics::calculate(&face_metrics);

        Ok(font_metrics)
    }
}
