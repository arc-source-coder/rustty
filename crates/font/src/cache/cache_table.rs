/// Fixed-bucket associative cache with LRU eviction.
///
/// Direct port of Ghostty's `datastruct/cache_table.zig`.
///
/// Each bucket holds up to `BUCKET_SIZE` entries. Access promotes an entry to
/// the most-recent position; inserting into a full bucket evicts the oldest.
///
/// ## Memory layout
///
/// Ghostty stores the buckets as inline struct fields. In Rust, a struct
/// this large (256 × 8 × sizeof(Entry) + 256 bytes) would blow the stack if
/// constructed as a local, and every `let x = …` or function return would
/// memcpy the entire thing, so the buckets and lengths live in one heap
/// allocation. The hot path still mirrors Ghostty's flat-array memmove rotations.
///
/// This table is specialized for hash keys: the `u64` key itself selects the
/// bucket. That matches Ghostty's shaped-run cache, where `hash(key) = key`.
///
/// ## Safety invariant
///
/// `lengths[i]` always equals the number of initialized slots in
/// `buckets[i]`. Slots `0..lengths[i]` contain valid, owned values;
/// slots `lengths[i]..BUCKET_SIZE` are uninitialized memory and must
/// never be read or dropped.
///
/// Ghostty reference: `zig/ghostty/src/datastruct/cache_table.zig`
use std::mem::MaybeUninit;
use std::ptr;

use utils::asserts::assert;

/// A KV pair stored in a bucket slot.
struct Entry<V> {
    key: u64,
    value: V,
}

/// Combined heap storage for buckets and per-bucket lengths.
///
/// Packed into a single allocation so `CacheTable` only carries one
/// pointer and bucket metadata (`lengths`) stays near bucket contents.
struct Storage<V, const BUCKETS: usize, const BUCKET_SIZE: usize> {
    buckets: [[MaybeUninit<Entry<V>>; BUCKET_SIZE]; BUCKETS],
    lengths: [u8; BUCKETS],
}

pub struct CacheTable<V, const BUCKETS: usize, const BUCKET_SIZE: usize> {
    /// Single heap allocation holding all bucket slots and length counters.
    storage: Box<Storage<V, BUCKETS, BUCKET_SIZE>>,
}

impl<V, const BUCKETS: usize, const BUCKET_SIZE: usize> CacheTable<V, BUCKETS, BUCKET_SIZE> {
    pub fn new() -> Self {
        // Compile-time parameter validation (const-eval).
        const {
            assert!(BUCKETS > 0, "BUCKETS must be > 0");
            // Ghostty enforces power-of-two bucket count for fast modulus (bitwise AND).
            assert!(BUCKETS.is_power_of_two(), "BUCKETS must be a power of two");
            assert!(BUCKET_SIZE > 0, "BUCKET_SIZE must be > 0");
            const MAX_SIZE: usize = u8::MAX as usize;
            assert!(BUCKET_SIZE <= MAX_SIZE, "BUCKET_SIZE must fit in u8");
        }

        // SAFETY: zeroed `lengths` means every bucket starts empty. Zeroed
        // `MaybeUninit<Entry<...>>` bytes are never read until overwritten by
        // real entries, so this does not require `V` to be zero-valid.
        let storage = unsafe {
            let layout = std::alloc::Layout::new::<Storage<V, BUCKETS, BUCKET_SIZE>>();
            let ptr = std::alloc::alloc_zeroed(layout).cast::<Storage<V, BUCKETS, BUCKET_SIZE>>();
            if ptr.is_null() {
                std::alloc::handle_alloc_error(layout);
            }
            Box::from_raw(ptr)
        };

        Self { storage }
    }

    /// Insert `(key, value)` into the table.
    ///
    /// If the target bucket has space, the entry is appended and `None` is
    /// returned. If the bucket is full, the oldest entry is evicted and
    /// returned — exactly matching Ghostty's `rotateIn` semantics.
    pub fn put(&mut self, key: u64, value: V) -> Option<(u64, V)> {
        let idx = (key as usize) & (BUCKETS - 1);
        let s = &mut *self.storage;
        let len = s.lengths[idx] as usize;
        let bucket = &mut s.buckets[idx];

        // Safety: `len` is the initialized prefix length for this bucket.
        assert(len <= BUCKET_SIZE);

        if len < BUCKET_SIZE {
            // Bucket has room — just append.
            bucket[len] = MaybeUninit::new(Entry { key, value });
            s.lengths[idx] += 1;
            return None;
        }

        // SAFETY: the bucket is full, so every slot is initialized.
        // This is Ghostty's `fastmem.rotateIn`: read the oldest slot,
        // shift the rest down, write the new entry as most-recent.
        unsafe {
            let base: *mut Entry<V> = bucket.as_mut_ptr().cast();
            let evicted = ptr::read(base);
            ptr::copy(base.add(1), base, BUCKET_SIZE - 1);
            ptr::write(base.add(BUCKET_SIZE - 1), Entry { key, value });
            Some((evicted.key, evicted.value))
        }
    }

    /// Look up an entry by key.
    ///
    /// On hit the entry is promoted to most-recent within its bucket
    /// (mirrors Ghostty's `fastmem.rotateOnce` on the tail slice).
    /// Returns `None` on a miss.
    pub fn get(&mut self, key: u64) -> Option<&V> {
        let idx = (key as usize) & (BUCKETS - 1);
        let s = &mut *self.storage;
        let len = s.lengths[idx] as usize;
        let bucket = &mut s.buckets[idx];

        // Safety: `len` is the initialized prefix length for this bucket.
        assert(len <= BUCKET_SIZE);

        // Reverse scan — most-recent entries are at the end, so scanning
        // backwards finds hot entries faster (same order as Ghostty).
        let mut i = len;
        while i > 0 {
            i -= 1;
            // SAFETY: i < len, and all slots 0..len are initialized per
            // the length invariant.
            let entry = unsafe { bucket[i].assume_init_ref() };
            if entry.key == key {
                if i < len - 1 {
                    // SAFETY: slots i..len are initialized. This is Ghostty's
                    // `fastmem.rotateOnce` on the hit-to-tail slice.
                    unsafe {
                        let base = bucket.as_mut_ptr().cast::<Entry<V>>();
                        let hit = ptr::read(base.add(i));
                        ptr::copy(base.add(i + 1), base.add(i), len - 1 - i);
                        ptr::write(base.add(len - 1), hit);
                    }
                }
                // SAFETY: slot[len-1] is initialized (we just wrote to it,
                // or i was already len-1 so it was already initialized).
                return Some(unsafe { &bucket[len - 1].assume_init_ref().value });
            }
        }

        None
    }

    /// Remove all entries.
    pub fn clear(&mut self) {
        let s = &mut *self.storage;
        for (bucket, len) in s.buckets.iter_mut().zip(s.lengths.iter_mut()) {
            let mut remaining = *len as usize;
            assert(remaining <= BUCKET_SIZE);
            while remaining > 0 {
                remaining -= 1;
                *len = remaining as u8;
                // SAFETY: slot at *len is initialized, and decrementing first
                // keeps the table consistent if a destructor panics.
                unsafe { bucket[remaining].assume_init_drop() };
            }
        }
    }
}

impl<V, const BUCKETS: usize, const BUCKET_SIZE: usize> Drop
    for CacheTable<V, BUCKETS, BUCKET_SIZE>
{
    fn drop(&mut self) {
        self.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mirrors Ghostty's `test CacheTable` with a 2×2 table.
    #[test]
    fn basic_put_get_eviction() {
        let mut t = CacheTable::<u32, 2, 2>::new();

        // Fill both buckets (bucket 0: keys 0,2; bucket 1: keys 1,3).
        assert!(t.put(0, 10).is_none());
        assert!(t.put(1, 11).is_none());
        assert!(t.put(2, 12).is_none());
        assert!(t.put(3, 13).is_none());

        // Bucket 0 is full. Inserting key=4 (bucket 0) evicts key=0.
        let evicted = t.put(4, 14);
        assert_eq!(evicted, Some((0, 10)));

        // key=0 is gone, key=4 is present.
        assert!(t.get(0).is_none());
        assert_eq!(t.get(4), Some(&14));
    }

    #[test]
    fn get_promotes_to_most_recent() {
        let mut t = CacheTable::<u32, 2, 3>::new();
        t.put(0, 100);
        t.put(2, 200);
        t.put(4, 300);
        // Bucket 0 is full: [0, 2, 4]. Access key=0 promotes it.
        assert_eq!(t.get(0), Some(&100));
        // Now insert key=6 (bucket 0) — should evict key=2 (oldest).
        let evicted = t.put(6, 400);
        assert_eq!(evicted, Some((2, 200)));
    }

    #[test]
    fn drop_runs_destructors() {
        use std::sync::atomic::{AtomicU32, Ordering};
        static DROP_COUNT: AtomicU32 = AtomicU32::new(0);

        struct Tracked;
        impl Drop for Tracked {
            fn drop(&mut self) {
                DROP_COUNT.fetch_add(1, Ordering::Relaxed);
            }
        }

        DROP_COUNT.store(0, Ordering::Relaxed);
        {
            let mut t = CacheTable::<Tracked, 2, 2>::new();
            t.put(0, Tracked);
            t.put(1, Tracked);
            t.put(2, Tracked);
        }
        // 3 entries should have been dropped.
        assert_eq!(DROP_COUNT.load(Ordering::Relaxed), 3);
    }
}
