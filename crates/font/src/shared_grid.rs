use std::collections::hash_map::Entry;
use std::hash::{Hash, Hasher};
use std::sync::RwLock;
use std::sync::atomic::Ordering;

#[cfg(target_os = "windows")]
use anyhow::{Result, anyhow};
use ghostty::RawCell;
use rapidhash::{HashMapExt, RapidHashMap};

use crate::atlas::{Atlas, Format, INITIAL_SIZE, Region};
#[cfg(target_os = "windows")]
use crate::backend::dwrite::face::{
    DWriteGlyphRasterizer, DWriteGridMetricsConfig, measure_grid_metrics,
};
#[cfg(target_os = "windows")]
use crate::backend::dwrite::fallback::FontFallbackContext;
#[cfg(target_os = "windows")]
use crate::backend::dwrite::variation::{StyleVariationRequest, resolve_primary_faces};
use crate::cache::glyph_cache::{CachedGlyph, GlyphAtlasKind, GlyphCache, GlyphKey};
use crate::collection::Collection;
use crate::resolver::CodepointResolver;
use crate::types::{FontIndex, Style};
#[cfg(target_os = "windows")]
use windows::Win32::Graphics::DirectWrite::{IDWriteFactory2, IDWriteFactory6, IDWriteFontFace2};
#[cfg(target_os = "windows")]
use windows_core::Interface;

pub use crate::types::Presentation;

const GLYPH_PADDING: u32 = 1;

/// Grid metrics owned by the shared font runtime.
#[derive(Clone, Copy, Debug, Default)]
pub struct GridMetrics {
    pub cell_width: f32,
    pub cell_height: f32,
    pub baseline: f32,
}

impl PartialEq for GridMetrics {
    fn eq(&self, other: &Self) -> bool {
        self.cell_width.to_bits() == other.cell_width.to_bits()
            && self.cell_height.to_bits() == other.cell_height.to_bits()
            && self.baseline.to_bits() == other.baseline.to_bits()
    }
}

impl Eq for GridMetrics {}

impl Hash for GridMetrics {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u32(self.cell_width.to_bits());
        state.write_u32(self.cell_height.to_bits());
        state.write_u32(self.baseline.to_bits());
    }
}

pub struct AtlasSnapshot<'a> {
    pub format: Format,
    pub size: u32,
    pub modified: u64,
    pub resized: u64,
    pub data: &'a [u8],
}

/// Shared font runtime — Ghostty's `SharedGrid`.
pub struct SharedGrid {
    max_atlas_size: u32,
    #[cfg(target_os = "windows")]
    dwrite: Option<DWriteSharedGrid>,
    inner: RwLock<SharedGridInner>,
}

#[cfg(target_os = "windows")]
struct DWriteSharedGrid {
    rasterizer: DWriteGlyphRasterizer,
    raster_font_size: f32,
    raster_scale_factor: f32,
    raster_cell_width: f32,
}

struct SharedGridInner {
    atlas_grayscale: Atlas,
    atlas_color: Atlas,
    codepoints: RapidHashMap<CodepointKey, Option<FontIndex>>,
    glyphs: GlyphCache,
    resolver: CodepointResolver,
    metrics: GridMetrics,
}

impl SharedGridInner {
    #[inline]
    fn atlas_for_kind(&self, kind: GlyphAtlasKind) -> &Atlas {
        match kind {
            GlyphAtlasKind::Grayscale => &self.atlas_grayscale,
            GlyphAtlasKind::Color => &self.atlas_color,
        }
    }

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

    pub fn with_atlas_max_size(
        resolver: CodepointResolver,
        metrics: GridMetrics,
        max_atlas_size: u32,
    ) -> Self {
        Self {
            max_atlas_size,
            #[cfg(target_os = "windows")]
            dwrite: None,
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
    pub fn configure_dwrite(
        &mut self,
        factory: &IDWriteFactory6,
        requests: &[StyleVariationRequest<'_>; Style::COUNT],
        metric_config: DWriteGridMetricsConfig,
        raster_scale_factor: f32,
        fallback: Option<FontFallbackContext>,
        locale: &str,
    ) -> Result<()> {
        let faces = resolve_primary_faces(factory, requests)?;
        let metrics = measure_grid_metrics(&faces[Style::Normal as usize], &metric_config)
            .unwrap_or(GridMetrics {
                cell_width: metric_config.font_size * 0.6,
                cell_height: metric_config.font_size * 1.3,
                baseline: metric_config.font_size,
            });

        {
            let mut inner = self.inner.write().expect("shared grid poisoned");
            for style in Style::ALL {
                inner
                    .resolver
                    .add_dwrite_face(style, &faces[style as usize])?;
            }
            if let Some(fallback) = fallback {
                inner.resolver.set_dwrite_fallback(fallback, locale);
            }
            inner.codepoints.clear();
            inner.metrics = metrics;
        }

        let factory2 = factory.cast::<IDWriteFactory2>()?;
        let raster_scale = raster_scale_factor.max(1.0);
        self.dwrite = Some(DWriteSharedGrid {
            rasterizer: DWriteGlyphRasterizer::new(factory2),
            raster_font_size: metric_config.font_size,
            raster_scale_factor: raster_scale,
            raster_cell_width: (metrics.cell_width * raster_scale).round().max(1.0),
        });
        Ok(())
    }

    pub fn metrics(&self) -> GridMetrics {
        self.inner.read().expect("shared grid poisoned").metrics
    }

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
                let resolved = unsafe { (*resolver).get_index(codepoint, style, presentation) };
                slot.insert(resolved);
                resolved
            }
        }
    }

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
    pub(crate) fn face_for_index(&self, index: FontIndex) -> Option<IDWriteFontFace2> {
        let inner = self.inner.read().expect("shared grid poisoned");
        inner.resolver.collection.face_for_index(index)
    }

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

    pub fn clear_codepoint_cache(&self) {
        let mut inner = self.inner.write().expect("shared grid poisoned");
        inner.codepoints.clear();
    }

    pub fn with_atlas_snapshot<R>(
        &self,
        kind: GlyphAtlasKind,
        f: impl FnOnce(AtlasSnapshot<'_>) -> R,
    ) -> R {
        let inner = self.inner.read().expect("shared grid poisoned");
        let atlas = inner.atlas_for_kind(kind);
        f(AtlasSnapshot {
            format: atlas.format(),
            size: atlas.size(),
            modified: atlas.modified.load(Ordering::Relaxed),
            resized: atlas.resized.load(Ordering::Relaxed),
            data: atlas.data(),
        })
    }

    #[cfg(target_os = "windows")]
    pub fn render_glyph(&self, key: GlyphKey) -> Result<CachedGlyph> {
        {
            let inner = self.inner.read().expect("shared grid poisoned");
            if let Some(found) = inner.glyphs.get(&key) {
                return Ok(*found);
            }
        }

        let dwrite = self
            .dwrite
            .as_ref()
            .ok_or_else(|| anyhow!("shared grid is missing dwrite runtime"))?;
        let mut inner = self.inner.write().expect("shared grid poisoned");
        if let Some(found) = inner.glyphs.get(&key) {
            return Ok(*found);
        }

        let Some(face2) = inner.resolver.collection.face_for_index(key.font_index()) else {
            return Ok(inner
                .glyphs
                .insert_or_get_existing(key, CachedGlyph::default()));
        };
        let raster = dwrite.rasterizer.rasterize(
            &face2,
            key.glyph_index() as u16,
            dwrite.raster_font_size,
            dwrite.raster_scale_factor,
        )?;
        if raster.width == 0 || raster.height == 0 {
            return Ok(inner
                .glyphs
                .insert_or_get_existing(key, CachedGlyph::default()));
        }

        let atlas_kind = raster.atlas_kind;
        let mut did_reset = false;
        loop {
            let reserve = inner
                .atlas_for_kind_mut(atlas_kind)
                .reserve(raster.width + GLYPH_PADDING, raster.height + GLYPH_PADDING);

            match reserve {
                Ok(region) => {
                    let glyph_region = Region {
                        x: region.x,
                        y: region.y,
                        width: raster.width,
                        height: raster.height,
                    };
                    inner
                        .atlas_for_kind_mut(atlas_kind)
                        .set(glyph_region, &raster.pixels);
                    let cached = CachedGlyph {
                        width: raster.width,
                        height: raster.height,
                        offset_x: raster.offset_x,
                        offset_y: raster.offset_y,
                        atlas_x: glyph_region.x,
                        atlas_y: glyph_region.y,
                        atlas_kind: Some(atlas_kind),
                        overlap_split: Self::glyph_needs_overlap_split(
                            raster.offset_x,
                            raster.width,
                            Self::glyph_cell_width(key),
                            dwrite.raster_cell_width,
                        ),
                    };
                    return Ok(inner.glyphs.insert_or_get_existing(key, cached));
                }
                Err(_) => {
                    if self.try_grow_atlas(&mut inner, atlas_kind) {
                        continue;
                    }
                    if did_reset {
                        return Err(anyhow!("atlas allocation failed after hard-limit reset"));
                    }
                    did_reset = true;
                    self.reset_all_atlases(&mut inner);
                    inner.glyphs.clear();
                }
            }
        }
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

    fn try_grow_atlas(&self, inner: &mut SharedGridInner, kind: GlyphAtlasKind) -> bool {
        let current_size = inner.atlas_for_kind(kind).size();
        let new_size = current_size * 2;
        if self.max_atlas_size > 0 && new_size > self.max_atlas_size {
            return false;
        }
        inner.atlas_for_kind_mut(kind).grow(new_size);
        true
    }

    fn reset_all_atlases(&self, inner: &mut SharedGridInner) {
        inner.atlas_grayscale.clear();
        inner.atlas_color.clear();
    }

    #[cfg(target_os = "windows")]
    fn glyph_needs_overlap_split(
        offset_x: i32,
        width: u32,
        cell_width_count: u8,
        raster_cell_width: f32,
    ) -> bool {
        if raster_cell_width <= 0.0 || (width as f32) < raster_cell_width {
            return false;
        }

        let half_cell_width = raster_cell_width * 0.5;
        let logical_advance_width = raster_cell_width * f32::from(cell_width_count.max(1));
        let overhang_left = offset_x as f32;
        let overhang_right = overhang_left + width as f32;

        overhang_left <= -half_cell_width
            || overhang_right >= logical_advance_width + half_cell_width
    }

    #[cfg(target_os = "windows")]
    fn glyph_cell_width(key: GlyphKey) -> u8 {
        let packed = key.packed_options();
        ((packed & 0b11) as u8).max(1)
    }

    #[cfg(test)]
    fn with_atlas_write<R>(&self, kind: GlyphAtlasKind, f: impl FnOnce(&mut Atlas) -> R) -> R {
        let mut inner = self.inner.write().expect("shared grid poisoned");
        f(inner.atlas_for_kind_mut(kind))
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

    use crate::backend::dwrite::face::DWriteGridMetricsConfig;
    use crate::backend::dwrite::variation::StyleVariationRequest;
    use crate::types::Presentation;
    use crate::types::Style;
    use windows::Win32::Graphics::DirectWrite::{
        DWRITE_FACTORY_TYPE_SHARED, DWriteCreateFactory, IDWriteFactory6,
    };

    fn raw_grapheme_codepoint(cp: u32) -> RawCell {
        let bits = 1u64 | ((cp as u64) << 2);
        unsafe { std::mem::transmute(bits) }
    }

    fn integration_grid() -> SharedGrid {
        let factory6: IDWriteFactory6 =
            unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).expect("create dwrite") };

        let mut grid = SharedGrid::with_collection(Collection::new(), GridMetrics::default());
        let requests: [StyleVariationRequest<'_>; Style::COUNT] =
            std::array::from_fn(|_| StyleVariationRequest {
                family: "Segoe UI",
                axes: Default::default(),
            });
        grid.configure_dwrite(
            &factory6,
            &requests,
            DWriteGridMetricsConfig {
                font_size: 16.0,
                cell_width: 0.0,
                line_height: 0.0,
                baseline: 0.0,
            },
            16.0,
            None,
            "en-US",
        )
        .expect("configure dwrite");
        grid
    }

    #[test]
    fn overlap_split_trigger_ignores_regular_aa_overhang() {
        assert!(!SharedGrid::glyph_needs_overlap_split(-2, 10, 1, 10.0));
    }

    #[test]
    fn overlap_split_trigger_matches_wt_ligature_threshold() {
        assert!(SharedGrid::glyph_needs_overlap_split(0, 18, 1, 10.0));
        assert!(!SharedGrid::glyph_needs_overlap_split(0, 20, 2, 10.0));
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
            grid.with_atlas_snapshot(GlyphAtlasKind::Grayscale, |atlas| atlas.format),
            Format::Grayscale
        );
        assert_eq!(
            grid.with_atlas_snapshot(GlyphAtlasKind::Color, |atlas| atlas.format),
            Format::Bgra
        );
        assert_eq!(
            grid.with_atlas_snapshot(GlyphAtlasKind::Grayscale, |atlas| atlas.size),
            INITIAL_SIZE
        );
        assert_eq!(
            grid.with_atlas_snapshot(GlyphAtlasKind::Color, |atlas| atlas.size),
            INITIAL_SIZE
        );
    }

    #[test]
    fn presentation_routes_to_expected_atlas() {
        let grid = SharedGrid::with_collection(Collection::new(), GridMetrics::default());
        assert_eq!(
            grid.with_atlas_snapshot(Presentation::Text.atlas_kind(), |atlas| atlas.format),
            Format::Grayscale
        );
        assert_eq!(
            grid.with_atlas_snapshot(Presentation::Emoji.atlas_kind(), |atlas| atlas.format),
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
        let mut inner = grid.inner.write().expect("shared grid poisoned");
        assert!(!grid.try_grow_atlas(&mut inner, GlyphAtlasKind::Grayscale));
    }

    #[test]
    fn try_grow_succeeds_within_limit() {
        let grid = SharedGrid::with_atlas_max_size(
            CodepointResolver::new(Collection::new()),
            GridMetrics::default(),
            2048,
        );
        let mut inner = grid.inner.write().expect("shared grid poisoned");
        assert!(grid.try_grow_atlas(&mut inner, GlyphAtlasKind::Grayscale));
        assert_eq!(inner.atlas_for_kind(GlyphAtlasKind::Grayscale).size(), 1024);
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

        let mut inner = grid.inner.write().expect("shared grid poisoned");
        grid.reset_all_atlases(&mut inner);

        drop(inner);
        grid.with_atlas_write(GlyphAtlasKind::Grayscale, |atlas| {
            atlas.reserve(INITIAL_SIZE - 2, INITIAL_SIZE - 2).unwrap();
        });
        grid.with_atlas_write(GlyphAtlasKind::Color, |atlas| {
            atlas.reserve(INITIAL_SIZE - 2, INITIAL_SIZE - 2).unwrap();
        });
    }
}
