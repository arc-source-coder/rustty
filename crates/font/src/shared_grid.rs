use anyhow::Result;
use parking_lot::RwLock;
use rustc_hash::FxHashMap;
use std::collections::hash_map::Entry;

use crate::backend::dwrite::face::Face;
use crate::metrics::FontMetrics;
use crate::resolver::CodepointResolver;
use crate::types::{FontError, FontIndex, FontStyle, Presentation, PresentationMode};

/// Shared font runtime — Ghostty's `SharedGrid`.
pub struct SharedGrid {
    pub metrics: FontMetrics,
    inner: RwLock<SharedGridInner>,
}

struct SharedGridInner {
    resolver: CodepointResolver,
    codepoints: FxHashMap<CodepointKey, Option<FontIndex>>,
}

/// Packed cache key for codepoint-to-font resolution.
///
/// Bit layout:
/// [31:0]  Unicode codepoint
/// [39:32] font style
/// [47:40] presentation (0 = none, 1 = text, 2 = emoji)
/// [63:48] reserved
#[derive(Clone, Copy, Eq, PartialEq, Hash)]
#[repr(transparent)]
struct CodepointKey(u64);

impl CodepointKey {
    #[inline]
    fn new(cp: u32, style: FontStyle, presentation: Option<Presentation>) -> Self {
        let presentation: u64 = match presentation {
            None => 0,
            Some(Presentation::Text) => 1,
            Some(Presentation::Emoji) => 2,
        };

        Self(u64::from(cp) | ((style as u64) << 32) | (presentation << 40))
    }
}

impl SharedGrid {
    pub fn new(mut resolver: CodepointResolver) -> Result<Self> {
        let font_metrics = resolver.collection.update_metrics()?;

        let mut codepoints = FxHashMap::default();
        codepoints.reserve(128);

        Ok(Self {
            inner: RwLock::new(SharedGridInner { resolver, codepoints }),
            metrics: font_metrics,
        })
    }

    pub fn get_index(
        &self,
        cp: u32,
        style: FontStyle,
        presentation: Option<Presentation>,
    ) -> Result<Option<FontIndex>, FontError> {
        let key = CodepointKey::new(cp, style, presentation);

        {
            if let Some(&found) = self.inner.read().codepoints.get(&key) {
                return Ok(found);
            }
        }

        let inner = &mut *self.inner.write();
        match inner.codepoints.entry(key) {
            Entry::Occupied(found) => Ok(*found.get()),
            Entry::Vacant(slot) => {
                let resolved = inner.resolver.get_index(cp, style, presentation);

                if let Some(index) = resolved {
                    // Sprite fonts don't need to be preloaded
                    if index.special().is_none() {
                        inner.resolver.collection.ensure_loaded(index)?;
                    }
                }

                slot.insert(resolved);
                Ok(resolved)
            }
        }
    }

    /// Borrow an existing face while holding the grid's read lock.
    /// Does not load the face. The closure must not request a grid
    /// write lock (for example, by resolving an uncached codepoint).
    #[inline]
    pub fn with_face<T>(&self, idx: FontIndex, f: impl FnOnce(&Face) -> T) -> Result<T, FontError> {
        let inner = self.inner.read();
        let face = inner.resolver.collection.get_face(idx)?;
        Ok(f(face))
    }

    #[inline]
    pub fn has_codepoint(&self, idx: FontIndex, cp: u32, p: Option<Presentation>) -> bool {
        let p_mode = p.map_or(PresentationMode::Any, PresentationMode::Explicit);

        let inner = self.inner.read();
        inner.resolver.collection.has_codepoint(idx, cp, p_mode)
    }

    #[inline]
    pub fn dpi(&self) -> (u16, u16) {
        let size = self.inner.read().resolver.collection.size;
        (size.x_dpi, size.y_dpi)
    }
}
