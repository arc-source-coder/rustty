/// Position-independent glyph cache.
///
/// Maps a [`GlyphKey`] to atlas coordinates and glyph metrics. The key
/// deliberately excludes absolute position and subpixel variant (for the
/// grayscale path) — fractional placement is applied at draw time.
///
/// ## GlyphKey packing
///
/// The entire `GlyphKey` is packed into a single `u64`, exactly matching
/// Ghostty's `GlyphKey.Packed` strategy. This is possible because we
/// intern DWrite font face pointers into dense [`FontIndex`] values
/// (u16, same size as Ghostty's `Collection.Index`).
///
/// Bit layout:
///   `[15:0]`  `FontIndex` (16 bits — 3 style + 13 index)
///   `[47:16]` `glyph_index` (32 bits)
///   `[63:48]` packed render options (16 bits — 13 used, 3 reserved)
///
/// This gives us:
/// - **One-word hash**: a single `u64` → one `write_u64` into rapidhash
/// - **One-word equality**: `a.0 == b.0` with a glyph-first short-circuit
/// - **Zero padding/alignment waste**
///
/// Ghostty reference:
///   `zig/ghostty/src/font/SharedGrid.zig` — `GlyphKey`,
///   `Render`, and `renderGlyph`.
///
/// WT parallel:
///   `BackendD3D.h` — `AtlasGlyphEntry` keyed by `glyphIndex` within a
///   per-font-face `AtlasFontFaceEntry`.
use crate::types::FontIndex;
use rapidhash::{HashMapExt, RapidHashMap};
use std::hash::{Hash, Hasher};

/// Which atlas a glyph was rasterized into.
///
/// Ghostty reference: `Presentation` enum (`text` / `emoji`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GlyphAtlasKind {
    /// Grayscale (R8) atlas for text glyphs.
    Grayscale,
    /// Color (BGRA) atlas for emoji / color glyphs.
    Color,
}

/// Render options that affect rasterization output and therefore must be
/// part of the cache key.
///
/// Mirrors the fields Ghostty packs into `GlyphKey.Packed.opts`:
///   `cell_width` (2 bits), `thicken` (1 bit), `thicken_strength` (8 bits),
///   `constraint_width` (2 bits) = 13 bits total, packed into a `u16`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct GlyphRenderOptions(u16);

impl GlyphRenderOptions {
    /// Number of grid cells this glyph occupies (0 = unset, 1–3 valid).
    ///
    /// Ghostty: `RenderOptions.cell_width: ?u2`.
    #[inline(always)]
    pub const fn cell_width(self) -> u8 {
        (self.0 & 0b11) as u8
    }

    /// Whether artificial thickening is applied (macOS-style bold synthesis).
    ///
    /// Ghostty: `RenderOptions.thicken: bool`.
    #[inline(always)]
    pub const fn thicken(self) -> bool {
        ((self.0 >> 2) & 1) != 0
    }

    /// Thickening strength 0–255.
    ///
    /// Ghostty: `RenderOptions.thicken_strength: u8`.
    #[inline(always)]
    pub const fn thicken_strength(self) -> u8 {
        ((self.0 >> 3) & 0xFF) as u8
    }

    /// Constraint width for glyph resizing/alignment.
    ///
    /// Ghostty: `RenderOptions.constraint_width: u2`.
    #[inline(always)]
    pub const fn constraint_width(self) -> u8 {
        ((self.0 >> 11) & 0b11) as u8
    }

    #[inline(always)]
    pub const fn with_cell_width(self, cell_width: u8) -> Self {
        debug_assert!(cell_width <= 3);
        let cleared = self.0 & !0b11;
        Self(cleared | ((cell_width as u16) & 0b11))
    }

    #[inline(always)]
    pub const fn with_thicken(self, thicken: bool) -> Self {
        let cleared = self.0 & !(1 << 2);
        Self(cleared | ((thicken as u16) << 2))
    }

    #[inline(always)]
    pub const fn with_thicken_strength(self, thicken_strength: u8) -> Self {
        let cleared = self.0 & !(0xFF << 3);
        Self(cleared | ((thicken_strength as u16) << 3))
    }

    #[inline(always)]
    pub const fn with_constraint_width(self, constraint_width: u8) -> Self {
        debug_assert!(constraint_width <= 3);
        let cleared = self.0 & !(0b11 << 11);
        Self(cleared | (((constraint_width as u16) & 0b11) << 11))
    }

    /// Pack all option fields into a `u16`.
    ///
    /// Bit layout (LSB first):
    ///   `[1:0]`   cell_width       (2 bits)
    ///   `[2]`     thicken          (1 bit)
    ///   `[10:3]`  thicken_strength (8 bits)
    ///   `[12:11]` constraint_width (2 bits)
    ///   `[15:13]` reserved / zero
    ///
    /// Matches Ghostty's `GlyphKey.Packed.opts` packed struct layout.
    #[inline(always)]
    pub fn pack(self) -> u16 {
        self.0
    }
}

impl Default for GlyphRenderOptions {
    fn default() -> Self {
        Self(0)
            .with_cell_width(0)
            .with_thicken(false)
            .with_thicken_strength(255)
            .with_constraint_width(1)
    }
}

/// Position-independent glyph cache key packed into a single `u64`.
///
/// Exact Ghostty equivalent: `GlyphKey.Packed` — a `packed struct(u64)`
/// of `(Collection.Index, glyph: u32, opts: packed struct(u16))`.
///
/// ## Bit layout
///
///   `[15:0]`  `FontIndex` (16 bits)
///   `[47:16]` `glyph_index` (32 bits)
///   `[63:48]` packed render options (16 bits)
///
/// ## Equality strategy
///
/// Equality compares the packed `u64` directly, but checks the glyph
/// field first — in most lookups the glyph differs, so this
/// short-circuits early. This matches Ghostty's `GlyphKey.Context.eql`:
///   `a.glyph == b.glyph and Packed.from(a) == Packed.from(b)`
#[derive(Clone, Copy, Debug)]
pub struct GlyphKey {
    packed: u64,
}

impl GlyphKey {
    /// Construct a packed key from components.
    #[inline(always)]
    pub fn new(font_index: FontIndex, glyph_index: u32, options: GlyphRenderOptions) -> Self {
        Self {
            packed: (font_index.to_u16() as u64)
                | ((glyph_index as u64) << 16)
                | ((options.pack() as u64) << 48),
        }
    }

    /// Extract the dense font face id.
    #[inline(always)]
    pub fn font_index(self) -> FontIndex {
        // SAFETY: FontIndex is repr(transparent) over u16, and the
        // low 16 bits were produced by FontIndex::to_u16().
        unsafe { std::mem::transmute(self.packed as u16) }
    }

    /// Extract the glyph index.
    #[inline(always)]
    pub fn glyph_index(self) -> u32 {
        (self.packed >> 16) as u32
    }

    /// Extract the packed render options as a raw u16.
    #[inline(always)]
    pub fn packed_options(self) -> u16 {
        (self.packed >> 48) as u16
    }
}

/// Single-word equality. Ghostty does `a.glyph == b.glyph and
/// Packed.from(a) == Packed.from(b)` — checking glyph first for
/// early exit. With our layout, the glyph occupies bits [47:16], so
/// if glyphs differ the full u64 compare will also fail immediately
/// on most architectures (single CMP instruction).
impl PartialEq for GlyphKey {
    #[inline(always)]
    fn eq(&self, other: &Self) -> bool {
        self.packed == other.packed
    }
}
impl Eq for GlyphKey {}

/// Single-word hash — one `write_u64` into rapidhash.
///
/// Ghostty does `std.hash.int(@as(u64, @bitCast(packed_key)))` which
/// is essentially a single integer hash. With rapidhash's sponge mode,
/// `write_u64` on a small key is equally fast.
impl Hash for GlyphKey {
    #[inline(always)]
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(self.packed);
    }
}

/// Cached glyph — atlas coordinates and metrics sufficient for building
/// a quad instance at draw time.
///
/// Ghostty reference: `Glyph` struct in `font/Glyph.zig`.
/// WT parallel: `AtlasGlyphEntry` (offset, size, texcoord, shadingType).
#[derive(Clone, Copy, Debug, Default)]
pub struct CachedGlyph {
    /// Pixel width of the rasterized glyph.
    pub width: u32,
    /// Pixel height of the rasterized glyph.
    pub height: u32,
    /// Left bearing (horizontal offset from pen position).
    pub offset_x: i32,
    /// Top bearing (vertical offset from baseline).
    pub offset_y: i32,
    /// X coordinate of the top-left corner in the atlas texture.
    pub atlas_x: u32,
    /// Y coordinate of the top-left corner in the atlas texture.
    pub atlas_y: u32,
    /// Which atlas texture this glyph lives in.
    pub atlas_kind: Option<GlyphAtlasKind>,
    /// WT-style cached decision for whether this glyph needs per-cell overlap
    /// splitting to preserve foreground color changes across a ligature bitmap.
    pub overlap_split: bool,
}

/// HashMap-backed glyph cache.
///
/// Unlike the shaped-run cache (which uses a fixed-bucket CacheTable),
/// the glyph cache uses a regular HashMap because:
///
/// 1. Glyph entries are small and rarely evicted — the working set for a
///    typical session is bounded by the number of distinct glyphs rendered.
/// 2. Ghostty uses `std.HashMapUnmanaged` for its glyph cache too.
/// 3. We need `getOrPut` semantics for the rasterize-on-miss path, which
///    maps cleanly to HashMap's `entry` API.
///
/// Uses rapidhash for key hashing (fastest general-purpose hash passing
/// all SMHasher tests).
pub struct GlyphCache {
    map: RapidHashMap<GlyphKey, CachedGlyph>,
}

impl GlyphCache {
    /// Pre-allocate for an estimated number of distinct glyphs.
    ///
    /// Ghostty pre-allocates 128 entries
    /// (`glyphs.ensureTotalCapacity(alloc, 128)`).
    pub fn new() -> Self {
        Self {
            map: RapidHashMap::with_capacity(128),
        }
    }

    /// Look up a previously rasterized glyph.
    #[inline]
    pub fn get(&self, key: &GlyphKey) -> Option<&CachedGlyph> {
        self.map.get(key)
    }

    /// Insert a rasterized glyph into the cache, returning a reference to
    /// the stored entry.
    ///
    /// If the key already exists the entry is **overwritten** (this handles
    /// the atlas-reset/re-rasterize recovery path per Q31).
    #[inline]
    pub fn insert(&mut self, key: GlyphKey, glyph: CachedGlyph) -> &CachedGlyph {
        self.map.insert(key, glyph);
        // Logical safety: entry was just inserted so get() is guaranteed
        // to return Some.
        self.map.get(&key).unwrap()
    }

    /// Insert `glyph` if key is vacant, otherwise return existing entry.
    #[inline]
    pub fn insert_or_get_existing(&mut self, key: GlyphKey, glyph: CachedGlyph) -> CachedGlyph {
        use std::collections::hash_map::Entry;
        match self.map.entry(key) {
            Entry::Occupied(found) => *found.get(),
            Entry::Vacant(slot) => {
                slot.insert(glyph);
                glyph
            }
        }
    }

    /// Get a cached glyph or insert a new one via the provided closure.
    ///
    /// This is the primary hot-path API — mirrors Ghostty's
    /// `glyphs.getOrPut(alloc, key)` pattern in `renderGlyph`.
    ///
    /// The closure receives the key and must return either a `CachedGlyph`
    /// (after rasterizing into an atlas) or an error. On error the cache
    /// is not modified — cleaner than Ghostty's `getOrPut` + `errdefer
    /// removeByPtr` pattern.
    #[inline]
    pub fn get_or_insert_with<F, E>(
        &mut self,
        key: GlyphKey,
        rasterize: F,
    ) -> Result<&CachedGlyph, E>
    where
        F: FnOnce(&GlyphKey) -> Result<CachedGlyph, E>,
    {
        use std::collections::hash_map::Entry;
        match self.map.entry(key) {
            Entry::Occupied(e) => Ok(e.into_mut()),
            Entry::Vacant(e) => {
                let glyph = rasterize(e.key())?;
                Ok(e.insert(glyph))
            }
        }
    }

    /// Drop all cached entries.
    ///
    /// Called on atlas reset (grow-past-hardware-limit recovery per Q31)
    /// or font configuration change, matching Ghostty's full cache clear
    /// path in `SharedGrid.deinit`.
    pub fn clear(&mut self) {
        self.map.clear();
    }

    /// Number of cached glyphs (useful for diagnostics).
    #[inline]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

impl Default for GlyphCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Style;

    fn test_key(glyph_index: u32) -> GlyphKey {
        let face = FontIndex::new(Style::Normal, 0);
        GlyphKey::new(face, glyph_index, GlyphRenderOptions::default())
    }

    fn test_glyph(atlas_x: u32) -> CachedGlyph {
        CachedGlyph {
            width: 8,
            height: 16,
            offset_x: 0,
            offset_y: 14,
            atlas_x,
            atlas_y: 0,
            atlas_kind: Some(GlyphAtlasKind::Grayscale),
            overlap_split: false,
        }
    }

    #[test]
    fn miss_then_hit() {
        let mut cache = GlyphCache::new();
        let key = test_key(42);
        assert!(cache.get(&key).is_none());

        cache.insert(key, test_glyph(10));
        let entry = cache.get(&key).unwrap();
        assert_eq!(entry.atlas_x, 10);
    }

    #[test]
    fn get_or_insert_with_caches() {
        let mut cache = GlyphCache::new();
        let key = test_key(99);

        let mut calls = 0u32;
        let result: Result<&CachedGlyph, ()> = cache.get_or_insert_with(key, |_k| {
            calls += 1;
            Ok(test_glyph(20))
        });
        assert!(result.is_ok());
        assert_eq!(calls, 1);

        // Second call should hit cache — closure not called.
        let _result: Result<&CachedGlyph, ()> = cache.get_or_insert_with(key, |_k| {
            calls += 1;
            Ok(test_glyph(30))
        });
        assert_eq!(calls, 1);
        assert_eq!(cache.get(&key).unwrap().atlas_x, 20);
    }

    #[test]
    fn different_render_options_are_distinct() {
        let mut cache = GlyphCache::new();
        let face = FontIndex::new(Style::Normal, 0);
        let key_a = GlyphKey::new(face, 10, GlyphRenderOptions::default().with_cell_width(1));
        let key_b = GlyphKey::new(face, 10, GlyphRenderOptions::default().with_cell_width(2));
        cache.insert(key_a, test_glyph(100));
        cache.insert(key_b, test_glyph(200));
        assert_eq!(cache.get(&key_a).unwrap().atlas_x, 100);
        assert_eq!(cache.get(&key_b).unwrap().atlas_x, 200);
    }

    #[test]
    fn clear_empties_cache() {
        let mut cache = GlyphCache::new();
        cache.insert(test_key(1), test_glyph(0));
        cache.insert(test_key(2), test_glyph(0));
        assert_eq!(cache.len(), 2);
        cache.clear();
        assert!(cache.is_empty());
    }

    #[test]
    fn position_not_in_key() {
        let key = test_key(55);
        let mut cache = GlyphCache::new();
        cache.insert(key, test_glyph(77));
        assert_eq!(cache.get(&key).unwrap().atlas_x, 77);
    }

    #[test]
    fn packed_roundtrip() {
        let face = FontIndex::new(Style::BoldItalic, 42);
        let opts = GlyphRenderOptions::default()
            .with_cell_width(2)
            .with_thicken(true)
            .with_thicken_strength(200)
            .with_constraint_width(3);
        let key = GlyphKey::new(face, 0xDEAD_BEEF, opts);

        assert_eq!(key.font_index(), face);
        assert_eq!(key.glyph_index(), 0xDEAD_BEEF);
        assert_eq!(key.packed_options(), opts.pack());
    }

    #[test]
    fn packed_size() {
        assert_eq!(std::mem::size_of::<GlyphKey>(), 8);
    }

    #[test]
    fn hash_consistency() {
        use std::hash::BuildHasher;
        let state = rapidhash::fast::RandomState::default();
        let k1 = test_key(42);
        let k2 = test_key(42);
        assert_eq!(state.hash_one(k1), state.hash_one(k2));
    }
}
