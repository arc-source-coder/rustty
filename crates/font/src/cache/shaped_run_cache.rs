/// Shaped-run cache — maps run hashes to shaped cell arrays.
///
/// Direct port of Ghostty's `font/shaper/Cache.zig`. The run hash (produced
/// by [`crate::shaper::hash::RunHasher`]) is the sole cache key; the cache
/// does not inspect the hash further — it trusts the hash quality to avoid
/// collisions, exactly as Ghostty does.
///
/// ## Value representation
///
/// Ghostty stores `[]font.shape.Cell` — an owned Zig slice (pointer + len).
/// We use `Box<[Cell]>` which is the closest Rust equivalent: an owned
/// heap allocation with a pointer and length but **no spare capacity**.
/// This is smaller than `Vec<Cell>` (2 words vs 3) and matches the
/// semantics — cached cell arrays are never mutated after insertion.
///
/// Ghostty reference:
///   `zig/ghostty/src/font/shaper/Cache.zig`
///
/// Sizing (from Ghostty source comments):
///   256 buckets — "an average of 256 frequently cached runs is a safe
///   guess for most terminal screens."
///   8 items per bucket — "decent resiliency to important runs."
use crate::cache::cache_table::CacheTable;
use crate::types::Cell;

const BUCKETS: usize = 256;
const BUCKET_SIZE: usize = 8;

/// Caches shaped cell output keyed by position-independent run hash.
///
/// The cache owns heap-allocated `Box<[Cell]>` values. On eviction the
/// box is dropped automatically (Ghostty equivalent: `alloc.free`
/// inside the eviction callback).
pub struct ShapedRunCache {
    table: CacheTable<u64, Box<[Cell]>, BUCKETS, BUCKET_SIZE>,
}

impl ShapedRunCache {
    pub fn new() -> Self {
        Self {
            table: CacheTable::new(),
        }
    }

    /// Retrieve cached shaped cells for `run_hash`, or `None` on a miss.
    ///
    /// Mirrors Ghostty `Cache.get(run)` → `self.map.get(run.hash)`.
    #[inline]
    pub fn get(&mut self, run_hash: u64) -> Option<&[Cell]> {
        self.table.get(run_hash, |k| *k == run_hash).map(|v| &**v)
    }

    /// Insert shaped cells for `run_hash`.
    ///
    /// The cells are copied into a `Box<[Cell]>` (mirrors Ghostty's
    /// `alloc.dupe`). If a previous entry is evicted to make room, its
    /// allocation is dropped — freeing the memory.
    ///
    /// Mirrors Ghostty `Cache.put(alloc, run, cells)`.
    pub fn put(&mut self, run_hash: u64, cells: &[Cell]) {
        let copy: Box<[Cell]> = cells.to_vec().into_boxed_slice();
        // Evicted (key, Box<[Cell]>) dropped here — frees the heap
        // allocation, equivalent to Ghostty's `alloc.free(kv.value)`.
        let _evicted = self.table.put(run_hash, run_hash, copy);
    }

    /// Drop all cached entries and free their allocations.
    ///
    /// Mirrors Ghostty `Cache.clear(alloc)` → iterates all buckets and
    /// frees each value, then resets lengths.
    pub fn clear(&mut self) {
        // Values are dropped by the CacheTable::clear callback receiving
        // ownership. Box<[Cell]>::drop runs automatically.
        self.table.clear(|_k, _v| {});
    }
}

impl Default for ShapedRunCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Cell;

    fn cell(x: u16, glyph: u32) -> Cell {
        Cell {
            x,
            x_offset: 0,
            y_offset: 0,
            glyph_index: glyph,
        }
    }

    #[test]
    fn miss_then_hit() {
        let mut cache = ShapedRunCache::new();
        assert!(cache.get(1).is_none());

        cache.put(1, &[cell(0, 42), cell(1, 43)]);

        let result = cache.get(1).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].glyph_index, 42);
        assert_eq!(result[1].glyph_index, 43);
    }

    #[test]
    fn eviction_frees_old_entry() {
        let mut cache = ShapedRunCache::new();
        // All these hashes map to the same bucket (hash % 256 == 0).
        // Fill bucket (8 items) then overflow to trigger eviction.
        for i in 0..9u64 {
            let hash = i * 256; // all land in bucket 0
            cache.put(hash, &[cell(i as u16, i as u32)]);
        }
        // First entry (hash=0) should have been evicted.
        assert!(cache.get(0).is_none());
        // Last entry should still be present.
        assert!(cache.get(8 * 256).is_some());
    }

    #[test]
    fn clear_empties_cache() {
        let mut cache = ShapedRunCache::new();
        cache.put(1, &[cell(0, 0)]);
        cache.clear();
        assert!(cache.get(1).is_none());
    }
}
