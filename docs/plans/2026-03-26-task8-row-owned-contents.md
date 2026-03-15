# Task 8: Row-Owned Persistent Contents + Shaper Rework

**Goal:** Replace the splice-based ephemeral scene model with Ghostty's row-owned persistent contents model, cursor lanes, direct mapped GPU upload from row lanes (Ghostty `syncFromArrayLists` style), and a Ghostty-aligned Shaper — eliminating all O(n) splice/offset fixup pathology, clone-on-cache-hit, RefCell hook plumbing, per-row scratch allocations, and the `RunIteratorHook` trait.

**Architecture:** Port Ghostty's `cell.zig::Contents` data model into `scene.rs`. Each row owns its own `Vec<QuadInstance>` via an `FgRows` collection (Ghostty's `ArrayListCollection`). Two dedicated cursor lanes (first + last) control z-order. Dirty rows are cleared and rebuilt in-place. At draw time, the D3D11 backend maps the instance buffer (`D3D11_MAP_WRITE_DISCARD`) and sequentially memcpy's lane-by-lane directly from `fg_rows.lists` (Ghostty `syncFromArrayLists` parity), with no intermediate flattened CPU `Vec<QuadInstance>`. The `Shaper` struct in the font crate mirrors Ghostty's CoreText Shaper — owning codepoints + UTF-16 scratch, with a concrete `ShaperHook` (raw-pointer, NOT a trait) that writes into the Shaper's buffers during run iteration. `RowScratch` is eliminated entirely (matching Ghostty, which has no equivalent). `RendererFrontend` is eliminated — the renderer struct owns everything directly (matching Ghostty's `generic.zig::Renderer`).

**Scope:** The QuadInstance → CellText vertex format change is NOT included — that requires shader + backend + input layout changes and belongs in Task 9 (unified draw path). This task *does* include a small backend API change so D3D11 can upload directly from row lists.

**Tech Stack:** Rust, D3D11, DirectWrite, `crates/font`, `crates/renderer`, `crates/ghostty-vt`

---

## Key Design Decisions

### 1. QuadInstance stays (CellText deferred to Task 9)

Ghostty's `CellText` stores `grid_pos: [2]u16` and the shader converts to pixels via uniforms. Our `QuadInstance` stores pre-computed pixel `pos: [i16; 2]`. Switching requires changing the HLSL shader, D3D11 input layout, and every QuadInstance creation site — a separate concern from the data ownership rework. Contents works with either vertex format. Deferring keeps this task focused on the data model.

### 2. Shaper struct mirrors Ghostty's CoreText ownership

CoreText is the closer reference for DWrite (both are platform-native APIs that take UTF-16 input).

Ghostty's CoreText `Shaper` owns:
- `run_state: RunState` containing `codepoints: ArrayListUnmanaged(Codepoint)` + `unichars: ArrayListUnmanaged(u16)`
- `cell_buf: ArrayListUnmanaged(Cell)` — shaped output storage
- `features` / `features_no_default` — parsed once at init, reused forever

The `RunIteratorHook` is a concrete struct nested inside the Shaper — NOT a trait/interface. It holds `shaper: *Shaper` (raw pointer). During `RunIterator.next()`, `hook.addCodepoint()` pushes into `shaper.run_state.codepoints` AND builds `shaper.run_state.unichars` incrementally (encoding surrogate pairs inline).

`shape()` takes only the `TextRun` — all other state (features, buffers) is already owned by the Shaper from init.

Our DWrite equivalent: `DWriteAnalyzer` owns output+scratch arenas. It takes `&[u16]` as input and returns `ShapedCells<'a>` borrowing from its arena. The analyzer already manages its own cell output — we don't need a separate `cell_buf`.

The `Shaper` wraps `DWriteAnalyzer` and adds the missing pieces:
- `codepoints: Vec<Codepoint>` — filled by hook (CoreText: `RunState.codepoints`)
- `utf16_buf: Vec<u16>` — built incrementally by hook (CoreText: `RunState.unichars`)
- `analyzer: DWriteAnalyzer` — the actual shaping backend

This eliminates: `RunCodepointHook`, `RefCell<Vec<RunCodepoint>>`, per-run `Vec<u16>` allocation, the `runs.push((run, run_cp.borrow().clone()))` pattern, and the `RunIteratorHook` trait entirely.

### 3. RunIteratorHook is NOT a trait

Ghostty's `RunIteratorHook` is a concrete struct nested inside each Shaper backend. The `RunIterator` in `run.zig` has a field `hooks: font.Shaper.RunIteratorHook` — it's the comptime-selected concrete type, not a vtable/interface. The RunIterator is monomorphic at compile time.

Since we have exactly one backend (DWrite), the trait adds no value. Replace with a concrete `ShaperHook` struct that holds raw pointers to the Shaper's buffers (matching Ghostty's `shaper: *Shaper` pattern). The `RunIterator` becomes non-generic.

### 4. No RowScratch (matching Ghostty)

Ghostty has **no** RowScratch equivalent. Colors are resolved inline per-cell in `rebuildRow`. Graphemes come directly from the RenderState as borrowed slices.

We eliminate `RowScratch` entirely:
- `fg_by_col: Vec<Color32>` → resolve fg/bg colors inline at the point of use (matching Ghostty exactly — see §8)
- `graphemes: Vec<Option<Vec<u32>>>` → expose row-level grapheme slice from Zig FFI (see §5)
- `instances: Vec<QuadInstance>` → replaced by `Contents.add(y, instance)` pushing directly into row-owned storage

### 5. Row-level grapheme FFI (zero-copy, zero-allocation)

**Problem:** Current code calls `frame.cell_grapheme(row, col)` per-cell, then `.to_vec()`s each grapheme into `RowScratch.graphemes`. This is unnecessary — the data is already in Zig memory.

**Solution:** Expose a new `row_graphemes(row)` FFI function that returns a pointer to the entire row's grapheme SoA column — `items(.grapheme)`. This is a contiguous array of Zig slices (`[]const u21`), where each slice is `{ ptr: [*]const u21, len: usize }`. Since `u21` is guaranteed `u32` in memory (comptime asserts verify `@sizeOf(u21) == @sizeOf(u32)` and `@alignOf(u21) == @alignOf(u32)`), each element is `{ *const u32, usize }`.

On the Rust side, keep the ABI mirror as an internal `#[repr(C)] struct GraphemeSlice { ptr: *const u32, len: usize }` and cast the returned pointer to `&[GraphemeSlice]`. Convert to `&[u32]` lazily using `slice::from_raw_parts(ptr, len)` only when `raw.has_grapheme()` is true.

This gives `RunIterator` direct access to graphemes — zero allocation, zero copy, matching Ghostty's `cells_slice.items(.grapheme)` pattern exactly.

**Hot-path access rule (from review):** do not pre-populate any per-cell grapheme structure in Rust. Keep a row-level borrowed slice and only read `graphemes[x]` when `raw.has_grapheme()` is true. This mirrors Ghostty's access pattern.

**Implementation:** Add `ghostty_vt_terminal_render_row_graphemes(ptr, row, out_len) -> ?[*]const GraphemeSlice` to `render.zig`. The old per-cell `cell_grapheme` function can remain for other callers.

### 6. No RendererFrontend — renderer owns everything (matching Ghostty)

Ghostty's `generic.zig::Renderer(GraphicsAPI)` **IS** the entire renderer — there's no separate "frontend". It owns `cells: Contents`, `font_shaper: Shaper`, `font_shaper_cache: ShaperCache`, `font_grid: *SharedGrid`, `api: GraphicsAPI`, etc. `rebuildCells`, `rebuildRow`, `addGlyph` are all methods on this same struct.

Our `RendererFrontend` is unnecessary separation. Dissolve it — move its fields into the renderer struct that currently calls it. The `build_batch` method becomes a method on the renderer, and `rebuild_row` / `add_glyph` become free functions for borrow-splitting (matching Ghostty's `self: *Self` pattern in Rust idiom).

### 7. Free functions for borrow splitting

Ghostty uses `self: *Self` (raw pointer, no borrow checker). In Rust, `rebuild_row` needs `&mut contents`, `&mut shaper`, `&mut shaper_cache`, `&shared_grid`, `&config`, etc. simultaneously. We use free functions that take disjoint field references — idiomatic Rust for this pattern.

### 8. No fg_colors Vec — resolve colors inline per-cell

Ghostty resolves fg/bg colors **inline per-cell** inside `rebuildRow` (generic.zig lines 2788-2916). There is no pre-computed per-column color array. The fg color goes directly to `addGlyph` as a parameter. The bg color goes directly to `contents.bgCell(y, x)`.

For Task 8, we remove `fg_by_col` and keep color resolution inline in the row loop, but defer full Ghostty color-parity equations to the Color FFI rework (see Future Optimizations).

### 9. Zero-clone shaping pipeline

Current: `shaped_cache.get()` → `Option<&[Cell]>` → `.to_vec()` (clone on every hit!)
Ghostty: `cache.get()` returns borrowed `?[]const Cell`, used directly.

With Contents, shaped cells are iterated once to push `QuadInstance` into row storage, then discarded. The borrowed slice from cache or from `shaper.shape()` is sufficient — no clone ever needed.

### 10. ShapeOptions → init-time features (matching Ghostty)

Ghostty's `shape.Options` (containing `features: []const []const u8`) is passed at `Shaper.init()` time — NOT per-shape-call. CoreText parses features once in `init()` and builds cached CF dictionaries reused forever. HarfBuzz does the same.

Our `ShapeOptions { locale, font_size, cell_width, variant, features }` is passed per `shape()` call. Move features/locale/font_size into `Shaper::new()` (or a `reconfigure()` method). `shape()` should take only `(run, face)` — matching Ghostty exactly.

### 11. Color32 stays

`Color32` is a clean u32 newtype. No reason to change it now — it will be addressed when `ColorRGB` becomes a u32 newtype after the FFI rework.

---

## Naming Alignment (Ghostty → Rustty)

| Ghostty | Current Rustty | New Rustty |
|---------|---------------|------------|
| `Contents` | `RendererModel` + `RowScratch` | `Contents` |
| `Contents.bg_cells` | `RenderBatch.bg_cells_rgba` | `Contents.bg_cells` |
| `Contents.fg_rows` | `RendererModel.rows: Vec<RowSpan>` + global `Vec<QuadInstance>` | `Contents.fg_rows: FgRows` |
| `Contents.size` | `RendererModel.{rows, cols}` | `Contents.size: GridSize` |
| `Contents.resize()` | ad-hoc in `build_batch` | `Contents.resize()` |
| `Contents.reset()` | ad-hoc `clear()` calls | `Contents.reset()` |
| `Contents.clear(y)` | N/A (splice) | `Contents.clear(y)` |
| `Contents.add(.text, cell)` | `scratch.instances.push(instance)` | `Contents.add(y, instance)` |
| `Contents.setCursor()` | cursor push/pop at end of `instances` | `Contents.set_cursor()` |
| `Contents.bgCell(row, col)` | index math in `rebuild_row_into_scratch` | `Contents.bg_cell(row, col)` |
| `Shaper` (Ghostty renderer field) | `analyzer` + `RunCodepointHook` | `Shaper` (font crate) |
| `Shaper.RunIteratorHook` (concrete struct) | `RunIteratorHook` (trait) + `RunCodepointHook` | `ShaperHook` (concrete, raw-ptr) |
| `Shaper.RunState` (coretext) | N/A | `Shaper.{codepoints, utf16_buf}` |
| `font_shaper_cache` | `shaped_cache` | `shaper_cache` |
| `Shaper.runIterator()` | `RunIterator::with_hooks(opts, hook)` | `shaper.run_iterator(opts)` |
| `Shaper.shape(run)` | `shape_run_cached()` (free fn) | `shaper.shape(run, face)` / `shape_run_cached()` |
| `Shaper.codepoints` | `RefCell<Vec<RunCodepoint>>` | `Shaper.codepoints: Vec<Codepoint>` |
| `Shaper.RunState.unichars` (coretext) | per-run `Vec<u16>` in `shape_run_cached` | `Shaper.utf16_buf: Vec<u16>` |
| `rebuildCells` | `build_batch` (outer) | `build_batch` (method on renderer) |
| `rebuildRow` | `rebuild_row_into_scratch` | `rebuild_row` (free fn) |
| `addGlyph` | `emit_shaped_cells` (loop body) | `add_glyph` (free fn) |

## Deleted Structures

| Structure | Why |
|-----------|-----|
| `RendererFrontend` | Dissolved into renderer struct (Ghostty has no equivalent) |
| `RendererModel` | Absorbed into `Contents` |
| `RowSpan` | Replaced by row-owned lanes |
| `RowScratch` | Eliminated entirely (Ghostty has no equivalent) |
| `RunCodepoint` | Replaced by `Shaper.codepoints: Vec<Codepoint>` |
| `RunCodepointHook` | Replaced by concrete `ShaperHook` with raw pointers |
| `RunIteratorHook` (trait) | Replaced by concrete `ShaperHook` struct |

## Architecture Diagram

```
  RenderFrame (from VT)
       │
       ▼
  Renderer::build_batch()                       [was RendererFrontend]
       │
       ├─► for each dirty row y:
       │     contents.clear(y)
       │     rebuild_row(y, ...)                [free fn, disjoint field refs]
       │       │
       │       ├─► for each cell x:
       │       │     resolve fg/bg colors INLINE (no fg_by_col, matching Ghostty)
       │       │     *contents.bg_cell(y, x) = bg_color
       │       │
       │       ├─► shaper.run_iterator(opts).next()
       │       │     └─► ShaperHook (raw ptr) pushes to
       │       │           shaper.codepoints + shaper.utf16_buf
       │       │
       │       ├─► shape_run_cached()
       │       │     cache hit  → &[Cell] (borrowed, zero-copy)
       │       │     cache miss → shaper.shape(run, face) → &[Cell]
       │       │                  cache.put(hash, cells) (one copy)
       │       │
       │       └─► add_glyph(fg, ...) → contents.add(y, QuadInstance)
       │
       ├─► contents.set_cursor(cursor_quad, style)
       │
       └─► draw() sync (D3D11 backend)
             Map(instance_buffer, WRITE_DISCARD)
             memcpy lane 0 (cursor-first)  ──► mapped ptr
             memcpy lane 1..N (text rows)  ──► mapped ptr
             memcpy lane N+1 (cursor-last) ──► mapped ptr
             Unmap(instance_buffer)
             
             if bg_full_upload:
               batch.bg_cells_rgba = contents.bg_cells.clone()
             else:
               copy only dirty rows
```

---

## Task 1: Row-level grapheme FFI

**Files:**
- Modify: `crates/ghostty-vt/zig/render.zig` (add `row_graphemes` export)
- Modify: `crates/ghostty-vt/src/lib.rs` (add extern declaration)
- Modify: `crates/ghostty-vt/src/terminal.rs` (add `row_graphemes` wrapper)

### Rationale

Ghostty's `RunIterator` accesses graphemes via `cells_slice.items(.grapheme)` — a single SoA column access returning `[]const []const u21` (the entire row's graphemes as a contiguous array of fat pointers). Each `[]const u21` is `{ ptr: [*]const u21, len: usize }` and since `u21 == u32` in memory (comptime asserts), each element is `{ *const u32, usize }`.

Currently we call `cell_grapheme(row, col)` per-cell then `.to_vec()` each one. Instead, expose the entire column array in one FFI call.

### Step 1: Zig export

```zig
pub const GraphemeSlice = extern struct {
    ptr: ?[*]const u32,
    len: usize,
};

/// Returns a direct pointer into the grapheme SoA column for a row.
/// Each element is a Zig slice []const u21 = { ptr: [*]const u21, len: usize }.
/// Since @sizeOf(u21) == @sizeOf(u32), ptr can be read as [*]const u32.
///
/// For cells without graphemes (content_tag != codepoint_grapheme), the
/// slice is undefined — caller must check the raw cell's content_tag.
///
/// Zero-copy: the pointer is into persistent Zig memory.
/// Valid until the next render_update() call.
export fn ghostty_vt_terminal_render_row_graphemes(
    ptr: ?*anyopaque,
    row: u16,
    out_len: ?*u16,
) callconv(.c) ?[*]const GraphemeSlice {
    if (ptr == null) return null;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    if (row >= handle.render_state.rows) return null;

    const cells = handle.render_state.row_data.items(.cells)[row];
    const graphemes = cells.items(.grapheme);
    if (out_len) |len| len.* = @intCast(graphemes.len);
    return @ptrCast(graphemes.ptr);
}
```

**Note on ABI:** Zig `[]const u21` is `{ [*]const u21, usize }` — same layout as `GraphemeSlice` (`{ ?[*]const u32, usize }`) because u21 == u32 in memory. The `@ptrCast` from `[*]const []const u21` to `[*]const GraphemeSlice` is valid because the layouts match. Add comptime asserts:

```zig
std.debug.assert(@sizeOf([]const u21) == @sizeOf(GraphemeSlice));
std.debug.assert(@alignOf([]const u21) == @alignOf(GraphemeSlice));
```

### Step 2: Rust types and wrapper

```rust
// crates/ghostty-vt/src/terminal.rs

/// FFI-compatible mirror of Zig `[]const u21`.
/// Since @sizeOf(u21) == @sizeOf(u32), ptr points to u32 values.
#[repr(C)]
pub struct GraphemeSlice {
    pub ptr: *const u32,
    pub len: usize,
}

impl GraphemeSlice {
    /// Convert to a Rust slice, returning None if ptr is null or len is 0.
    ///
    /// SAFETY: Caller must ensure ptr is valid for len elements.
    pub unsafe fn as_slice(&self) -> Option<&[u32]> {
        if self.ptr.is_null() || self.len == 0 {
            None
        } else {
            Some(std::slice::from_raw_parts(self.ptr, self.len))
        }
    }
}

// In RenderFrame impl:
pub fn row_graphemes(&self, row: u16) -> Option<&[GraphemeSlice]> {
    let mut len: u16 = 0;
    let ptr = unsafe {
        ghostty_vt_terminal_render_row_graphemes(self.handle, row, &mut len)
    };
    if ptr.is_null() || len == 0 {
        return None;
    }
    Some(unsafe { std::slice::from_raw_parts(ptr, len as usize) })
}
```

### Step 3: Update RowCells to use row-level graphemes

Change `RowCells.graphemes` from `&'a [Option<&'a [u32]>]` to `&'a [GraphemeSlice]`, and update `RunIterator` to read graphemes directly from the `GraphemeSlice` array.

Do this **inline** in the run loop (Ghostty-style), not via a separate `grapheme_at` helper:

```rust
let cps: &[u32] = if raw.has_grapheme() {
    graphemes
        .get(x)
        .and_then(|s| unsafe { s.as_slice() })
        .unwrap_or(&[])
} else {
    &[]
};
```

This remains zero-allocation: we do **not** build a `Vec<&[u32]>`.

---

## Task 2: `Shaper` struct + concrete `ShaperHook` + remove `RunIteratorHook` trait

**Files:**
- Create: `crates/font/src/shaper/shaper.rs`
- Modify: `crates/font/src/shaper/mod.rs` (add module, re-export)
- Modify: `crates/font/src/shaper/run_iter.rs` (remove trait, use concrete hook)

**Implementation note (2026-03-28):** The concrete hook was initially named `ShaperHook`, then renamed to `RunIteratorHook` to align with Ghostty naming (`Shaper.RunIteratorHook`). It remains a concrete raw-pointer struct (not a trait). Tests use a small test-only constructor to bind the same hook implementation to owned Vec buffers.

### Rationale

Ghostty's `RunIteratorHook` is NOT a trait/interface. It's a concrete struct inside each shaper backend (`Shaper.RunIteratorHook`). The `RunIterator` takes the concrete type — monomorphic at compile time.

Our Rust trait `RunIteratorHook` forces the generic `RunIterator<'a, H: RunIteratorHook>` and introduces the `RefCell` pattern. Since we have exactly one backend, eliminate the trait entirely.

### Step 1: Define `Codepoint` and `Shaper`

```rust
// crates/font/src/shaper/shaper.rs

use crate::backend::dwrite::analyzer::DWriteAnalyzer;
use crate::shaper::run_iter::{RunIterator, RunOptions, ShaperHook};
use crate::types::{Cell, ShapeOptions, ShapedCells, TextRun};

/// Ghostty: `Shaper.Codepoint` (both harfbuzz.zig and coretext.zig)
#[derive(Clone, Copy)]
pub struct Codepoint {
    pub codepoint: u32,
    pub cluster: u32,
}

/// DWrite shaping engine with Ghostty-aligned buffer ownership.
///
/// Mirrors Ghostty's CoreText `Shaper` struct: owns run_state
/// (codepoints + UTF-16 unichars), features (parsed at init),
/// and the shaping backend.
///
/// Ghostty reference: `font/shaper/coretext.zig::Shaper`
pub struct Shaper {
    pub(crate) analyzer: DWriteAnalyzer,
    /// Ghostty CoreText: `RunState.codepoints`
    pub(crate) codepoints: Vec<Codepoint>,
    /// Ghostty CoreText: `RunState.unichars`
    pub(crate) utf16_buf: Vec<u16>,
    /// Shape configuration owned by the shaper (Ghostty-style init-time options).
    pub(crate) shape_options: ShapeOptions,
}

impl Shaper {
    pub fn new(analyzer: DWriteAnalyzer, shape_options: ShapeOptions) -> Self {
        Self {
            analyzer,
            codepoints: Vec::new(),
            utf16_buf: Vec::new(),
            shape_options,
        }
    }

    /// Reconfigure shape options when renderer text config changes
    /// (font size, locale, feature spec, etc.).
    pub fn reconfigure(&mut self, shape_options: ShapeOptions) {
        self.shape_options = shape_options;
    }

    /// Returns a RunIterator whose hook points back to this Shaper
    /// via raw pointer (matching Ghostty's `shaper: *Shaper`).
    ///
    /// Ghostty: `Shaper.runIterator(opts) -> RunIterator`
    pub fn run_iterator<'a>(&'a mut self, opts: RunOptions<'a>) -> RunIterator<'a> {
        RunIterator::new(opts, ShaperHook::new(self))
    }

    /// Shape the current run using codepoints/UTF-16 collected during
    /// the most recent run iteration.
    ///
    /// Ghostty: `Shaper.shape(run) -> []const Cell`
    pub fn shape<'a>(
        &'a mut self,
        run: TextRun,
        face: &IDWriteFontFace2,
    ) -> Result<ShapedCells<'a>> {
        if self.utf16_buf.is_empty() {
            return Ok(ShapedCells::empty());
        }
        self.analyzer.shape(run, &self.utf16_buf, &self.shape_options, face)
    }
}
```

### Step 2: Concrete `ShaperHook` with raw pointers

```rust
// crates/font/src/shaper/run_iter.rs (replaces RunIteratorHook trait)

use crate::shaper::shaper::Codepoint;

/// Concrete hook that writes into the Shaper's owned buffers via raw pointer.
///
/// Ghostty: `Shaper.RunIteratorHook` (a concrete struct, NOT an interface).
/// Harfbuzz: `{ shaper: *Shaper }`, CoreText: `{ shaper: *Shaper }`.
///
/// SAFETY: The raw pointers are valid for the lifetime of the RunIterator.
/// Single-threaded renderer — no concurrent access. The hook only writes
/// during next(); between next() calls the buffers are stable for reading.
pub struct ShaperHook {
    codepoints: *mut Vec<Codepoint>,
    utf16_buf: *mut Vec<u16>,
}

impl ShaperHook {
    pub(crate) fn new(shaper: &mut crate::shaper::shaper::Shaper) -> Self {
        Self {
            codepoints: &mut shaper.codepoints as *mut _,
            utf16_buf: &mut shaper.utf16_buf as *mut _,
        }
    }

    /// Ghostty: `RunIteratorHook.prepare` — clear buffers, retain capacity.
    /// CoreText: `self.shaper.run_state.reset()` which calls
    /// `codepoints.clearRetainingCapacity()` + `unichars.clearRetainingCapacity()`.
    pub(crate) fn prepare(&mut self) {
        // SAFETY: single-threaded, pointer valid for iterator lifetime
        unsafe {
            (*self.codepoints).clear();
            (*self.utf16_buf).clear();
        }
    }

    /// Ghostty: `RunIteratorHook.addCodepoint`
    /// CoreText version: encodes to UTF-16 surrogates, appends dummy codepoint
    /// for surrogate pairs to keep indices aligned 1:1 with UTF-16 positions.
    pub(crate) fn add_codepoint(&mut self, cp: u32, cluster: u32) {
        // SAFETY: single-threaded, pointer valid for iterator lifetime
        unsafe {
            (*self.codepoints).push(Codepoint { codepoint: cp, cluster });
            let c = char::from_u32(cp).unwrap_or('\u{FFFD}');
            let mut buf = [0u16; 2];
            let encoded = c.encode_utf16(&mut buf);
            (*self.utf16_buf).extend_from_slice(encoded);
            // CoreText adds a dummy codepoint={0} entry for surrogate pairs
            // to keep codepoints[] aligned 1:1 with UTF-16 positions.
            // DWrite doesn't need this — its cluster map handles the mapping.
        }
    }

    /// Ghostty: `RunIteratorHook.finalize`
    pub(crate) fn finalize(&mut self) {
        // No-op for DWrite (HarfBuzz: guessSegmentProperties, CoreText: no-op)
    }
}
```

### Step 3: Remove trait, make RunIterator concrete

```rust
// crates/font/src/shaper/run_iter.rs

pub struct RunIterator<'a> {
    hooks: ShaperHook,
    opts: RunOptions<'a>,
    i: usize,
    max: usize,
}

impl<'a> RunIterator<'a> {
    pub(crate) fn new(opts: RunOptions<'a>, hooks: ShaperHook) -> Self {
        Self {
            max: trim_right_empty(opts.cells.raw_cells),
            hooks,
            opts,
            i: 0,
        }
    }

    pub fn next(&mut self) -> Option<TextRun> {
        // ... same logic as current RunIterator::next(),
        // but calls self.hooks.prepare(), self.hooks.add_codepoint(),
        // self.hooks.finalize() directly (no trait dispatch)
    }

    fn add_codepoint(&mut self, hasher: &mut RunHasher, cp: u32, cluster: u32) {
        hasher.add_codepoint(cp, cluster);
        self.hooks.add_codepoint(cp, cluster);
    }
}
```

**Delete:** `RunIteratorHook` trait, `NoopHook` struct, the generic parameter `H: RunIteratorHook`, `RunCodepoint`, `RunCodepointHook`.

**Note on tests:** The existing `RunIterator` tests in `run_iter.rs` use `NoopHook`. Update them to use `ShaperHook::new()` with a dummy `Shaper`, or create a test-only constructor for `RunIterator` that creates a no-op `ShaperHook` (the buffers are just unused Vecs).

---

## Task 3: `Contents` and `FgRows` — the core data model

**Files:**
- Modify: `crates/renderer/src/gpu/scene.rs`

### Step 1: Define `GridSize`, `FgRows`, and `Contents`

Port of Ghostty's `cell.zig::Contents` + `ArrayListCollection`.

```rust
/// Ghostty: `renderer.GridSize`
#[derive(Clone, Copy, Default, Eq, PartialEq)]
struct GridSize {
    rows: u16,
    columns: u16,
}

/// Ghostty: `ArrayListCollection(CellText)` — owns per-row Vec allocations.
///
/// Layout: lists[0] = cursor-first, lists[1..=rows] = text rows,
///         lists[rows+1] = cursor-last.
///
/// Ghostty reference:
///   `src/datastruct/array_list_collection.zig`
///   `src/renderer/cell.zig` — `Contents.fg_rows`
struct FgRows {
    lists: Vec<Vec<QuadInstance>>,
}

impl FgRows {
    fn new() -> Self {
        Self { lists: Vec::new() }
    }

    /// Resize to `rows + 2` lists (cursor-first + N rows + cursor-last).
    ///
    /// Ghostty: `Contents.resize` → `ArrayListCollection.init(rows + 2, cols * 3)`.
    /// Pre-allocate each row list with capacity `cols * 3` to match Ghostty's
    /// sizing heuristic (glyph + underline + strikethrough per column).
    ///
    /// Note: Ghostty says "appendAssumeCapacity MUST NOT be used since it is
    /// possible to exceed this with combining glyphs" — we use `push()` which
    /// handles reallocation automatically.
    fn resize(&mut self, rows: usize, cols: usize) {
        let count = rows + 2;
        self.lists.clear();
        self.lists.reserve(count);
        // Cursor-first lane (capacity 1, matching Ghostty)
        self.lists.push(Vec::with_capacity(1));
        // Text row lanes
        for _ in 0..rows {
            self.lists.push(Vec::with_capacity(cols * 3));
        }
        // Cursor-last lane (capacity 1, matching Ghostty)
        self.lists.push(Vec::with_capacity(1));
    }

    /// Ghostty: `ArrayListCollection.reset` — clear all lists, retain capacity.
    fn reset(&mut self) {
        for list in &mut self.lists {
            list.clear();
        }
    }

    /// Total element count across all lists (for direct upload size).
    fn total_len(&self) -> usize {
        self.lists.iter().map(|l| l.len()).sum()
    }
}

/// Ghostty: `cell.zig::Contents`
///
/// Row-owned persistent cell contents for the terminal grid.
/// Dirty rows are cleared and rebuilt in-place. Backends upload
/// directly from per-row lists (Ghostty `syncFromArrayLists` style).
struct Contents {
    size: GridSize,
    /// Flat array of background colors: `bg_cells[row * cols + col]`.
    /// Ghostty: `Contents.bg_cells: []CellBg`
    bg_cells: Vec<u32>,
    /// Per-row foreground instance lists with cursor lanes.
    /// Ghostty: `Contents.fg_rows: ArrayListCollection(CellText)`
    fg_rows: FgRows,
    /// Tracking state carried over from RendererModel.
    last_cursor_row: Option<u16>,
    bg_generation: u64,
}

impl Contents {
    fn new() -> Self {
        Self {
            size: GridSize::default(),
            bg_cells: Vec::new(),
            fg_rows: FgRows::new(),
            last_cursor_row: None,
            bg_generation: 0,
        }
    }

    /// Ghostty: `Contents.resize`
    fn resize(&mut self, size: GridSize) {
        self.size = size;
        let cell_count = size.rows as usize * size.columns as usize;
        self.bg_cells.resize(cell_count, 0);
        self.fg_rows.resize(size.rows as usize, size.columns as usize);
    }

    /// Ghostty: `Contents.reset`
    fn reset(&mut self) {
        // Zero all background cells
        self.bg_cells.fill(0);
        self.fg_rows.reset();
    }

    /// Ghostty: `Contents.clear(y)` — clear row y's bg slice + fg list.
    fn clear(&mut self, y: u16) {
        let cols = self.size.columns as usize;
        let start = y as usize * cols;
        // Bounds check: if row is out of range, no-op (fail-soft)
        if let Some(slice) = self.bg_cells.get_mut(start..start + cols) {
            slice.fill(0);
        }
        // fg_rows index: y + 1 (index 0 is cursor-first)
        if let Some(list) = self.fg_rows.lists.get_mut(y as usize + 1) {
            list.clear();
        }
    }

    /// Ghostty: `Contents.bgCell(row, col)` — mutable ref to one bg cell.
    #[inline]
    fn bg_cell(&mut self, row: u16, col: u16) -> &mut u32 {
        let idx = row as usize * self.size.columns as usize + col as usize;
        &mut self.bg_cells[idx]
    }

    /// Ghostty: `Contents.add(.text, cell)` — append to row y's fg list.
    #[inline]
    fn add(&mut self, y: u16, instance: QuadInstance) {
        self.fg_rows.lists[y as usize + 1].push(instance);
    }

    /// Ghostty: `Contents.setCursor`
    ///
    /// Block cursors go in cursor-first (drawn before text).
    /// Bar/underline/hollow go in cursor-last (drawn after text).
    fn set_cursor(&mut self, cell: Option<QuadInstance>, block: bool) {
        if self.size.rows == 0 {
            return;
        }
        let rows = self.size.rows as usize;
        // Clear both cursor lanes
        self.fg_rows.lists[0].clear();
        self.fg_rows.lists[rows + 1].clear();

        let Some(cell) = cell else { return };
        if block {
            self.fg_rows.lists[0].push(cell);
        } else {
            self.fg_rows.lists[rows + 1].push(cell);
        }
    }

    /// Ghostty parity helper: lane lists in render order.
    ///
    /// Order: cursor-first → row 0..N-1 → cursor-last.
    #[inline]
    fn fg_lists(&self) -> &[Vec<QuadInstance>] {
        &self.fg_rows.lists
    }
}
```

---

## Task 4: Rewrite rendering pipeline — dissolve RendererFrontend

**Files:**
- Modify: `crates/renderer/src/gpu/scene.rs`
- Modify: whatever struct currently owns/calls `RendererFrontend` (move fields there)

This is the main integration task. Dissolve `RendererFrontend` into the parent renderer struct. Rewrite `build_batch`, `rebuild_row`, `add_glyph` to use `Contents` + `Shaper` + inline color resolution.

### Step 1: New renderer field layout

Move these fields from `RendererFrontend` into the renderer struct that currently calls it:

```rust
// Fields that were on RendererFrontend, now on the renderer:
config: RendererTextConfig,
shared_grid: Arc<SharedGrid>,
shaper: Shaper,                    // was: analyzer + RunCodepointHook
shaper_cache: ShapedRunCache,      // was: shaped_cache
contents: Contents,                // was: model + scratch
cell_metrics: CellMetrics,
locale: String,
feature_spec: FontFeatureSpec,
rasterizer: DWriteGlyphRasterizer,
```

`ShapeOptions` moves into `Shaper` ownership. When config changes, call
`shaper.reconfigure(new_shape_options)`.

The `FrontendInit` struct becomes the init params for the renderer itself.

### Step 2: Rewrite `build_batch`

Method on the renderer struct. Key changes vs current:
- Uses `Contents` instead of `RendererModel` + `RowScratch`
- Calls `contents.clear(y)` + `rebuild_row(...)` for dirty rows
- Calls `contents.set_cursor(...)` for cursor
- Sets `out.instance_count = contents.total_len()`; backend uploads directly from `contents.fg_lists()` (`&[Vec<QuadInstance>]`)
- No `splice` / `apply_delta` / `append_row_at_end` / `replace_row`

```rust
pub(crate) fn build_batch(&mut self, frame: &RenderFrame, out: &mut RenderBatch) -> Result<()> {
    let rows = frame.rows() as usize;
    let cols = frame.cols();
    let dirty = frame.dirty();
    let colors = frame.colors();
    let default_fg = rgb_to_color32(colors.foreground);
    let default_bg = rgb_to_color32(colors.background);
    let scale_factor = self.config.scale_factor.max(1.0);

    out.clear_color = default_bg.to_float4();
    out.dirty_rects.clear();
    out.bg_dirty_rows.clear();
    out.bg_full_upload = false;
    out.grid_cols = cols;
    out.grid_rows = rows as u16;
    out.cell_size = [
        self.cell_metrics.cell_width * scale_factor,
        self.cell_metrics.line_height * scale_factor,
    ];

    // Ghostty: grid_size_diff → resize
    let new_size = GridSize { rows: rows as u16, columns: cols };
    let size_changed = self.contents.size != new_size;
    if size_changed {
        self.contents.resize(new_size);
    }

    let full_rebuild = size_changed || dirty == DirtyState::Full;

    if full_rebuild {
        // Ghostty: self.cells.reset()
        self.contents.reset();
        self.contents.last_cursor_row = None;

        for y in 0..rows {
            rebuild_row(
                y as u16, frame,
                &mut self.contents, &mut self.shaper, &mut self.shaper_cache,
                &self.shared_grid, &self.config, &self.cell_metrics,
                &self.rasterizer, default_fg, default_bg,
            )?;
            push_row_dirty_rect(out, y as u16, cols, self.cell_metrics, scale_factor);
        }
        out.bg_full_upload = true;
    } else {
        let cursor = frame.cursor();
        let cursor_row = cursor_row(&cursor, frame.rows());
        let force_dirty_prev_cursor = if dirty == DirtyState::Clean {
            None
        } else {
            self.contents.last_cursor_row
        };

        for y in 0..rows {
            let y_u16 = y as u16;
            let row_dirty = (dirty == DirtyState::Partial && frame.row_dirty(y_u16))
                || force_dirty_prev_cursor == Some(y_u16)
                || cursor_row == Some(y_u16);
            if !row_dirty {
                continue;
            }

            // Ghostty: self.cells.clear(y) then self.rebuildRow(y, ...)
            self.contents.clear(y_u16);
            rebuild_row(
                y_u16, frame,
                &mut self.contents, &mut self.shaper, &mut self.shaper_cache,
                &self.shared_grid, &self.config, &self.cell_metrics,
                &self.rasterizer, default_fg, default_bg,
            )?;
            push_row_dirty_rect(out, y_u16, cols, self.cell_metrics, scale_factor);
            out.bg_dirty_rows.push(y_u16);
        }

        if let Some(prev) = self.contents.last_cursor_row
            && Some(prev) != cursor_row
        {
            push_row_dirty_rect(out, prev, cols, self.cell_metrics, scale_factor);
        }
        if let Some(cur) = cursor_row {
            push_row_dirty_rect(out, cur, cols, self.cell_metrics, scale_factor);
        }
    }

    // Cursor
    let cursor = frame.cursor();
    let cursor_quad = cursor_instance(
        frame, cursor, colors.cursor_color, colors.has_cursor_color != 0,
        &self.cell_metrics, &self.config,
    );
    let is_block = cursor.style == 1;
    self.contents.set_cursor(cursor_quad, is_block);
    self.contents.last_cursor_row = cursor_row(&cursor, frame.rows());

    // Record fg count; backend uploads directly from lane slices.
    out.instance_count = self.contents.fg_rows.total_len();

    // Sync bg
    let bg_len = rows * cols as usize;
    if out.bg_cells_rgba.len() != bg_len {
        out.bg_cells_rgba.resize(bg_len, default_bg.to_rgba_u32());
        out.bg_full_upload = true;
    }
    if out.bg_full_upload {
        out.bg_cells_rgba.clear();
        out.bg_cells_rgba.extend_from_slice(&self.contents.bg_cells);
    } else {
        for &y in &out.bg_dirty_rows {
            let start = y as usize * cols as usize;
            let end = start + cols as usize;
            out.bg_cells_rgba[start..end]
                .copy_from_slice(&self.contents.bg_cells[start..end]);
        }
    }

    if out.bg_full_upload || !out.bg_dirty_rows.is_empty() {
        self.contents.bg_generation = self.contents.bg_generation.wrapping_add(1);
    }
    out.bg_generation = self.contents.bg_generation;

    Ok(())
}
```

### Step 3: `rebuild_row` as a free function — inline style + color use

Mirrors Ghostty's `rebuildRow` (generic.zig:2610). Resolves style and colors inline per-cell (no fg_by_col). Uses `ShaperHook` (raw pointer) for codepoints/UTF-16. Uses row-level grapheme FFI. Shapes runs lazily like Ghostty.

```rust
/// Ghostty: `rebuildRow`
///
/// Free function for borrow splitting across renderer fields.
fn rebuild_row(
    y: u16,
    frame: &RenderFrame,
    contents: &mut Contents,
    shaper: &mut Shaper,
    shaper_cache: &mut ShapedRunCache,
    shared_grid: &Arc<SharedGrid>,
    config: &RendererTextConfig,
    cell_metrics: &CellMetrics,
    rasterizer: &DWriteGlyphRasterizer,
    default_fg: Color32,
    default_bg: Color32,
) -> Result<()> {
    let Some(raw_cells) = frame.row_raw(y) else { return Ok(()) };
    let cols = raw_cells.len();

    // No separate pre-scan loop. Resolve style/grapheme in the main flow.
    //
    // Styles are needed throughout row processing, so fetch once.
    // Graphemes are row-level SoA and zero-copy, so one fetch per rebuilt row
    // is acceptable and keeps the control flow simple.
    let styles = frame.row_styles(y).unwrap_or(&[]);
    let graphemes = frame.row_graphemes(y).unwrap_or(&[]);

    let cursor = frame.cursor();
    let cursor_x = (cursor.visible != 0 && cursor.in_viewport != 0 && cursor.y == y)
        .then_some(cursor.x as usize);
    let selection = frame.row_selection(y).map(|(start, end)| [start, end]);

    let row_cells = RowCells { raw_cells, styles, graphemes };
    let run_opts = RunOptions {
        grid: shared_grid.as_ref(),
        cells: row_cells,
        selection,
        cursor_x,
    };

    let scale_factor = config.scale_factor.max(1.0);
    let baseline_y = y as f32 * cell_metrics.line_height + cell_metrics.baseline;

    // Create hook via raw pointer (matching Ghostty's self: *Shaper)
    // SAFETY: shaper is exclusively borrowed for the duration of this function.
    // The hook writes to shaper.codepoints/utf16_buf during next().
    // Between next() calls, we read those buffers for shape().
    // Single-threaded renderer — no concurrent access.
    let hook = ShaperHook::new(shaper);
    let mut run_iter = RunIterator::new(run_opts, hook);
    // run_iter owns ShaperHook (raw ptrs, no lifetime tie to shaper fields)
    // so we can still call shaper.analyzer.shape() between next() calls

    // Ghostty pattern: iterate cells, lazily advance run iterator and shape
    // For simplicity, we iterate runs then emit cells (equivalent result)
    let mut shaper_run: Option<TextRun> = run_iter.next();
    let mut shaper_cells: Option<&[Cell]> = None;
    let mut shaper_cells_i: usize = 0;

    for x in 0..cols {
        let raw = raw_cells[x];

        // Ghostty style access pattern: style is looked up by column when
        // this cell has styling; otherwise no style is applied.
        let style = if raw.style_id() != 0 {
            styles.get(x)
        } else {
            None
        };

        let mut fg = resolve_fg_color(style, frame.palette(), default_fg);
        let mut bg = if raw.is_bg_only() {
            resolve_bg_only(raw, frame.palette(), default_bg)
        } else {
            resolve_bg_color(style, frame.palette(), default_bg)
        };

        if style.is_some_and(CellStyle::is_inverse) {
            std::mem::swap(&mut fg, &mut bg);
        }
        if style.is_some_and(CellStyle::is_invisible) {
            fg = bg;
        }
        if style.is_some_and(CellStyle::is_faint) {
            fg = fg.with_alpha(((fg.a() as f32) * 0.7) as u8);
        }

        *contents.bg_cell(y, x as u16) = bg.to_rgba_u32();

        if style.is_some_and(CellStyle::is_invisible) {
            continue;
        }

        // --- Lazy run shaping (matching Ghostty rebuildRow) ---
        // Advance run iterator when current run's shaped cells are exhausted
        if shaper_cells.is_some_and(|c| shaper_cells_i >= c.len()) {
            shaper_run = run_iter.next();
            shaper_cells = None;
            shaper_cells_i = 0;
        }

        if let Some(run) = shaper_run {
            // Shape on demand (cache check first)
            if shaper_cells.is_none() {
                shaper_cells = Some(shape_run_cached(
                    shaper_cache, shaper, run, shared_grid,
                )?);
            }

            if let Some(cells) = shaper_cells {
                // Emit all shaped cells that match column x
                while shaper_cells_i < cells.len()
                    && run.offset.saturating_add(cells[shaper_cells_i].x) as usize == x
                {
                    add_glyph(
                        y, raw_cells, run, &cells[shaper_cells_i], fg,
                        contents, shared_grid, config, cell_metrics,
                        rasterizer, baseline_y, scale_factor,
                    )?;
                    shaper_cells_i += 1;
                }
            }
        }
    }

    Ok(())
}
```

**Ghostty comparison:** Ghostty does not need a lazy-FFI pattern because it reads style/grapheme SoA columns directly in-process (`cells_slice.items(.style/.grapheme)`). The equivalent behavior we preserve is no style-only pre-pass and per-cell style/grapheme use in the main render/shaping flow.

**Borrow checker note on `shape_run_cached`:** The hook holds raw pointers to `shaper.codepoints` and `shaper.utf16_buf`. Between `run_iter.next()` calls (which write to those buffers via the hook), we read `shaper.utf16_buf` and call `shaper.shape()`. This is safe because:
1. `run_iter.next()` returns before we access the buffers
2. The raw pointer hook doesn't create a Rust mutable borrow
3. We only read `utf16_buf` (shared access) after `next()` returns
4. `Shaper::shape` only reads `utf16_buf` for the current run and mutably borrows the analyzer internally

### Step 4: `shape_run_cached` as a free function (zero-copy)

```rust
/// Shape a run, returning a borrowed slice. Zero-copy on cache hit.
///
/// Ghostty: `shaper_cells = self.font_shaper_cache.get(run) orelse cache: { ... }`
fn shape_run_cached<'a>(
    cache: &'a mut ShapedRunCache,
    shaper: &mut Shaper,
    run: TextRun,
    shared_grid: &Arc<SharedGrid>,
) -> Result<&'a [Cell]> {
    if let Some(cells) = cache.get(run.hash) {
        return Ok(cells);
    }

    let Some(face) = shared_grid.face_for_index(run.font_index) else {
        cache.put(run.hash, &[]);
        return Ok(cache.get(run.hash).unwrap_or(&[]));
    };

    if shaper.utf16_buf.is_empty() {
        cache.put(run.hash, &[]);
        return Ok(cache.get(run.hash).unwrap_or(&[]));
    }

    let shaped = shaper.shape(run, &face)?;
    cache.put(run.hash, shaped.cells);
    Ok(cache.get(run.hash).unwrap_or(&[]))
}
```

### Step 5: `add_glyph` as a free function

```rust
/// Ghostty: `addGlyph`
fn add_glyph(
    y: u16,
    raw_cells: &[RawCell],
    run: TextRun,
    cell: &Cell,
    fg: Color32,
    contents: &mut Contents,
    shared_grid: &Arc<SharedGrid>,
    config: &RendererTextConfig,
    cell_metrics: &CellMetrics,
    rasterizer: &DWriteGlyphRasterizer,
    baseline_y: f32,
    scale_factor: f32,
) -> Result<()> {
    let col = run.offset.saturating_add(cell.x) as usize;
    if col >= raw_cells.len() {
        return Ok(());
    }
    let raw = raw_cells[col];
    if !raw.has_text() || raw.codepoint() == 0 {
        return Ok(());
    }

    let mut options = GlyphRenderOptions::default();
    options = options.with_cell_width(if raw.wide() == 1 { 2 } else { 1 });
    let key = GlyphKey::new(run.font_index, cell.glyph_index, options);
    let cached = resolve_glyph_cached(shared_grid, rasterizer, config, key)?;
    if cached.width == 0 || cached.height == 0 {
        return Ok(());
    }

    let pen_x = (f32::from(run.offset + cell.x) * cell_metrics.cell_width
        + f32::from(cell.x_offset)) * scale_factor;
    let pen_y = (baseline_y + f32::from(cell.y_offset)) * scale_factor;
    let origin = [pen_x + cached.offset_x as f32, pen_y + cached.offset_y as f32];
    let size = [cached.width as f32, cached.height as f32];

    let mut instance = if cached.atlas_kind == Some(GlyphAtlasKind::Color) {
        QuadInstance::color_glyph_rect(origin, size)
    } else {
        QuadInstance::glyph_rect(origin, size, fg.to_rgba_u32())
    };
    instance.set_texcoord(
        cached.atlas_x.min(u16::MAX as u32) as u16,
        cached.atlas_y.min(u16::MAX as u32) as u16,
    );

    contents.add(y, instance);
    Ok(())
}
```

### Step 6: `cursor_instance` as a free function

Extract from `RendererFrontend` method to free function, taking config/metrics params.

---

## Task 5: Delete old structures

**Files:**
- Modify: `crates/renderer/src/gpu/scene.rs`

Delete:
- `RendererFrontend` struct (dissolved into renderer)
- `FrontendInit` struct (absorbed into renderer init)
- `RendererModel` struct
- `RowSpan` struct
- `RowScratch` struct
- `RunCodepoint` struct
- `RunCodepointHook` struct + impl
- `append_row_at_end` method
- `replace_row` method
- `apply_delta` function
- `push_row_dirty_rect_for_instances` function
- `rebuild_row_into_scratch` method
- `shape_run_cached` method on RendererFrontend (replaced by free function)
- `emit_shaped_cells` method (replaced by `add_glyph`)

Also:
- Remove `std::cell::RefCell` import
- Remove `RunIteratorHook` trait from `run_iter.rs`
- Remove `NoopHook` struct from `run_iter.rs`

---

## Task 6: Direct foreground GPU upload (Ghostty parity)

**Files:**
- Modify: `crates/renderer/src/gpu/types.rs`
- Modify: `crates/renderer/src/gpu/backend_d3d11.rs`
- Modify: `crates/renderer/src/gpu/scene.rs`

### Step 1: Update RenderBatch contract

Remove `instances: Vec<QuadInstance>` from `RenderBatch`. Add `instance_count: usize`.

Foreground instance ownership lives in `Contents.fg_rows`; `build_batch` only updates row-owned lists and sets `instance_count`.

### Step 2: Backend uploads from row lanes (Ghostty Metal-style loop)

Add a D3D11 draw path that takes `&[Vec<QuadInstance>]` and uploads directly.

Implementation pattern (mirrors Ghostty `renderer/metal/buffer.zig::syncFromArrayLists`):

1. Read lane lists in order (`cursor-first`, rows, `cursor-last`) from `contents.fg_lists()`.
2. `Map(instance_buffer, D3D11_MAP_WRITE_DISCARD)` once.
3. Keep a running byte offset, and for each lane slice do `copy_nonoverlapping`/`memcpy` into mapped memory.
4. `Unmap(instance_buffer)`.
5. `DrawIndexedInstanced(6, instance_count, ...)`.

Notes:
- `&[Vec<QuadInstance>]` is a pure borrow of existing lane vectors; it does not move or copy instance data.
- Inside the backend loop, each `&Vec<QuadInstance>` coerces to `&[QuadInstance]` (`lane.as_slice()`), then memcpy into mapped GPU memory.

This matches Ghostty's backend pattern:
- `renderer/generic.zig`: `frame.cells.syncFromArrayLists(self.cells.fg_rows.lists)`
- `renderer/opengl/buffer.zig`: `syncFromArrayLists`
- `renderer/metal/buffer.zig`: `syncFromArrayLists`

### Step 3: Keep background sync as-is

Background upload remains unchanged:
- `Contents.bg_cells` is the persistent store
- `RenderBatch.bg_cells_rgba` is the upload buffer
- Full upload: `extend_from_slice(&contents.bg_cells)`
- Partial: `copy_from_slice` per dirty row

### Windows Terminal comparison

WT does not use Ghostty's row-list upload API, but it applies the same core optimization goal: stage quads in a reusable CPU buffer and perform a single mapped upload before draw.

Reference:
- `opensrc/repos/microsoft/terminal/src/renderer/atlas/BackendD3D.cpp`
  - `_appendQuad()` appends quads into contiguous `_instances`
  - `_flushQuads()` maps `_instanceBuffer` with `D3D11_MAP_WRITE_DISCARD`, `memcpy`s contiguous instances, unmaps, then draws

So our direct lane-to-mapped-copy path is Ghostty-style in structure, and WT-style in D3D11 upload mechanics.

### D3D11 upload strategy note

Base implementation: one `Map(D3D11_MAP_WRITE_DISCARD)` + sequential memcpy + one draw.

Potential upgrade (only if profiling proves needed): dynamic ring-buffer strategy with
`D3D11_MAP_WRITE_NO_OVERWRITE` for append-heavy multi-draw workloads, falling back to
`WRITE_DISCARD` on wrap. This is more complex and unnecessary for the current single-draw
foreground path.

---

## Task 7: Verification

### Step 1: Build check

```bash
cargo build -p renderer 2>&1 | head -50
```

### Step 2: Runtime verification

- Text renders correctly with new Contents model
- Cursor renders correctly for block (cursor-first lane) / bar / underline (cursor-last lane)
- Dirty-row rebuild only touches changed rows
- Resize and scroll remain smooth
- No panics in steady-state rendering
- Shaped-run cache hits in steady-state (no clone overhead)
- Atlas grows without crash
- Color emoji renders correctly
- Selection highlighting works
- Wide characters (CJK) render correctly

---

## Unresolved Questions

1. **`shape_run_cached` borrow checker:** The returned `&'a [Cell]` borrows from `&'a mut ShapedRunCache`. Within the per-cell loop, each run's cells are consumed (pushed into Contents) before the next run is shaped. If the borrow checker complains, fallback: copy cell data into a small `SmallVec<[Cell; 32]>` per run.

2. **`ShaperHook` raw pointer safety:** The raw-pointer hook pattern is safe in practice (single-threaded, no concurrent access, data valid for duration of use). Add a `// SAFETY:` comment block explaining the invariants. Matches Ghostty's `self: *Shaper` exactly.

3. **`CursorState.style` values:** FFI mapping in `render.zig` is `0=bar, 1=block, 2=underline, 3=block_hollow`. For `Contents.set_cursor`, `is_block` should be true only for style `1`.

4. **RunIterator tests:** Current tests use `NoopHook` / `RunIterator::new(opts)`. After removing the trait, update tests to construct `RunIterator` with a `ShaperHook` backed by a dummy `Shaper` (buffers are allocated but unused in tests). Or add a test-only constructor.

5. **GraphemeSlice ABI verification:** Add comptime asserts in `render.zig` that `@sizeOf([]const u21) == @sizeOf(GraphemeSlice)` and `@alignOf([]const u21) == @alignOf(GraphemeSlice)`. Also keep access guarded by `raw.has_grapheme()` because non-grapheme entries are undefined in Ghostty RenderState.

6. **No separate style pre-pass:** remove `raw_cells.iter().any(|cell| cell.style_id() != 0)` from `rebuild_row`; fetch style row once and resolve inline per cell (Ghostty-aligned behavior).

---

## Future Optimizations (remaining)

### A. (Integrated in this task)

Direct mapped foreground upload from row lists is now part of Task 6.

### B. Arena rework (replace VirtualAlloc arena with simple Vecs)

Ghostty does NOT use an arena for shaping. Both HarfBuzz and CoreText shapers own simple `ArrayListUnmanaged` buffers that are `clearRetainingCapacity`'d between calls. CoreText uses a short-lived `ArenaAllocator` only for temporary CTRun data within a single `shape()` call.

Our `DWriteAnalyzer` uses a VirtualAlloc-backed VM arena (`OutputArena` + `ScratchArena`) for the DWrite API output buffers (GetGlyphs, GetGlyphPlacements). The arena is over-engineered for this — simple reusable `Vec` fields with `clear()` between calls would match Ghostty's approach and be simpler to reason about.

**Trade-off:** The arena has some perf advantages (contiguous memory, no realloc) but adds complexity. Consider replacing with Vecs when doing a DWriteAnalyzer cleanup pass.

### C. (Integrated in this task)

`ShapeOptions` init-time binding (Ghostty ref: `Shaper.init()`) is now part of Task 2/4:
- `Shaper` owns `shape_options`
- renderer updates via `shaper.reconfigure(...)` on config changes
- `shape()` takes `(run, face)` and uses shaper-owned options

### D. Deferred: full Ghostty color-resolution parity (move to Color FFI rework)

Defer full color-parity logic until the Color32 removal / Color FFI rework pass.
Task 8 keeps inline color resolution and removes `fg_by_col`, but does not port all
Ghostty color equations yet.

When doing the Color FFI rework, port Ghostty `generic.zig::rebuildRow` color path
(~2752-2916) exactly, including:
- selected-state model:

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SelectedState {
    False,
    Selection,
    Search,
    SearchSelected,
}
```

- `x_compare` spacer-tail handling for selection checks,
- `bg` equation including `inverse != isCovering(codepoint)`,
- `fg` equation including inverse/selected branches,
- faint alpha (`faint_opacity`) and background alpha policy (`background_opacity*`),
- invisible behavior and decoration interactions,
- renderer config wiring for user config fields (`bold-color`, `faint-opacity`,
  `background-opacity`, `background-opacity-cells`, selection/search fg/bg sources).
