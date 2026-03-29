/// Texture atlas — skyline bin-packing allocator with CPU-owned pixel data.
///
/// Direct port of Ghostty's `font/Atlas.zig`. The algorithm is based on
/// "A Thousand Ways to Pack the Bin" by Jukka Jylänki, via Nicolas P.
/// Rougier's freetype-gl and Jukka's C++ RectangleBinPack.
///
/// Limitations (inherited from Ghostty, easy to lift if needed):
///   - Written data must be packed (no custom strides).
///   - Texture is always square (width == height).
///   - Regions written *into* the atlas need not be square.
///
/// ## Thread safety
///
/// `modified` and `resized` are atomic counters that allow a renderer on
/// another thread to detect when the GPU texture needs re-uploading or
/// re-creating. All mutation methods (`reserve`, `set`, `grow`, `clear`)
/// increment `modified`; `grow` additionally increments `resized`.
///
/// Ghostty reference:
///   `crates/ghostty-vt/zig/ghostty/src/font/Atlas.zig`
use std::sync::atomic::{AtomicU64, Ordering};

/// Initial atlas side length (matches Ghostty's `512`).
pub const INITIAL_SIZE: u32 = 512;

/// Number of skyline nodes to pre-allocate.
const NODE_PREALLOC: usize = 64;

/// Pixel format of the atlas texture data.
///
/// Ghostty reference: `Atlas.Format`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Format {
    /// 1 byte per pixel — grayscale text glyphs.
    Grayscale,
    /// 4 bytes per pixel — color emoji / color glyphs.
    Bgra,
}

impl Format {
    /// Bytes per pixel.
    #[inline]
    pub const fn depth(self) -> u32 {
        match self {
            Format::Grayscale => 1,
            Format::Bgra => 4,
        }
    }
}

/// A reserved rectangular region within the atlas.
///
/// Ghostty reference: `Atlas.Region`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Region {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Skyline node — tracks a horizontal span of available space.
///
/// Ghostty reference: `Atlas.Node`.
#[derive(Clone, Copy, Debug)]
struct Node {
    x: u32,
    y: u32,
    width: u32,
}

/// Atlas error — the atlas is full and cannot fit the requested region.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AtlasFullError;

impl std::fmt::Display for AtlasFullError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "atlas is full")
    }
}

impl std::error::Error for AtlasFullError {}

/// A texture atlas that owns CPU-side pixel data and a skyline allocator.
///
/// The renderer mirrors this data into a GPU texture, gated on `modified`
/// and `resized` counters.
pub struct Atlas {
    /// Raw texture data. Length = `size * size * format.depth()`.
    data: Box<[u8]>,
    /// Width and height (always square).
    size: u32,
    /// Skyline nodes (available space tracking).
    nodes: Vec<Node>,
    /// Pixel format.
    format: Format,
    /// Incremented on every data mutation. Renderer uses this to detect
    /// when the GPU texture contents need re-uploading.
    pub modified: AtomicU64,
    /// Incremented on every resize. Renderer uses this to detect when the
    /// GPU texture needs to be re-created at a larger size.
    pub resized: AtomicU64,
}

impl Atlas {
    /// Create a new atlas with the given side length and format.
    ///
    /// Ghostty reference: `Atlas.init`.
    pub fn new(size: u32, format: Format) -> Self {
        let depth = format.depth();
        let data = vec![0u8; (size * size * depth) as usize].into_boxed_slice();
        let mut nodes = Vec::with_capacity(NODE_PREALLOC);
        // Initial skyline: one node spanning the usable interior
        // (1px border on all sides to avoid sampling artifacts).
        nodes.push(Node {
            x: 1,
            y: 1,
            width: size - 2,
        });
        Self {
            data,
            size,
            nodes,
            format,
            modified: AtomicU64::new(0),
            resized: AtomicU64::new(0),
        }
    }

    /// Current side length (width == height).
    #[inline]
    pub fn size(&self) -> u32 {
        self.size
    }

    /// Pixel format.
    #[inline]
    pub fn format(&self) -> Format {
        self.format
    }

    /// Raw pixel data slice. Length = `size * size * format.depth()`.
    #[inline]
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Reserve a region of `width × height` pixels within the atlas.
    ///
    /// Returns `Err(AtlasFullError)` if the atlas cannot fit the region.
    /// Does **not** grow automatically — the caller must call `grow` and
    /// retry (exactly as Ghostty does in `SharedGrid.renderGlyph`).
    ///
    /// Ghostty reference: `Atlas.reserve`.
    pub fn reserve(&mut self, width: u32, height: u32) -> Result<Region, AtlasFullError> {
        // Zero-size region: return origin (simplifies callers).
        if width == 0 && height == 0 {
            return Ok(Region {
                x: 0,
                y: 0,
                width: 0,
                height: 0,
            });
        }

        // Best-fit search: find the skyline node that yields the lowest
        // y + height while still fitting the requested width.
        let mut region = Region {
            x: 0,
            y: 0,
            width,
            height,
        };
        let mut best_height: u32 = u32::MAX;
        let mut best_width: u32 = u32::MAX;
        let mut chosen: Option<usize> = None;

        for i in 0..self.nodes.len() {
            let Some(y) = self.fit(i, width, height) else {
                continue;
            };
            let node = self.nodes[i];
            if (y + height) < best_height
                || ((y + height) == best_height && node.width > 0 && node.width < best_width)
            {
                chosen = Some(i);
                best_width = node.width;
                best_height = y + height;
                region.x = node.x;
                region.y = y;
            }
        }

        let best_idx = chosen.ok_or(AtlasFullError)?;

        // Insert a new node for the placed rectangle.
        self.nodes.insert(
            best_idx,
            Node {
                x: region.x,
                y: region.y + height,
                width,
            },
        );

        // Shrink or remove subsequent overlapping nodes.
        let i = best_idx + 1;
        while i < self.nodes.len() {
            let prev_end = self.nodes[i - 1].x + self.nodes[i - 1].width;
            if self.nodes[i].x < prev_end {
                let shrink = prev_end - self.nodes[i].x;
                self.nodes[i].x += shrink;
                self.nodes[i].width = self.nodes[i].width.saturating_sub(shrink);
                if self.nodes[i].width == 0 {
                    self.nodes.remove(i);
                    continue;
                }
            }
            break;
        }

        self.merge();
        Ok(region)
    }

    /// Write packed pixel data into a previously reserved region.
    ///
    /// The data length must equal `region.width * region.height * depth`.
    ///
    /// Ghostty reference: `Atlas.set`.
    pub fn set(&mut self, reg: Region, data: &[u8]) {
        debug_assert!(reg.x < self.size - 1);
        debug_assert!(reg.x + reg.width < self.size);
        debug_assert!(reg.y < self.size - 1);
        debug_assert!(reg.y + reg.height < self.size);

        let depth = self.format.depth();
        for row in 0..reg.height {
            let tex_offset = (((reg.y + row) * self.size) + reg.x) * depth;
            let data_offset = row * reg.width * depth;
            let row_bytes = reg.width * depth;
            self.data[tex_offset as usize..(tex_offset + row_bytes) as usize]
                .copy_from_slice(&data[data_offset as usize..(data_offset + row_bytes) as usize]);
        }

        self.modified.fetch_add(1, Ordering::Relaxed);
    }

    /// Write a sub-rectangle from a larger source buffer.
    ///
    /// Ghostty reference: `Atlas.setFromLarger`.
    pub fn set_from_larger(
        &mut self,
        reg: Region,
        src: &[u8],
        src_width: u32,
        src_x: u32,
        src_y: u32,
    ) {
        debug_assert!(reg.x < self.size - 1);
        debug_assert!(reg.x + reg.width < self.size);
        debug_assert!(reg.y < self.size - 1);
        debug_assert!(reg.y + reg.height < self.size);

        let depth = self.format.depth();
        for row in 0..reg.height {
            let tex_offset = (((reg.y + row) * self.size) + reg.x) * depth;
            let src_offset = (((src_y + row) * src_width) + src_x) * depth;
            let row_bytes = reg.width * depth;
            self.data[tex_offset as usize..(tex_offset + row_bytes) as usize]
                .copy_from_slice(&src[src_offset as usize..(src_offset + row_bytes) as usize]);
        }

        self.modified.fetch_add(1, Ordering::Relaxed);
    }

    /// Grow the atlas to `new_size`, preserving all existing pixel data
    /// and glyph coordinates.
    ///
    /// Ghostty reference: `Atlas.grow`.
    pub fn grow(&mut self, new_size: u32) {
        debug_assert!(new_size >= self.size);
        if new_size == self.size {
            return;
        }

        let depth = self.format.depth();
        let old_data = std::mem::take(&mut self.data);
        let old_size = self.size;

        self.size = new_size;
        self.data = vec![0u8; (new_size * new_size * depth) as usize].into_boxed_slice();

        // Copy old pixel data back. Skip the first and last border rows
        // (Ghostty: `.y = 1, .height = size_old - 2`).
        self.set(
            Region {
                x: 0,
                y: 1,
                width: old_size,
                height: old_size - 2,
            },
            &old_data[(old_size * depth) as usize..],
        );

        // Add a new skyline node for the expanded right-hand space.
        self.nodes.push(Node {
            x: old_size - 1,
            y: 1,
            width: new_size - old_size,
        });

        // `set` above already incremented `modified` once; we add the
        // resize signal on top.
        self.resized.fetch_add(1, Ordering::Relaxed);
    }

    /// Clear all allocations, resetting the atlas to empty.
    ///
    /// Does not shrink the backing allocation. Existing glyph cache
    /// entries become invalid — the caller must clear those too.
    ///
    /// Ghostty reference: `Atlas.clear`.
    pub fn clear(&mut self) {
        self.modified.fetch_add(1, Ordering::Relaxed);
        self.data.fill(0);
        self.nodes.clear();
        // Re-establish the initial skyline node (1px border).
        self.nodes.push(Node {
            x: 1,
            y: 1,
            width: self.size - 2,
        });
    }

    // ------------------------------------------------------------------
    // Internal skyline helpers
    // ------------------------------------------------------------------

    /// Check if a `width × height` rectangle fits starting at node `idx`.
    /// Returns the Y coordinate if it fits, `None` otherwise.
    ///
    /// Ghostty reference: `Atlas.fit`.
    fn fit(&self, idx: usize, width: u32, height: u32) -> Option<u32> {
        let node = self.nodes[idx];
        // Would the right edge exceed the usable area?
        if node.x + width > self.size - 1 {
            return None;
        }

        let mut y = node.y;
        let mut i = idx;
        let mut width_left = width;
        while width_left > 0 {
            let n = self.nodes[i];
            if n.y > y {
                y = n.y;
            }
            // Would the bottom edge exceed the usable area?
            if y + height > self.size - 1 {
                return None;
            }
            width_left = width_left.saturating_sub(n.width);
            i += 1;
        }

        Some(y)
    }

    /// Merge adjacent nodes with the same Y value.
    ///
    /// Ghostty reference: `Atlas.merge`.
    fn merge(&mut self) {
        let mut i = 0;
        while i + 1 < self.nodes.len() {
            if self.nodes[i].y == self.nodes[i + 1].y {
                self.nodes[i].width += self.nodes[i + 1].width;
                self.nodes.remove(i + 1);
            } else {
                i += 1;
            }
        }
    }
}

// ------------------------------------------------------------------
// Tests — ported from Ghostty Atlas.zig tests
// ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserve_and_set_grayscale() {
        let mut atlas = Atlas::new(32, Format::Grayscale);
        let reg = atlas.reserve(2, 2).unwrap();
        assert_eq!(reg.x, 1);
        assert_eq!(reg.y, 1);
        atlas.set(reg, &[1, 2, 3, 4]);
        let off = |x: u32, y: u32| (y * atlas.size() + x) as usize;
        assert_eq!(atlas.data()[off(1, 1)], 1);
        assert_eq!(atlas.data()[off(2, 1)], 2);
        assert_eq!(atlas.data()[off(1, 2)], 3);
        assert_eq!(atlas.data()[off(2, 2)], 4);
    }

    #[test]
    fn reserve_too_large_fails() {
        let mut atlas = Atlas::new(32, Format::Grayscale);
        let result = atlas.reserve(31, 31);
        assert!(result.is_err());
    }

    #[test]
    fn zero_size_reserve() {
        let mut atlas = Atlas::new(32, Format::Grayscale);
        let reg = atlas.reserve(0, 0).unwrap();
        assert_eq!(reg.width, 0);
        assert_eq!(reg.height, 0);
    }

    #[test]
    fn full_then_fail() {
        // 4x4 atlas → usable area is 2x2 (1px border).
        let mut atlas = Atlas::new(4, Format::Grayscale);
        let _r = atlas.reserve(2, 2).unwrap();
        let result = atlas.reserve(1, 1);
        assert!(result.is_err());
    }

    #[test]
    fn grow_preserves_data() {
        let mut atlas = Atlas::new(4, Format::Grayscale);
        let reg = atlas.reserve(2, 2).unwrap();
        atlas.set(reg, &[1, 2, 3, 4]);

        // Verify data before grow.
        assert_eq!(atlas.data()[(atlas.size() + 1) as usize], 1);
        assert_eq!(atlas.data()[(atlas.size() + 2) as usize], 2);

        let old_modified = atlas.modified.load(Ordering::Relaxed);
        let old_resized = atlas.resized.load(Ordering::Relaxed);

        atlas.grow(atlas.size() + 1);

        assert!(atlas.modified.load(Ordering::Relaxed) > old_modified);
        assert!(atlas.resized.load(Ordering::Relaxed) > old_resized);

        // Data should be in same place accounting for new size.
        assert_eq!(atlas.data()[(atlas.size() + 1) as usize], 1);
        assert_eq!(atlas.data()[(atlas.size() + 2) as usize], 2);
        assert_eq!(atlas.data()[(atlas.size() * 2 + 1) as usize], 3);
        assert_eq!(atlas.data()[(atlas.size() * 2 + 2) as usize], 4);

        // Should now fit a new 1x1 region.
        atlas.reserve(1, 1).unwrap();
    }

    #[test]
    fn grow_bgra_preserves_data() {
        let mut atlas = Atlas::new(4, Format::Bgra);
        let reg = atlas.reserve(2, 2).unwrap();
        // 2x2 region, 4 bpp = 16 bytes.
        #[rustfmt::skip]
        atlas.set(reg, &[
            10, 11, 12, 13,  14, 15, 16, 17,
            20, 21, 22, 23,  24, 25, 26, 27,
        ]);

        let depth = atlas.format().depth() as usize;
        let tl = (atlas.size() as usize * depth) + depth;
        assert_eq!(atlas.data()[tl], 10);
        assert_eq!(atlas.data()[tl + 4], 14);

        atlas.grow(atlas.size() + 1);

        let tl = (atlas.size() as usize * depth) + depth;
        assert_eq!(atlas.data()[tl], 10);
        assert_eq!(atlas.data()[tl + 4], 14);
        let row2 = tl + (atlas.size() as usize * depth);
        assert_eq!(atlas.data()[row2], 20);
        assert_eq!(atlas.data()[row2 + 4], 24);
    }

    #[test]
    fn clear_resets_state() {
        let mut atlas = Atlas::new(32, Format::Grayscale);
        let reg = atlas.reserve(4, 4).unwrap();
        atlas.set(reg, &[1u8; 16]);
        atlas.clear();
        // Should be able to allocate from scratch again.
        atlas.reserve(30, 30).unwrap();
    }

    #[test]
    fn modified_increments_on_set() {
        let mut atlas = Atlas::new(32, Format::Grayscale);
        let reg = atlas.reserve(1, 1).unwrap();
        let before = atlas.modified.load(Ordering::Relaxed);
        atlas.set(reg, &[255]);
        assert!(atlas.modified.load(Ordering::Relaxed) > before);
    }

    #[test]
    fn set_from_larger() {
        let mut atlas = Atlas::new(32, Format::Grayscale);
        let reg = atlas.reserve(2, 2).unwrap();
        // Source is 5 wide; we copy a 2x2 sub-rect starting at (1, 1).
        #[rustfmt::skip]
        let src = [
            0, 0, 0, 0, 0,
            0, 1, 2, 0, 0,
            0, 3, 4, 0, 0,
            0, 0, 0, 0, 0,
        ];
        let old = atlas.modified.load(Ordering::Relaxed);
        atlas.set_from_larger(reg, &src, 5, 1, 1);
        assert!(atlas.modified.load(Ordering::Relaxed) > old);

        let off = |x: u32, y: u32| (y * atlas.size() + x) as usize;
        assert_eq!(atlas.data()[off(reg.x, reg.y)], 1);
        assert_eq!(atlas.data()[off(reg.x + 1, reg.y)], 2);
        assert_eq!(atlas.data()[off(reg.x, reg.y + 1)], 3);
        assert_eq!(atlas.data()[off(reg.x + 1, reg.y + 1)], 4);
    }
}
