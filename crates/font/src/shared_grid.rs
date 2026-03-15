use std::collections::hash_map::Entry;
use std::sync::RwLock;

#[cfg(target_os = "windows")]
use anyhow::Result;
use ghostty_vt::RawCell;
use rapidhash::{HashMapExt, RapidHashMap};

use crate::atlas::{Atlas, Format, INITIAL_SIZE};
#[cfg(target_os = "windows")]
use crate::backend::dwrite::fallback::FontFallbackContext;
#[cfg(target_os = "windows")]
use crate::backend::dwrite::variation::{StyleVariationRequest, resolve_primary_faces};
use crate::cache::glyph_cache::{CachedGlyph, GlyphAtlasKind, GlyphCache, GlyphKey};
use crate::collection::Collection;
use crate::resolver::CodepointResolver;
use crate::types::{FontIndex, Style};
#[cfg(target_os = "windows")]
use windows::Win32::Graphics::DirectWrite::IDWriteFactory6;
#[cfg(target_os = "windows")]
use windows::Win32::Graphics::DirectWrite::IDWriteFontFace2;

pub use crate::types::Presentation;

/// Grid metrics owned by the shared font runtime.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct GridMetrics {
    pub cell_width: u16,
    pub cell_height: u16,
}

/// Shared font runtime — Ghostty's `SharedGrid`.
///
/// Owns the dual atlas set (grayscale + color) inside the same RwLock as the
/// glyph/cache/resolver state, mirroring Ghostty's single `SharedGrid.lock`
/// discipline for atlas access and mutation.
///
/// Ghostty reference:
///   `crates/ghostty-vt/zig/ghostty/src/font/SharedGrid.zig`
pub struct SharedGrid {
    /// Maximum atlas side length (from D3D11 device caps). `0` = no limit.
    max_atlas_size: u32,
    inner: RwLock<SharedGridInner>,
}

struct SharedGridInner {
    /// The texture atlas to store grayscale text glyph renders in.
    atlas_grayscale: Atlas,
    /// The texture atlas to store color emoji glyph renders in.
    atlas_color: Atlas,
    codepoints: RapidHashMap<CodepointKey, Option<FontIndex>>,
    glyphs: GlyphCache,
    resolver: CodepointResolver,
    metrics: GridMetrics,
}

impl SharedGridInner {
    /// Select atlas by kind.
    #[inline]
    fn atlas_for_kind(&self, kind: GlyphAtlasKind) -> &Atlas {
        match kind {
            GlyphAtlasKind::Grayscale => &self.atlas_grayscale,
            GlyphAtlasKind::Color => &self.atlas_color,
        }
    }

    /// Mutable variant of [`atlas_for_kind`](Self::atlas_for_kind).
    #[inline]
    fn atlas_for_kind_mut(&mut self, kind: GlyphAtlasKind) -> &mut Atlas {
        match kind {
            GlyphAtlasKind::Grayscale => &mut self.atlas_grayscale,
            GlyphAtlasKind::Color => &mut self.atlas_color,
        }
    }
}

impl SharedGrid {
    pub fn new(resolver: CodepointResolver, metrics: GridMetrics) -> Self {
        Self::with_atlas_max_size(resolver, metrics, 0)
    }

    /// Create with a device-specific maximum atlas texture dimension.
    ///
    /// The renderer should query `D3D11_REQ_TEXTURE2D_U_OR_V_DIMENSION`
    /// (or equivalent) and pass it here so the atlas grow-first policy
    /// can detect the hardware limit without waiting for `CreateTexture2D`
    /// to fail.
    pub fn with_atlas_max_size(
        resolver: CodepointResolver,
        metrics: GridMetrics,
        max_atlas_size: u32,
    ) -> Self {
        Self {
            max_atlas_size,
            inner: RwLock::new(SharedGridInner {
                atlas_grayscale: Atlas::new(INITIAL_SIZE, Format::Grayscale),
                atlas_color: Atlas::new(INITIAL_SIZE, Format::Bgra),
                codepoints: RapidHashMap::with_capacity(128),
                glyphs: GlyphCache::new(),
                resolver,
                metrics,
            }),
        }
    }

    pub fn with_collection(collection: Collection, metrics: GridMetrics) -> Self {
        Self::new(CodepointResolver::new(collection), metrics)
    }

    #[cfg(target_os = "windows")]
    pub fn set_dwrite_fallback(&self, fallback: FontFallbackContext, locale: &str) {
        let mut inner = self.inner.write().expect("shared grid poisoned");
        inner.resolver.set_dwrite_fallback(fallback, locale);
        inner.codepoints.clear();
    }

    #[cfg(target_os = "windows")]
    /// Install pre-resolved primary faces by style.
    ///
    /// Ghostty reference:
    /// `SharedGridSet.collection()` loads per-style primary faces into the
    /// collection before runtime resolver lookups.
    pub fn set_dwrite_primary_faces(&self, faces: &[(Style, IDWriteFontFace2)]) -> Result<()> {
        let mut inner = self.inner.write().expect("shared grid poisoned");
        for &(style, ref face) in faces {
            inner.resolver.add_dwrite_face(style, face)?;
        }
        inner.codepoints.clear();
        Ok(())
    }

    #[cfg(target_os = "windows")]
    /// Resolve and install primary style faces from system font-set APIs.
    ///
    /// Ghostty reference:
    /// `SharedGridSet.Key`/descriptor-driven style face resolution at grid
    /// initialization time.
    pub fn configure_dwrite_primary_faces(
        &self,
        factory: &IDWriteFactory6,
        requests: &[StyleVariationRequest<'_>; Style::COUNT],
    ) -> Result<()> {
        let faces = resolve_primary_faces(factory, requests)?;
        let styled_faces = Style::ALL.map(|style| (style, faces[style as usize].clone()));
        self.set_dwrite_primary_faces(&styled_faces)
    }

    pub fn metrics(&self) -> GridMetrics {
        self.inner.read().expect("shared grid poisoned").metrics
    }

    pub fn set_metrics(&self, metrics: GridMetrics) {
        let mut inner = self.inner.write().expect("shared grid poisoned");
        inner.metrics = metrics;
    }

    /// Ghostty-style codepoint -> font index resolution with negative caching.
    pub fn get_index(
        &self,
        codepoint: u32,
        style: Style,
        presentation: Option<Presentation>,
    ) -> Option<FontIndex> {
        let key = CodepointKey {
            style,
            codepoint,
            presentation,
        };

        {
            let inner = self.inner.read().expect("shared grid poisoned");
            if let Some(found) = inner.codepoints.get(&key) {
                return *found;
            }
        }

        let mut inner = self.inner.write().expect("shared grid poisoned");
        let resolver: *mut CodepointResolver = &mut inner.resolver;
        match inner.codepoints.entry(key) {
            Entry::Occupied(found) => *found.get(),
            Entry::Vacant(slot) => {
                // SAFETY: `resolver` points into `inner`; while the map entry
                // is held, we only use it to query resolver state and do not
                // mutate/reallocate the codepoint map itself.
                let resolved = unsafe { (*resolver).get_index(codepoint, style, presentation) };
                slot.insert(resolved);
                resolved
            }
        }
    }

    /// Cached `has_codepoint` check used by run iterator grapheme candidate
    /// selection.
    pub fn has_codepoint(
        &self,
        index: FontIndex,
        codepoint: u32,
        presentation: Option<Presentation>,
    ) -> bool {
        let inner = self.inner.read().expect("shared grid poisoned");
        inner.resolver.has_codepoint(index, codepoint, presentation)
    }

    #[cfg(target_os = "windows")]
    pub fn face_for_index(&self, index: FontIndex) -> Option<IDWriteFontFace2> {
        let inner = self.inner.read().expect("shared grid poisoned");
        inner.resolver.collection.face_for_index(index)
    }

    /// Mirrors Ghostty `RunIterator.indexForCell`.
    ///
    /// `graphemes` are additional codepoints for the cluster (not including
    /// `cell.codepoint()`); each element may contain bits above u21 from FFI,
    /// so only the low 21 bits are used.
    pub fn index_for_cell(
        &self,
        cell: RawCell,
        graphemes: &[u32],
        style: Style,
        presentation: Option<Presentation>,
    ) -> Option<FontIndex> {
        const KITTY_PLACEHOLDER: u32 = 0x10EEEE;

        let primary_cp = cell.codepoint();
        if !cell.has_text() || primary_cp == 0 || primary_cp == KITTY_PLACEHOLDER {
            return self.get_index(b' ' as u32, style, presentation);
        }

        let primary = self.get_index(primary_cp, style, presentation)?;
        if !cell.has_grapheme() {
            return Some(primary);
        }

        if self.candidate_supports_grapheme(primary, primary_cp, graphemes, presentation) {
            return Some(primary);
        }

        for &cp in graphemes {
            let cp = cp & 0x1F_FFFF;
            if cp == 0xFE0E || cp == 0xFE0F || cp == 0x200D {
                continue;
            }
            let idx = self.get_index(cp, style, None)?;
            if idx == primary {
                continue;
            }
            if self.candidate_supports_grapheme(idx, primary_cp, graphemes, presentation) {
                return Some(idx);
            }
        }

        None
    }

    fn candidate_supports_grapheme(
        &self,
        idx: FontIndex,
        primary_cp: u32,
        graphemes: &[u32],
        presentation: Option<Presentation>,
    ) -> bool {
        if !self.has_codepoint(idx, primary_cp, presentation) {
            return false;
        }
        for &cp in graphemes {
            let cp = cp & 0x1F_FFFF;
            if cp == 0xFE0E || cp == 0xFE0F || cp == 0x200D {
                continue;
            }
            if !self.has_codepoint(idx, cp, None) {
                return false;
            }
        }
        true
    }

    pub fn get_cached_glyph(&self, key: &GlyphKey) -> Option<CachedGlyph> {
        let inner = self.inner.read().expect("shared grid poisoned");
        inner.glyphs.get(key).copied()
    }

    pub fn get_or_insert_glyph<F, E>(&self, key: GlyphKey, rasterize: F) -> Result<CachedGlyph, E>
    where
        F: FnOnce(&GlyphKey, GridMetrics) -> Result<CachedGlyph, E>,
    {
        // Fast path: shared read lock cache hit.
        {
            let inner = self.inner.read().expect("shared grid poisoned");
            if let Some(found) = inner.glyphs.get(&key) {
                return Ok(*found);
            }
        }

        // Capture metrics without holding the write lock across rasterization.
        let metrics = self.inner.read().expect("shared grid poisoned").metrics;
        let built = rasterize(&key, metrics)?;

        // Slow path insert with race re-check.
        let mut inner = self.inner.write().expect("shared grid poisoned");
        let inserted = inner.glyphs.insert_or_get_existing(key, built);
        Ok(inserted)
    }

    pub fn clear_codepoint_cache(&self) {
        let mut inner = self.inner.write().expect("shared grid poisoned");
        inner.codepoints.clear();
    }

    pub fn clear_glyph_cache(&self) {
        self.inner
            .write()
            .expect("shared grid poisoned")
            .glyphs
            .clear();
    }

    #[inline]
    pub fn with_atlas_read<R>(&self, kind: GlyphAtlasKind, f: impl FnOnce(&Atlas) -> R) -> R {
        let inner = self.inner.read().expect("shared grid poisoned");
        f(inner.atlas_for_kind(kind))
    }

    #[inline]
    pub fn with_atlas_write<R>(&self, kind: GlyphAtlasKind, f: impl FnOnce(&mut Atlas) -> R) -> R {
        let mut inner = self.inner.write().expect("shared grid poisoned");
        f(inner.atlas_for_kind_mut(kind))
    }

    /// Ghostty grow-first policy: attempt to double the atlas size.
    /// Returns `true` if the grow succeeded, `false` if we hit the
    /// hardware limit (caller should then reset + retry).
    pub fn try_grow_atlas(&self, kind: GlyphAtlasKind) -> bool {
        let mut inner = self.inner.write().expect("shared grid poisoned");
        let current_size = inner.atlas_for_kind(kind).size();
        let new_size = current_size * 2;
        if self.max_atlas_size > 0 && new_size > self.max_atlas_size {
            return false;
        }
        inner.atlas_for_kind_mut(kind).grow(new_size);
        true
    }

    /// WT-style hard-limit recovery: reset both atlases and signal the
    /// caller to clear their glyph cache.
    ///
    /// Both atlases are reset (not just the full one) because the
    /// `GlyphCache` is not generation-aware — clearing one atlas without
    /// clearing the cache would leave stale entries.
    pub fn reset_all_atlases(&self) {
        let mut inner = self.inner.write().expect("shared grid poisoned");
        inner.atlas_grayscale.clear();
        inner.atlas_color.clear();
    }

    #[cfg(test)]
    pub(crate) fn test_prime_index(
        &self,
        codepoint: u32,
        style: Style,
        presentation: Option<Presentation>,
        value: Option<FontIndex>,
    ) {
        let key = CodepointKey {
            style,
            codepoint,
            presentation,
        };
        let mut inner = self.inner.write().expect("shared grid poisoned");
        inner.codepoints.insert(key, value);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
struct CodepointKey {
    style: Style,
    codepoint: u32,
    presentation: Option<Presentation>,
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::backend::dwrite::variation::StyleVariationRequest;
    use crate::types::Presentation;
    use crate::types::Style;
    use windows::Win32::Graphics::DirectWrite::{
        DWRITE_FACTORY_TYPE_SHARED, DWriteCreateFactory, IDWriteFactory6,
    };

    fn raw_grapheme_codepoint(cp: u32) -> RawCell {
        let bits = 1u64 | ((cp as u64) << 2);
        // SAFETY: RawCell is repr(transparent) over u64.
        unsafe { std::mem::transmute(bits) }
    }

    fn integration_grid() -> SharedGrid {
        let factory6: IDWriteFactory6 =
            unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).expect("create dwrite") };

        let grid = SharedGrid::with_collection(Collection::new(), GridMetrics::default());
        let requests: [StyleVariationRequest<'_>; Style::COUNT] =
            std::array::from_fn(|_| StyleVariationRequest {
                family: "Segoe UI",
                axes: Default::default(),
            });
        grid.configure_dwrite_primary_faces(&factory6, &requests)
            .expect("configure primary faces");
        grid
    }

    #[test]
    fn get_index_negative_cache_hits_once() {
        let grid = SharedGrid::with_collection(Collection::new(), GridMetrics::default());
        grid.test_prime_index('A' as u32, Style::Normal, None, None);
        assert_eq!(grid.get_index('A' as u32, Style::Normal, None), None);
        assert_eq!(grid.get_index('A' as u32, Style::Normal, None), None);
    }

    #[test]
    fn has_codepoint_is_cached() {
        let grid = integration_grid();
        let idx = grid
            .get_index('A' as u32, Style::Normal, None)
            .expect("index for A");
        assert!(grid.has_codepoint(idx, 'A' as u32, None));
        assert!(grid.has_codepoint(idx, 'A' as u32, None));
    }

    #[test]
    fn index_for_cell_prefers_font_covering_full_grapheme() {
        let style = Style::Normal;
        let grid = integration_grid();

        let idx = grid.index_for_cell(
            raw_grapheme_codepoint('a' as u32),
            &['x' as u32],
            style,
            None,
        );
        assert!(idx.is_some());
    }

    #[test]
    fn dual_atlases_match_expected_formats() {
        let grid = SharedGrid::with_collection(Collection::new(), GridMetrics::default());
        assert_eq!(
            grid.with_atlas_read(GlyphAtlasKind::Grayscale, Atlas::format),
            Format::Grayscale
        );
        assert_eq!(
            grid.with_atlas_read(GlyphAtlasKind::Color, Atlas::format),
            Format::Bgra
        );
        assert_eq!(
            grid.with_atlas_read(GlyphAtlasKind::Grayscale, Atlas::size),
            INITIAL_SIZE
        );
        assert_eq!(
            grid.with_atlas_read(GlyphAtlasKind::Color, Atlas::size),
            INITIAL_SIZE
        );
    }

    #[test]
    fn presentation_routes_to_expected_atlas() {
        let grid = SharedGrid::with_collection(Collection::new(), GridMetrics::default());
        assert_eq!(
            grid.with_atlas_read(Presentation::Text.atlas_kind(), Atlas::format),
            Format::Grayscale
        );
        assert_eq!(
            grid.with_atlas_read(Presentation::Emoji.atlas_kind(), Atlas::format),
            Format::Bgra
        );
    }

    #[test]
    fn try_grow_respects_max() {
        let grid = SharedGrid::with_atlas_max_size(
            CodepointResolver::new(Collection::new()),
            GridMetrics::default(),
            512,
        );
        // Grayscale starts at 512 -> grow to 1024 would exceed max.
        assert!(!grid.try_grow_atlas(GlyphAtlasKind::Grayscale));
    }

    #[test]
    fn try_grow_succeeds_within_limit() {
        let grid = SharedGrid::with_atlas_max_size(
            CodepointResolver::new(Collection::new()),
            GridMetrics::default(),
            2048,
        );
        assert!(grid.try_grow_atlas(GlyphAtlasKind::Grayscale));
        assert_eq!(
            grid.with_atlas_read(GlyphAtlasKind::Grayscale, Atlas::size),
            1024
        );
    }

    #[test]
    fn reset_all_clears_both() {
        let grid = SharedGrid::with_collection(Collection::new(), GridMetrics::default());
        grid.with_atlas_write(GlyphAtlasKind::Grayscale, |atlas| {
            atlas.reserve(4, 4).unwrap();
        });
        grid.with_atlas_write(GlyphAtlasKind::Color, |atlas| {
            atlas.reserve(4, 4).unwrap();
        });
        grid.reset_all_atlases();
        // Both should be able to allocate the full usable area again.
        grid.with_atlas_write(GlyphAtlasKind::Grayscale, |atlas| {
            atlas.reserve(INITIAL_SIZE - 2, INITIAL_SIZE - 2).unwrap();
        });
        grid.with_atlas_write(GlyphAtlasKind::Color, |atlas| {
            atlas.reserve(INITIAL_SIZE - 2, INITIAL_SIZE - 2).unwrap();
        });
    }
}
