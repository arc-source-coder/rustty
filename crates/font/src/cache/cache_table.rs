/// Fixed-bucket associative cache with LRU eviction.
///
/// Direct port of Ghostty's `datastruct/cache_table.zig`. Each bucket holds
/// up to `BUCKET_SIZE` entries; on insert into a full bucket the oldest
/// (least-recently-used) entry is rotated out.
///
/// Access promotes an entry to the most-recent position within its bucket,
/// keeping frequently-used items pinned indefinitely.
///
/// ## Memory layout
///
/// Ghostty declares `buckets` and `lengths` as inline struct fields (Zig's
/// `= undefined` / `= @splat(0)` leaves the memory uninitialized / zeroed
/// inline). In Zig, structs are value types and are typically stack-allocated
/// or embedded directly in their parent — the compiler handles the large
/// size at the call site, and there's no equivalent of Rust's implicit move
/// semantics that would memcpy a huge struct on every return/assignment.
///
/// In Rust, a struct this large (256 × 8 × sizeof(Entry) + 256 bytes)
/// would blow the stack if constructed as a local, and every `let x = …`
/// or function return would memcpy the entire thing. Boxing places it on
/// the heap once; the outer `CacheTable` is then only one pointer wide
/// and can be moved cheaply.
///
/// We box both `buckets` and `lengths` into a single [`Storage`] allocation
/// so there is exactly **one heap allocation and one pointer indirection**
/// to reach either array. Ghostty achieves zero-indirection via inline
/// fields, but the single-box approach is the closest Rust equivalent
/// without fighting the language.
///
/// ## Performance
///
/// The hot-path (`get`/`put`) uses raw pointer memmoves instead of
/// clone-based rotation. This mirrors Ghostty's `fastmem.rotateIn` /
/// `fastmem.rotateOnce` which compile to a single `memmove` + one
/// temporary — the fastest possible LRU promotion for a flat array.
///
/// ## Safety invariant
///
/// `lengths[i]` always equals the number of initialized slots in
/// `buckets[i]`. Slots `0..lengths[i]` contain valid, owned values;
/// slots `lengths[i]..BUCKET_SIZE` are uninitialized memory and must
/// never be read or dropped.
///
/// Ghostty reference:
///   `crates/ghostty-vt/zig/ghostty/src/datastruct/cache_table.zig`
use std::mem::MaybeUninit;
use std::ptr;

/// Bucket count used by Ghostty's shaper cache.
pub const DEFAULT_BUCKETS: usize = 256;
/// Items per bucket used by Ghostty's shaper cache.
pub const DEFAULT_ITEMS_PER_BUCKET: usize = 8;

/// A KV pair stored in a bucket slot.
///
/// Kept `repr(C)` so the layout is predictable for pointer arithmetic
/// inside the `memmove`-based rotation paths.
#[repr(C)]
struct Entry<K, V> {
    key: K,
    value: V,
}

/// Combined heap storage for buckets and per-bucket lengths.
///
/// Packed into a single allocation so `CacheTable` only carries one
/// pointer (vs two `Box`es with two indirections). The `lengths` array
/// is placed after `buckets` for locality — a bucket scan reads
/// `lengths[idx]` then immediately walks `buckets[idx]`, and both live
/// in the same allocation.
#[repr(C)]
struct Storage<K, V, const BUCKETS: usize, const BUCKET_SIZE: usize> {
    buckets: [[MaybeUninit<Entry<K, V>>; BUCKET_SIZE]; BUCKETS],
    lengths: [u8; BUCKETS],
}

pub struct CacheTable<
    K,
    V,
    const BUCKETS: usize = DEFAULT_BUCKETS,
    const BUCKET_SIZE: usize = DEFAULT_ITEMS_PER_BUCKET,
> {
    /// Single heap allocation holding all bucket slots and length counters.
    storage: Box<Storage<K, V, BUCKETS, BUCKET_SIZE>>,
}

// Ghostty enforces power-of-two bucket count for fast modulus (bitwise AND).
// BUCKET_SIZE is stored as u8 per-bucket, so must fit. Must also be > 0.
//
// We enforce these at compile-time for each monomorphized instantiation.
const fn validate_params_const<const B: usize, const S: usize>() {
    assert!(B > 0, "BUCKETS must be > 0");
    assert!((B & (B - 1)) == 0, "BUCKETS must be a power of two");
    assert!(S > 0, "BUCKET_SIZE must be > 0");
    assert!(S <= u8::MAX as usize, "BUCKET_SIZE must fit in u8");
}

impl<K, V, const BUCKETS: usize, const BUCKET_SIZE: usize> CacheTable<K, V, BUCKETS, BUCKET_SIZE> {
    pub fn new() -> Self {
        // Compile-time parameter validation (const-eval).
        const { validate_params_const::<BUCKETS, BUCKET_SIZE>() };
        // Runtime debug checks mirror the compile-time rules for easier
        // diagnosis in debug builds and to document hot-path assumptions.
        debug_assert!(BUCKETS > 0);
        debug_assert!((BUCKETS & (BUCKETS - 1)) == 0);
        debug_assert!(BUCKET_SIZE > 0);
        debug_assert!(BUCKET_SIZE <= u8::MAX as usize);

        // SAFETY: MaybeUninit<T> does not require initialization. We zero
        // the `lengths` array so no bucket slot is ever read before being
        // written. This avoids requiring Default bounds on K/V and mirrors
        // Ghostty's `= undefined` for buckets + `= @splat(0)` for lengths.
        let storage = unsafe {
            let layout = std::alloc::Layout::new::<Storage<K, V, BUCKETS, BUCKET_SIZE>>();
            let ptr = std::alloc::alloc(layout) as *mut Storage<K, V, BUCKETS, BUCKET_SIZE>;
            if ptr.is_null() {
                std::alloc::handle_alloc_error(layout);
            }
            // Zero the lengths array. Buckets are MaybeUninit and need no init.
            ptr::write_bytes(ptr::addr_of_mut!((*ptr).lengths), 0, 1);
            Box::from_raw(ptr)
        };

        Self { storage }
    }

    /// Insert `(key, value)` into the table using a pre-computed `hash`.
    ///
    /// If the target bucket has space, the entry is appended and `None` is
    /// returned. If the bucket is full, the oldest entry is evicted and
    /// returned — exactly matching Ghostty's `rotateIn` semantics.
    pub fn put(&mut self, hash: u64, key: K, value: V) -> Option<(K, V)> {
        let idx = (hash as usize) & (BUCKETS - 1);
        let s = &mut *self.storage;
        let len = s.lengths[idx] as usize;
        let bucket = &mut s.buckets[idx];

        if len < BUCKET_SIZE {
            // Bucket has room — just append.
            bucket[len] = MaybeUninit::new(Entry { key, value });
            s.lengths[idx] += 1;
            return None;
        }

        // Bucket full — rotate oldest out.
        // Equivalent to Ghostty's `fastmem.rotateIn`:
        //   1. Read out slot[0] (the oldest entry).
        //   2. Memmove slots[1..] down to slots[0..].
        //   3. Write the new entry into the last slot.

        // SAFETY: len == BUCKET_SIZE, so all slots 0..BUCKET_SIZE are
        // initialized. We read slot[0] via ptr::read (taking ownership),
        // shift remaining entries down via ptr::copy (memmove semantics
        // — handles overlapping regions), and write the new entry into
        // the last position. After this, all slots are still initialized.
        unsafe {
            let base = bucket.as_mut_ptr() as *mut Entry<K, V>;
            let evicted = ptr::read(base);
            ptr::copy(base.add(1), base, BUCKET_SIZE - 1);
            ptr::write(base.add(BUCKET_SIZE - 1), Entry { key, value });
            Some((evicted.key, evicted.value))
        }
    }

    /// Look up an entry by `hash` and equality predicate `eq`.
    ///
    /// On hit the entry is promoted to most-recent within its bucket
    /// (mirrors Ghostty's `fastmem.rotateOnce` on the tail slice).
    /// Returns `None` on a miss.
    pub fn get<F>(&mut self, hash: u64, eq: F) -> Option<&V>
    where
        F: Fn(&K) -> bool,
    {
        let idx = (hash as usize) & (BUCKETS - 1);
        let s = &mut *self.storage;
        let len = s.lengths[idx] as usize;
        let bucket = &mut s.buckets[idx];

        // Reverse scan — most-recent entries are at the end, so scanning
        // backwards finds hot entries faster (same order as Ghostty).
        let mut i = len;
        while i > 0 {
            i -= 1;
            // SAFETY: i < len, and all slots 0..len are initialized per
            // the length invariant.
            let entry = unsafe { bucket[i].assume_init_ref() };
            if eq(&entry.key) {
                // Promote to most-recent position via rotateOnce.
                // Mirrors Ghostty `fastmem.rotateOnce(KV, self.buckets[idx][i..len])`:
                //   1. Read out the hit entry at slot[i].
                //   2. Memmove slots[i+1..len] down to slots[i..].
                //   3. Write the hit entry into slot[len-1].
                if i < len - 1 {
                    // SAFETY: slots i..len are initialized. We read slot[i]
                    // (taking ownership of the bitwise value), shift
                    // slots[i+1..len] down by one (ptr::copy handles
                    // overlap), and write the hit entry into slot[len-1].
                    // All slots i..len remain initialized afterward.
                    unsafe {
                        let base = bucket.as_mut_ptr() as *mut Entry<K, V>;
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

    /// Remove all entries. Calls `on_evict` for every removed entry
    /// (mirrors Ghostty's optional `evicted` callback on Context).
    ///
    /// Entries are drained in reverse order with the length decremented
    /// **before** each callback, so a panic in `on_evict` leaves the
    /// table in a consistent state (no double-drop on the panicked slot).
    pub fn clear<F>(&mut self, mut on_evict: F)
    where
        F: FnMut(K, V),
    {
        let s = &mut *self.storage;
        for (bucket, len) in s.buckets.iter_mut().zip(s.lengths.iter_mut()) {
            while *len > 0 {
                *len -= 1;
                // SAFETY: we just decremented len, so bucket[*len] is the
                // last initialized slot. assume_init_read takes ownership;
                // the slot is now logically uninitialized and len no longer
                // counts it.
                let entry = unsafe { bucket[*len as usize].assume_init_read() };
                on_evict(entry.key, entry.value);
            }
        }
    }

    /// Remove all entries without calling an eviction callback.
    ///
    /// Entries are dropped in reverse order with the length decremented
    /// before each drop, maintaining the safety invariant even if a
    /// destructor panics.
    pub fn clear_no_evict(&mut self) {
        let s = &mut *self.storage;
        for (bucket, len) in s.buckets.iter_mut().zip(s.lengths.iter_mut()) {
            while *len > 0 {
                *len -= 1;
                // SAFETY: same as clear() — slot at *len is initialized,
                // and we've already excluded it from the live range.
                unsafe { bucket[*len as usize].assume_init_drop() };
            }
        }
    }
}

impl<K, V, const BUCKETS: usize, const BUCKET_SIZE: usize> Default
    for CacheTable<K, V, BUCKETS, BUCKET_SIZE>
{
    fn default() -> Self {
        Self::new()
    }
}

impl<K, V, const BUCKETS: usize, const BUCKET_SIZE: usize> Drop
    for CacheTable<K, V, BUCKETS, BUCKET_SIZE>
{
    fn drop(&mut self) {
        // Drop all initialized entries before the Box frees the allocation.
        // Uses the same panic-safe reverse-drain pattern as clear_no_evict.
        let s = &mut *self.storage;
        for (bucket, len) in s.buckets.iter_mut().zip(s.lengths.iter_mut()) {
            while *len > 0 {
                *len -= 1;
                // SAFETY: slot at *len is initialized per the length
                // invariant. After assume_init_drop, len no longer counts
                // it, so a panic here won't cause a double-drop.
                unsafe { bucket[*len as usize].assume_init_drop() };
            }
        }
        // Box<Storage<...>> handles deallocation.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mirrors Ghostty's `test CacheTable` with a 2×2 table.
    #[test]
    fn basic_put_get_eviction() {
        // Trivial hash: the key itself. With 2 buckets the bucket
        // index is `key & 1`, so even/odd keys go to separate buckets.
        let mut t = CacheTable::<u32, u32, 2, 2>::new();

        // Fill both buckets (bucket 0: keys 0,2; bucket 1: keys 1,3).
        assert!(t.put(0, 0, 10).is_none());
        assert!(t.put(1, 1, 11).is_none());
        assert!(t.put(2, 2, 12).is_none());
        assert!(t.put(3, 3, 13).is_none());

        // Bucket 0 is full. Inserting key=4 (bucket 0) evicts key=0.
        let evicted = t.put(4, 4, 14);
        assert_eq!(evicted, Some((0, 10)));

        // key=0 is gone, key=4 is present.
        assert!(t.get(0, |k| *k == 0).is_none());
        assert_eq!(t.get(4, |k| *k == 4), Some(&14));
    }

    #[test]
    fn get_promotes_to_most_recent() {
        let mut t = CacheTable::<u32, u32, 2, 3>::new();
        t.put(0, 0, 100);
        t.put(2, 2, 200);
        t.put(4, 4, 300);
        // Bucket 0 is full: [0, 2, 4]. Access key=0 promotes it.
        assert_eq!(t.get(0, |k| *k == 0), Some(&100));
        // Now insert key=6 (bucket 0) — should evict key=2 (oldest).
        let evicted = t.put(6, 6, 400);
        assert_eq!(evicted, Some((2, 200)));
    }

    #[test]
    fn clear_calls_eviction() {
        let mut t = CacheTable::<u32, u32, 4, 2>::new();
        t.put(0, 0, 0);
        t.put(1, 1, 1);
        t.put(2, 2, 2);

        let mut evicted = Vec::new();
        t.clear(|k, v| evicted.push((k, v)));
        assert_eq!(evicted.len(), 3);

        // Table is empty after clear.
        assert!(t.get(0, |k| *k == 0).is_none());
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
            let mut t = CacheTable::<u32, Tracked, 2, 2>::new();
            t.put(0, 0, Tracked);
            t.put(1, 1, Tracked);
            t.put(2, 2, Tracked);
        }
        // 3 entries should have been dropped.
        assert_eq!(DROP_COUNT.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn eviction_drops_value() {
        use std::sync::atomic::{AtomicU32, Ordering};
        static DROP_COUNT: AtomicU32 = AtomicU32::new(0);

        struct Tracked;
        impl Drop for Tracked {
            fn drop(&mut self) {
                DROP_COUNT.fetch_add(1, Ordering::Relaxed);
            }
        }

        DROP_COUNT.store(0, Ordering::Relaxed);
        let mut t = CacheTable::<u32, Tracked, 2, 1>::new();
        t.put(0, 0, Tracked); // bucket 0, slot 0
        assert_eq!(DROP_COUNT.load(Ordering::Relaxed), 0);
        let evicted = t.put(2, 2, Tracked); // bucket 0 full, evicts (0, Tracked)
        assert!(evicted.is_some());
        drop(evicted);
        assert_eq!(DROP_COUNT.load(Ordering::Relaxed), 1);
    }
}
