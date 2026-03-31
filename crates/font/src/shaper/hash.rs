use rapidhash::fast::RapidHasher;
/// Position-independent run hash builder.
///
/// Replicates Ghostty's `RunIterator.next` hashing strategy using rapidhash
/// instead of wyhash. The hash incorporates:
///
/// 1. Each codepoint and its **relative** cluster index (offset from run
///    start), making the hash position-independent within a row.
/// 2. The total run length (cell count).
/// 3. The font identity key for the run.
///
/// Ghostty reference:
///   `zig/ghostty/src/font/shaper/run.zig` — `RunIterator.next`
///   + `addCodepoint` which feeds `(cp, cluster)` pairs via `autoHash`.
use std::hash::Hasher;

/// Accumulates run content into a position-independent hash.
///
/// Usage mirrors Ghostty's inline hashing inside `RunIterator.next`:
/// ```ignore
/// let mut h = RunHasher::new();
/// for (relative_cluster, cp) in run_codepoints {
///     h.add_codepoint(cp, relative_cluster);
/// }
/// let hash = h.finish(run_length, font_key);
/// ```
pub struct RunHasher {
    /// rapidhash `fast::RapidHasher` seeded with 0 (matches Ghostty
    /// `Wyhash.init(0)`). The `'static` lifetime comes from borrowing
    /// the compiled-in `DEFAULT_RAPID_SECRETS` constant.
    inner: RapidHasher<'static>,
}

impl Default for RunHasher {
    fn default() -> Self {
        Self::new()
    }
}

impl RunHasher {
    /// Create a new hasher seeded with 0 (matches Ghostty `Wyhash.init(0)`).
    #[inline]
    pub fn new() -> Self {
        Self {
            inner: RapidHasher::new(0),
        }
    }

    /// Feed one codepoint and its cluster-relative index into the hash.
    ///
    /// Mirrors Ghostty `addCodepoint(&hasher, cp, cluster)` which calls
    /// `autoHash(hasher, cp); autoHash(hasher, cluster);`.
    #[inline]
    pub fn add_codepoint(&mut self, codepoint: u32, relative_cluster: u32) {
        self.inner.write_u32(codepoint);
        self.inner.write_u32(relative_cluster);
    }

    /// Finalize the hash by mixing in run length and font identity.
    ///
    /// Mirrors the tail of Ghostty's `RunIterator.next`:
    /// ```zig
    /// autoHash(&hasher, j - self.i);   // run length
    /// autoHash(&hasher, current_font); // font index
    /// hasher.final()
    /// ```
    #[inline]
    pub fn finish(mut self, run_length: u32, font_key: u64) -> u64 {
        self.inner.write_u32(run_length);
        self.inner.write_u64(font_key);
        self.inner.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_content_same_hash() {
        let hash_a = {
            let mut h = RunHasher::new();
            h.add_codepoint(b'h' as u32, 0);
            h.add_codepoint(b'i' as u32, 1);
            h.finish(2, 0)
        };
        let hash_b = {
            let mut h = RunHasher::new();
            h.add_codepoint(b'h' as u32, 0);
            h.add_codepoint(b'i' as u32, 1);
            h.finish(2, 0)
        };
        assert_eq!(hash_a, hash_b);
    }

    #[test]
    fn different_font_different_hash() {
        let hash_a = {
            let mut h = RunHasher::new();
            h.add_codepoint(b'h' as u32, 0);
            h.finish(1, 0)
        };
        let hash_b = {
            let mut h = RunHasher::new();
            h.add_codepoint(b'h' as u32, 0);
            h.finish(1, 1)
        };
        assert_ne!(hash_a, hash_b);
    }

    #[test]
    fn different_length_different_hash() {
        let hash_a = {
            let mut h = RunHasher::new();
            h.add_codepoint(b'a' as u32, 0);
            h.finish(1, 0)
        };
        let hash_b = {
            let mut h = RunHasher::new();
            h.add_codepoint(b'a' as u32, 0);
            h.finish(2, 0)
        };
        assert_ne!(hash_a, hash_b);
    }

    #[test]
    fn position_independent() {
        // Same codepoints at same relative clusters produce the same hash
        // regardless of any external offset — the caller is responsible for
        // passing cluster indices relative to the run start.
        let hash = |base: u32| {
            let mut h = RunHasher::new();
            h.add_codepoint(b'x' as u32, 0);
            h.add_codepoint(b'y' as u32, 1);
            let _ = base; // not fed into hash
            h.finish(2, 42)
        };
        assert_eq!(hash(0), hash(10));
        assert_eq!(hash(0), hash(100));
    }
}
