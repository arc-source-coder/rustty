/// A skyline bin-packing allocator for atlas bookkeeping.
///
/// Direct port of Ghostty's `font/Atlas.zig`. The algorithm is based on
/// "A Thousand Ways to Pack the Bin" by Jukka Jylänki, via Nicolas P.
/// Rougier's freetype-gl and Jukka's C++ RectangleBinPack.
///
/// Limitations (inherited from Ghostty, easy to lift if needed):
///   - Texture is always square (width == height).
///   - Regions written *into* the atlas need not be square.
///
/// Ghostty reference:
///   `zig/ghostty/src/font/Atlas.zig`
use std::fmt;

/// Number of skyline nodes to pre-allocate.
const NODE_PREALLOC: usize = 64;

/// A reserved rectangular region within the atlas.
/// Ghostty reference: `Atlas.Region`.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct Region {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl Region {
    pub fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }
}

/// Skyline node — tracks a horizontal span of available space.
/// Ghostty reference: `Atlas.Node`.
#[derive(Clone, Copy)]
struct Node {
    x: u16,
    y: u16,
    width: u16,
}

impl Node {
    pub fn new(x: u16, y: u16, width: u16) -> Self {
        Node { x, y, width }
    }
}

/// Atlas error — the atlas is full and cannot fit the requested region.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AtlasFullError;

impl std::error::Error for AtlasFullError {}

impl fmt::Display for AtlasFullError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("atlas full")
    }
}

/// A skyline allocator for atlas bookkeeping.
pub struct AtlasAllocator {
    /// Skyline nodes (available space tracking).
    nodes: Vec<Node>,
    /// Width and height (always square).
    pub size: u16,
}

impl AtlasAllocator {
    /// Create a new atlas with the given side length and format.
    /// Ghostty reference: `Atlas.init`.
    pub fn new(size: u16) -> Self {
        let mut nodes = Vec::with_capacity(NODE_PREALLOC);
        // Initial skyline: one node spanning the usable interior
        // (1px border on all sides to avoid sampling artifacts).
        nodes.push(Node::new(1, 1, size - 2));
        Self { nodes, size }
    }

    /// Reserve a region of `width × height` pixels within the atlas.
    ///
    /// Returns `Err(AtlasFullError)` if the atlas cannot fit the region.
    /// Does **not** grow automatically — the caller must call `grow` and
    /// retry (exactly as Ghostty does in `SharedGrid.renderGlyph`).
    ///
    /// Ghostty reference: `Atlas.reserve`.
    pub fn reserve(&mut self, width: u16, height: u16) -> Result<Region, AtlasFullError> {
        // Zero-size region: return origin (simplifies callers).
        if width == 0 || height == 0 {
            return Ok(Region::new(0, 0, 0, 0));
        }

        // Best-fit search: find the skyline node that yields the lowest
        // y + height while still fitting the requested width.
        let mut region = Region::new(0, 0, width, height);
        let mut best_height = u16::MAX;
        let mut best_width = u16::MAX;
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
        let node = Node::new(region.x, region.y + height, width);
        self.nodes.insert(best_idx, node);

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

    /// Grow the atlas to `new_size`, preserving all existing pixel data
    /// and glyph coordinates.
    /// Ghostty reference: `Atlas.grow`.
    pub fn grow(&mut self, new_size: u16) {
        let old_size = self.size;
        self.size = new_size;

        // Add a new skyline node for the expanded right-hand space.
        let node = Node::new(old_size - 1, 1, new_size - old_size);
        self.nodes.push(node);
    }

    /// Clear all allocations, resetting the atlas to empty.
    ///
    /// Does not shrink the backing allocation. Existing glyph cache
    /// entries become invalid — the caller must clear those too.
    ///
    /// Ghostty reference: `Atlas.clear`.
    pub fn clear(&mut self) {
        self.nodes.clear();
        // Re-establish the initial skyline node (1px border).
        self.nodes.push(Node::new(1, 1, self.size - 2));
    }

    // ------------------------------------------------------------------
    // Internal skyline helpers
    // ------------------------------------------------------------------

    /// Check if a `width × height` rectangle fits starting at node `idx`.
    /// Returns the Y coordinate if it fits, `None` otherwise.
    ///
    /// Ghostty reference: `Atlas.fit`.
    fn fit(&self, idx: usize, width: u16, height: u16) -> Option<u16> {
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
