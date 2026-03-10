# Zero-Copy FFI Rework

> Supersedes the `FlatCell` / `RenderSnapshot` model described in
> [02-data-model.md](02-data-model.md) and [06-ghostty-shim.md](06-ghostty-shim.md).

## Goal

Eliminate all per-frame copying and allocation in the FFI boundary
between Ghostty's Zig terminal engine and the Rust renderer. Replace
the owned `RenderSnapshot` with direct pointer access into
`RenderState`'s persistent memory.

## Motivation

The current pipeline has three measured bottlenecks:

1. **50–100 KB allocation inside the mutex every frame.**
   `RenderSnapshot::capture()` holds the terminal `Mutex`, calls
   `render_update()`, then iterates every row — flattening each cell
   into a 28-byte `FlatCell`, copying all 256 palette entries, and
   cloning grapheme clusters. This is O(rows × cols) work under the
   lock on every frame, even when most rows are clean.

2. **Mutex contention between renderer and read thread.**
   The read thread locks the terminal to call `feed()`. The UI thread
   locks it for the full `RenderSnapshot::capture()` duration. Under
   heavy output, profiling shows the read thread stalling on the lock
   while the renderer copies data it mostly won't use (clean rows).

3. **Redundant data transformation.**
   `FlatCell` flattens Ghostty's structured data (packed `page.Cell` +
   `Style` tagged union) into a wide, denormalized struct. The renderer
   immediately re-structures this into text runs and color lookups —
   the intermediate form exists only to cross the FFI boundary.

## Key Insight

`RenderState` is a **persistent, renderer-owned snapshot** that Ghostty
already maintains. It tracks dirty rows and only copies changed data
during `update()`. Its memory is stable between `update()` calls —
the raw cell array, style array, and grapheme data for each row live
in allocations that survive across frames.

Crucially, `RenderState` is only mutated by `render_update()`, which
is only called from the UI thread during prepaint. The read thread
calls `feed()` on the underlying `Terminal` but never touches
`RenderState`. The IO thread only calls `reset_synchronized_output()`.

This means: after `render_update()` completes and the mutex is dropped,
`RenderState`'s memory is immutable until the next `render_update()`
call — which happens on the same thread, in the next frame's prepaint.
We can safely hand out pointers to this memory for the duration of
the frame.

## Architecture

### Data Flow

```
CURRENT (per frame):
  lock mutex
    → render_update()                     [fast: Zig-side dirty-row copy]
    → RenderSnapshot::capture()           [slow: flatten ALL rows to FlatCell]
      → row_cells() × N rows              [28 bytes × cols per row, scratch alloc]
      → palette_color() × 256             [256 FFI calls]
      → cell_grapheme() per grapheme cell [Vec<u32> clone per cell]
  unlock mutex
  → build_text_runs(snapshot)             [re-parse FlatCell into runs]

NEW (per frame):
  lock mutex
    → render_frame()                      [calls render_update() + returns detached RenderFrame]
    → scrollbar_info(), is_alternate_screen()  [non-RenderState fields, require lock]
  unlock mutex                            [MutexGuard drops; RenderFrame lives on]
  → for each dirty row:
      row_raw(y)    → &[RawCell]          [zero-copy pointer into Zig memory]
      row_styles(y) → &[CellStyle]        [zero-copy pointer into Zig memory]
      cell_grapheme(y, col) → &[u32]      [on-demand, scratch buffer, rare]
  → palette()       → &[ColorRGB; 256]    [zero-copy sidecar, rebuild PaletteCache only when changed]
  → drop frame                            [clears dirty flags]
```

### Threading Model & Safety

```
                     ┌─────────────────────────────┐
                     │       UI Thread (GPUI)      │
                     │                             │
                     │  lock mutex ─┐              │
                     │    render_frame()           │
                     │    scrollbar_info()         │
                     │    is_alternate_screen()    │
                     │  unlock mutex ◄┘            │
                     │                             │
                     │  (frame lives on)           │
                     │    row_raw(y)  ──► Zig mem  │
                     │    row_styles(y) ► Zig mem  │
                     │    palette()   ──► Zig mem  │
                     │  drop frame                 │
                     └─────────────────────────────┘

  ┌──────────────────┐                  ┌──────────────┐
  │   Read Thread    │                  │  IO Thread   │
  │                  │                  │              │
  │  lock mutex ─┐   │                  │  (rare)      │
  │    feed()    │   │                  │  lock mutex  │
  │    drain()   │   │                  │  reset_sync  │
  │  unlock ◄────┘   │                  │  unlock      │
  │                  │                  │              │
  │  NEVER touches   │                  │  NEVER       │
  │  RenderState     │                  │  touches     │
  │                  │                  │  RenderState │
  └──────────────────┘                  └──────────────┘
```

**Safety invariant:** `RenderState` is only mutated by
`render_update()`, which is only called from the UI thread. After
`render_update()` returns and the mutex is dropped, no other thread
can mutate `RenderState` until the next `render_update()` call on
the same thread. Pointers into `RenderState` are therefore stable for
the duration of the frame.

The `RenderFrame` enforces this within Rust via an architectural invariant:
`render_frame()` is only called from prepaint on the UI thread. It calls
`render_update()` internally and returns a detached `RenderFrame { handle }`
— holding the raw Zig handle pointer, not a borrow of `Terminal`. The
`MutexGuard` is dropped at the end of the lock scope (not by `render_frame()`
itself), and `RenderFrame` lives on, allowing lock-free reads for the rest
of prepaint.

```rust
// Safety: RenderState is only mutated by render_update(), which is
// only callable via &mut Terminal. Between render_update() and the
// next prepaint, no thread can call render_update() because:
//   1. The read thread only calls feed() (never render_update())
//   2. The IO thread only calls reset_synchronized_output()
//   3. render_update() is only called here, on the UI thread
// Therefore, RenderState memory is stable for the frame's lifetime.
// RenderFrame holds a raw *mut c_void handle (Zig-allocated, heap-stable).
// The borrow checker cannot enforce the frame/render_update ordering, but
// the single-threaded UI loop makes it an architectural invariant.
//
// palette_cache is NOT in RenderState — it lives directly in TerminalHandle
// and is repopulated by render_update() only when palette_dirty is set.
// It is treated as frame-stable: stable from when render_update() returns
// until the next render_update() call. Accessing it via frame.palette()
// after the lock drops is safe under the same invariant as RenderState.
//
// RenderFrame is intentionally !Send. It must be created and dropped on
// the UI thread. Cross-thread use would violate the threading model above.
```

## FFI Surface

### Zero-Copy APIs

#### `ghostty_vt_terminal_render_row_raw` → Raw Cell Data

Returns a direct pointer into `RenderState`'s `MultiArrayList(Cell)`
`.raw` field — an array of `page.Cell` values.

```zig
export fn ghostty_vt_terminal_render_row_raw(
    ptr: ?*anyopaque, row: u16, out_len: ?*u16,
) callconv(.c) ?[*]const u64 {
    // ...
    const cells = handle.render_state.row_data.items(.cells)[row];
    const raws = cells.items(.raw);
    if (out_len) |len| len.* = @intCast(raws.len);
    return @ptrCast(raws.ptr);  // page.Cell is packed struct(u64)
}
```

Rust receives this as `&[RawCell]` where `RawCell` is a
`#[repr(transparent)]` wrapper over `u64` with bit-extraction methods
matching `page.Cell`'s packed layout.

#### `ghostty_vt_terminal_render_row_styles` → Style Data

Returns a direct pointer into `RenderState`'s `MultiArrayList(Cell)`
`.style` field — an array of `terminal.Style` values.

```zig
export fn ghostty_vt_terminal_render_row_styles(
    ptr: ?*anyopaque, row: u16, out_len: ?*u16,
) callconv(.c) ?[*]const CellStyle {
    // ...
    const cells = handle.render_state.row_data.items(.cells)[row];
    const styles = cells.items(.style);
    if (out_len) |len| len.* = @intCast(styles.len);
    // ptrCast valid: CellStyle extern struct layout matches terminal.Style
    // (verified by comptime assertions). No data conversion.
    return @ptrCast(styles.ptr);
}
```

`CellStyle` is a Zig `extern struct` defined in `render.zig` that mirrors
the byte layout of `terminal.Style`. It serves as a compile-time safety
check: the Zig type system validates the `@ptrCast`, and the comptime
assertions compare `@sizeOf`/`@offsetOf` of `CellStyle` against
`terminal.Style` directly. On the Rust side, `CellStyle` is mirrored as
a `#[repr(C)]` struct. No data conversion occurs — the pointer is directly
into `RenderState` memory. `StyleColor` (the tagged union field type) is
similarly defined as an `extern struct` on both sides.

See "ABI Safety" below for the assertion strategy.

#### `ghostty_vt_terminal_render_palette` → Palette Data

Returns a direct pointer into `RenderState`'s palette.

Ghostty's `color.RGB` is `packed struct(u24)` with `@sizeOf == 4`
(1 byte padding per entry). This means the palette's in-memory layout
is `[256]RGB` = 1024 bytes with per-element padding. We cannot directly
cast this to Rust's 3-byte `ColorRGB`.

We use a C-safe sidecar: the Zig shim maintains a `[256]PaletteColor`
(`extern struct { r: u8, g: u8, b: u8 }`, `@sizeOf == 3`) that is
populated during `render_update()` only when the palette is dirty.
`PaletteColor` is defined in `handle.zig` (since `TerminalHandle` owns it)
and imported into `render.zig`. On the Rust side it maps directly to `ColorRGB`
(`#[repr(C)]` with `{r, g, b}`) — no Rust-side `PaletteColor` type needed.
The FFI function returns a pointer to this sidecar.

```zig
// In handle.zig:
pub const PaletteColor = extern struct { r: u8, g: u8, b: u8 };

// In TerminalHandle:
palette_cache: [256]PaletteColor = std.mem.zeroes([256]PaletteColor),
palette_dirty: bool = true,

// In render_update():
if (handle.palette_dirty) {
    for (render_state.colors.palette, 0..) |rgb, i| {
        handle.palette_cache[i] = .{ .r = rgb.r, .g = rgb.g, .b = rgb.b };
    }
    handle.palette_dirty = false;
}
// palette_dirty is set to true on init (true = first frame always copies)
// and re-set whenever feed() processes color-change escape sequences.
// Ghostty processes palette changes via OSC 4/10/11 and SGR sequences
// through the same ShimHandler::vt() path as other terminal mutations.
// Since ShimHandler has no dedicated color-change hook, palette_dirty is
// set to true at the start of every feed() call in handle.zig. This is
// conservative but correct: feed() is called ≤ once per frame on the
// read thread, so one extra bool check per render_update() is negligible.
```

```zig
export fn ghostty_vt_terminal_render_palette(
    ptr: ?*anyopaque,
) callconv(.c) ?[*]const PaletteColor {
    // ...
    return &handle.palette_cache;
}
```

This is the one place where zero-copy from Ghostty's native layout is
impractical due to the padding mismatch. The conversion cost is
negligible: 256 × 3-byte copies, only when the palette changes.

#### `ghostty_vt_terminal_render_cell_grapheme` → Grapheme Data (On-Demand)

Graphemes are the rare path — most cells have `content_tag == .codepoint`
with no extra codepoints. When a cell does have a grapheme cluster,
the data lives in the row's arena as `[]const u21`.

Since Zig's `u21` and Rust's `u32` have different sizes, a widening
copy into a scratch buffer is required. The existing `grapheme_buf`
approach works here. The cost is negligible: grapheme cells are sparse,
and the buffer is reused across calls.

```zig
export fn ghostty_vt_terminal_render_cell_grapheme(
    ptr: ?*anyopaque, row: u16, col: u16, out_len: ?*u8,
) callconv(.c) ?[*]const u32 {
    // ... existing implementation, scratch buffer copy
}
```

### Scalar APIs (Unchanged)

These are small, POD copies — no performance concern:

- `ghostty_vt_terminal_render_dirty` → `u8`
- `ghostty_vt_terminal_render_rows` / `_cols` → `u16`
- `ghostty_vt_terminal_render_row_dirty` → `u8`
- `ghostty_vt_terminal_render_cursor` → `CursorState` (10 bytes)
- `ghostty_vt_terminal_render_colors` → `ColorState` (10 bytes)
- `ghostty_vt_terminal_render_row_selection` → `(u16, u16)`

## Rust-Side Types

### `RawCell` — Packed Cell Access

```rust
/// Zero-copy view into Ghostty's page.Cell packed struct(u64).
///
/// Bit layout (little-endian):
///   [1:0]   content_tag: 0=codepoint, 1=codepoint_grapheme,
///                        2=bg_color_palette, 3=bg_color_rgb
///   [22:2]  content: u21 codepoint, u8 palette index, or RGB
///   [25:23] (unused in content, part of union)
///   [41:26] style_id: u16 (0 = default style)
///   [43:42] wide: 0=narrow, 1=wide, 2=spacer_tail, 3=spacer_head
///   [44]    protected
///   [45]    hyperlink
///   [47:46] semantic_content
///   [63:48] _padding
#[repr(transparent)]
pub struct RawCell(u64);

impl RawCell {
    pub fn content_tag(&self) -> u8 { ... }
    pub fn codepoint(&self) -> u32 { ... }
    pub fn style_id(&self) -> u16 { ... }
    pub fn wide(&self) -> u8 { ... }
    pub fn is_spacer(&self) -> bool { ... }
    pub fn has_text(&self) -> bool { ... }
    pub fn is_bg_only(&self) -> bool { ... }
    pub fn has_grapheme(&self) -> bool { ... }
    // bg-from-content accessors for content_tag 2 and 3
    pub fn bg_palette_index(&self) -> u8 { ... }
    pub fn bg_rgb(&self) -> (u8, u8, u8) { ... }
}
```

### `CellStyle` — Direct Style Access

The exact byte layout of `CellStyle` must be determined empirically
at implementation time (see "Implementation Verification" below).

`terminal.Style` is a **regular Zig struct** (not `packed`, not
`extern`) containing three `Color` tagged unions and a `Flags` packed
struct. Zig does not guarantee field ordering or padding for regular
structs, and the layout of tagged unions (tag position relative to
payload, alignment) is implementation-defined.

What we know from the source:
- `Style` has fields: `fg_color`, `bg_color`, `underline_color` (all
  `Color`), and `flags` (`packed struct(u16)`)
- `Color = union(Tag) { none: void, palette: u8, rgb: color.RGB }`
- `Color.Tag = enum(u8) { none, palette, rgb }`
- `color.RGB = packed struct(u24)` with `@sizeOf == 4` (1 byte padding)
- `Flags = packed struct(u16)` with known bit layout

What we **don't** know without running Zig:
- Whether Zig puts the tag byte before or after the payload in `Color`
- `@sizeOf(Color)` — could be 4, 5, 8, etc. depending on alignment
- `@sizeOf(Style)` — the `PackedStyle` is 16 bytes but that's a
  different, manually packed representation
- Whether Zig reorders the struct fields in memory

The Rust `CellStyle` struct will be constructed after running the
verification steps below. The shape will be something like:

```rust
/// Zero-copy view into Ghostty's terminal.Style.
/// 
/// ABI safety: Comptime assertions on the Zig side verify
/// @sizeOf, @offsetOf, and byte-level layout of known values.
/// If Ghostty changes Style's layout, the build fails.
/// PLACEHOLDER — exact layout TBD from Zig comptime probing.
#[repr(C)]
pub struct CellStyle {
    // Field order, sizes, and padding determined by
    // implementation verification step.
    // Contains: fg color, bg color, underline color, flags
}

/// Matches Zig's terminal.Style.Color tagged union layout.
/// PLACEHOLDER — exact layout TBD.
#[repr(C)]
pub struct StyleColor {
    // Tag + payload, order and padding TBD
}
```

The flag accessors are known regardless of struct layout, since
`Flags` is `packed struct(u16)` with deterministic bit positions:

```rust
impl CellStyle {
    pub fn is_bold(&self) -> bool { self.flags & (1 << 0) != 0 }
    pub fn is_italic(&self) -> bool { self.flags & (1 << 1) != 0 }
    pub fn is_faint(&self) -> bool { self.flags & (1 << 2) != 0 }
    pub fn is_blink(&self) -> bool { self.flags & (1 << 3) != 0 }
    pub fn is_inverse(&self) -> bool { self.flags & (1 << 4) != 0 }
    pub fn is_invisible(&self) -> bool { self.flags & (1 << 5) != 0 }
    pub fn is_strikethrough(&self) -> bool { self.flags & (1 << 6) != 0 }
    pub fn is_overline(&self) -> bool { self.flags & (1 << 7) != 0 }
    pub fn underline_style(&self) -> u8 { ((self.flags >> 8) & 0x7) as u8 }
}
```

### Palette Entry — `ColorRGB`

The Zig-side `PaletteColor` extern struct maps directly to the existing Rust
`ColorRGB` type — same `#[repr(C)]` layout with `{r, g, b}`. No new Rust type
is introduced; `ghostty_vt_terminal_render_palette` returns `*const ColorRGB`
in the Rust FFI declaration.

```rust
/// Already exists in ghostty-vt/src/types.rs.
/// #[repr(C)] { r: u8, g: u8, b: u8 } — matches PaletteColor on Zig side.
// pub struct ColorRGB { pub r: u8, pub g: u8, pub b: u8 }
```

## ABI Safety: Comptime Assertions

The Zig shim includes comprehensive comptime assertions that verify the
Rust-side struct layouts match the Zig-side layouts. If Ghostty changes
any of these types, the assertions fail at build time — nothing can
silently corrupt at runtime.

### page.Cell (RawCell)

```zig
comptime {
    // Struct-level
    assert(@sizeOf(page.Cell) == 8);
    assert(@bitSizeOf(page.Cell) == 64);

    // Field bit offsets — if Ghostty reorders fields, these break
    assert(@bitOffsetOf(page.Cell, "content_tag") == 0);
    assert(@bitOffsetOf(page.Cell, "content") == 2);
    assert(@bitOffsetOf(page.Cell, "style_id") == 26);
    assert(@bitOffsetOf(page.Cell, "wide") == 42);
    assert(@bitOffsetOf(page.Cell, "protected") == 44);
    assert(@bitOffsetOf(page.Cell, "hyperlink") == 45);

    // Enum/tag values — if Ghostty renumbers enums, these break
    assert(@intFromEnum(page.Cell.ContentTag.codepoint) == 0);
    assert(@intFromEnum(page.Cell.ContentTag.codepoint_grapheme) == 1);
    assert(@intFromEnum(page.Cell.ContentTag.bg_color_palette) == 2);
    assert(@intFromEnum(page.Cell.ContentTag.bg_color_rgb) == 3);
    assert(@intFromEnum(page.Cell.Wide.narrow) == 0);
    assert(@intFromEnum(page.Cell.Wide.wide) == 1);
    assert(@intFromEnum(page.Cell.Wide.spacer_tail) == 2);
    assert(@intFromEnum(page.Cell.Wide.spacer_head) == 3);
}
```

### terminal.Style (CellStyle)

```zig
comptime {
    // Struct-level size
    assert(@sizeOf(terminal.Style) == @sizeOf(CellStyle));

    // Field offsets — catches field reordering
    assert(@offsetOf(terminal.Style, "fg_color") == @offsetOf(CellStyle, "fg_color"));
    assert(@offsetOf(terminal.Style, "bg_color") == @offsetOf(CellStyle, "bg_color"));
    assert(@offsetOf(terminal.Style, "underline_color") == @offsetOf(CellStyle, "underline_color"));
    assert(@offsetOf(terminal.Style, "flags") == @offsetOf(CellStyle, "flags"));

    // Color tagged union — verify tag position and payload layout
    assert(@sizeOf(terminal.Style.Color) == @sizeOf(StyleColor));
    assert(@intFromEnum(terminal.Style.Color.Tag.none) == 0);
    assert(@intFromEnum(terminal.Style.Color.Tag.palette) == 1);
    assert(@intFromEnum(terminal.Style.Color.Tag.rgb) == 2);

    // Byte-level verification of Color layout.
    // Catches changes to tag position or payload offset within
    // the tagged union — something @sizeOf/@offsetOf alone cannot.
    {
        const palette_color: terminal.Style.Color = .{ .palette = 0xAB };
        const bytes: [@sizeOf(terminal.Style.Color)]u8 = @bitCast(palette_color);
        assert(bytes[0] == 1);    // tag == .palette
        assert(bytes[1] == 0xAB); // payload at offset 1
    }
    {
        const rgb_color: terminal.Style.Color = .{ .rgb = .{ .r = 0x11, .g = 0x22, .b = 0x33 } };
        const bytes: [@sizeOf(terminal.Style.Color)]u8 = @bitCast(rgb_color);
        assert(bytes[0] == 2);    // tag == .rgb
        assert(bytes[1] == 0x11); // r
        assert(bytes[2] == 0x22); // g
        assert(bytes[3] == 0x33); // b
    }

    // Flags packed struct
    assert(@bitSizeOf(terminal.Style.Flags) == 16);
}
```

### Failure Mode

If any assertion fails, the Zig build produces a compile error. The
Rust side never sees mismatched data. This is strictly a build-time
safety net — there is no runtime cost and no possibility of silent
corruption.

When an assertion does fire (e.g., after updating Ghostty to a new
version), the fix is mechanical: examine what changed, update the
Rust-side struct to match, and verify the assertions pass again.

## Prepaint Integration

The `terminal_element.rs` prepaint phase changes from operating on an
owned `RenderSnapshot` to working directly with `RenderFrame` pointers:

```
CURRENT prepaint:
  snapshot = RenderSnapshot::capture(&mut terminal.lock())
  for row in snapshot.rows:
      build_row_runs(&row_snapshot, ...)

NEW prepaint:
  // Critical lock section
  // render_frame() calls render_update() internally.
  // Non-RenderState fields (scrollbar, is_alternate_screen) MUST 
  // be queried before the MutexGuard drops.
  let (frame, scrollbar, is_alt, ...) = {
      let mut term = terminal.lock();
      let frame = term.render_frame();     // calls render_update() internally
      let scrollbar = term.scrollbar_info();
      let is_alt = term.is_alternate_screen();
      (frame, scrollbar, is_alt, ...)
  }; // MutexGuard dropped here — read thread can call feed() immediately

  // RenderState is stable until next render_update() on the UI thread (here).
  let palette = frame.palette(); // &[ColorRGB; 256] — zero-copy sidecar
  // Reuse PaletteCache when hash unchanged (cached in TerminalElementState).
  let palette_cache = if p_hash == last_hash { cached } else { PaletteCache::from_raw(palette) };

  for y in 0..frame.rows() {
      if !force_full && !frame.row_dirty(y) { continue; }

      let raw_cells = frame.row_raw(y);    // &[RawCell] — zero-copy into Zig memory
      let styles = frame.row_styles(y);    // &[CellStyle] — zero-copy into Zig memory
      let selection = frame.row_selection(y);

      let runs = build_row_runs(raw_cells, styles, selection, y, ..., &frame);
      // ... update row_text_runs, bg_rects
  }
  // frame drops at end of prepaint → dirty flags cleared
```

### `build_row_runs` Signature Change

```rust
// OLD:
pub fn build_row_runs(row_snapshot: &RowSnapshot, ...) -> RowRuns

// NEW:
pub fn build_row_runs(
    raw_cells: &[RawCell],
    styles: &[CellStyle],
    selection: Option<(u16, u16)>,
    row: u16,
    palette: &PaletteCache,
    default_fg: Hsla,
    default_bg: Hsla,
    base_font: &Font,
    font_size: Pixels,
    frame: &RenderFrame,  // for on-demand grapheme lookups
) -> RowRuns
```

Note: `build_cursor` needs a similar change since it uses `RenderSnapshot`

### Style-Conditional Access

A key performance property: for cells with `style_id == 0` (the
default style — the vast majority of terminal content), the styles
array is never accessed. The renderer only reads `styles[col]` when
`raw.style_id() != 0 || raw.is_bg_only()`:

```rust
let style = if raw.style_id() != 0 || raw.is_bg_only() {
    Some(&styles[col])
} else {
    None
};
```

Note: `RenderState.update()` populates `cells_style[x]` for
`bg_color_palette` and `bg_color_rgb` content tags (denormalizing the
bg color into the style slot). So for bg-only cells, the bg color is
available in the style array even though `style_id == 0`. The
`is_bg_only()` check captures this case.

### Color Resolution

Color resolution moves from `FlatCell`-based to `RawCell` + `CellStyle`:

```rust
pub fn resolve_fg(
    style: Option<&CellStyle>,
    palette: &PaletteCache,
    default_fg: Hsla,
) -> Hsla {
    match style {
        None => default_fg,
        Some(s) => match s.fg.tag {
            1 => palette.get(s.fg.r),            // palette index
            2 => rgb_to_hsla(s.fg.r, s.fg.g, s.fg.b),  // direct RGB
            _ => default_fg,                     // none
        },
    }
}

pub fn resolve_bg(
    style: Option<&CellStyle>,
    palette: &PaletteCache,
    default_bg: Hsla,
) -> Hsla {
    // bg_color_* content tags: bg is denormalized into style by RenderState
    // So we just check style uniformly — no need for special raw-cell handling
    match style {
        None => default_bg,
        Some(s) => match s.bg.tag {
            1 => palette.get(s.bg.r),
            2 => rgb_to_hsla(s.bg.r, s.bg.g, s.bg.b),
            _ => default_bg,
        },
    }
}
```

### Flag Accessors

Style flag checks (`is_bold`, `is_inverse`, etc.) move from `FlatCell`
to `CellStyle`. Since `CellStyle.flags` is the same `u16` bitfield as
Ghostty's `Style.Flags`, the bit constants are unchanged:

```rust
// Before: color::is_bold(&flat_cell)
// After:  style.map_or(false, |s| s.is_bold())
```

## Deletions

| File/Type | Status |
|-----------|--------|
| `crates/terminal/src/snapshot.rs` | **Delete** |
| `RenderSnapshot` | **Delete** |
| `RowSnapshot` | **Delete** |
| `FlatCell` (Zig + Rust) | **Delete** |
| `TerminalHandle.flat_cells` | **Delete** |
| `TerminalHandle.ensureFlatCells()` | **Delete** |
| `flattenStyleColor()` | **Delete** |
| `ghostty_vt_terminal_render_row_cells()` | **Delete** (replaced by `_row_raw` + `_row_styles`) |
| `ghostty_vt_terminal_render_palette_color()` | **Delete** (replaced by `_palette`) |
| `color::is_bold(&FlatCell)` etc. | **Move** to `CellStyle` methods |

## New/Modified Files

### New (Zig side, `render.zig`)
- `CellStyle` extern struct — C-safe mirror of `terminal.Style`
- `StyleColor` extern struct — C-safe mirror of `terminal.Style.Color`

### New (Zig side, `handle.zig`)
- `PaletteColor` extern struct — C-safe palette entry `{r, g, b}`, `@sizeOf==3`

### New (Rust side, `ghostty-vt/src/types.rs`)
- `RawCell` — `#[repr(transparent)]` u64 wrapper for `page.Cell`
- `CellStyle` — `#[repr(C)]` struct matching `terminal.Style` layout
- `StyleColor` — `#[repr(C)]` struct matching `terminal.Style.Color` layout
- Note: `ColorRGB` already exists and serves as the Rust-side palette entry type (maps 1:1 to `PaletteColor`). No separate `PaletteColor` Rust type is introduced.

### Modified

| File | Change |
|------|--------|
| `crates/ghostty-vt/zig/render.zig` | Add `_row_raw`, `_row_styles`, `_palette`; remove `_row_cells`, `flattenStyleColor`; add comptime assertions |
| `crates/ghostty-vt/zig/handle.zig` | Add `PaletteColor`, `palette_cache: [256]PaletteColor`, `palette_dirty: bool`; remove `FlatCell`, `flat_cells`, `ensureFlatCells` |
| `crates/ghostty-vt/src/terminal.rs` | Detached `RenderFrame { handle }` + `render_frame()`; add `row_raw()`, `row_styles()`, `palette()`, `cell_grapheme()` (returns `&[u32]`); remove `row_cells()`, `palette_color()`, `begin_frame()`, `frame_active` |
| `crates/ghostty-vt/src/types.rs` | Add `RawCell`, `CellStyle`, `StyleColor`; remove `FlatCell`. `ColorRGB` is the Rust-side palette entry (no `PaletteColor` Rust type). |
| `crates/ghostty-vt/src/lib.rs` | Update FFI declarations and re-exports |
| `crates/renderer/src/color.rs` | Rewrite `resolve_fg`/`resolve_bg` for `CellStyle`; add `#[derive(Clone)]` to `PaletteCache`; remove `FlatCell` flag accessors |
| `crates/renderer/src/text_runs.rs` | Change `build_row_runs` signature to take slices + `&RenderFrame`; remove `RowSnapshot` dependency |
| `crates/renderer/src/terminal_element.rs` | Rewrite prepaint: scoped lock → `render_frame()` → unlock → lock-free frame access; `PaletteCache` cached in `TerminalElementState` |
| `crates/terminal/src/snapshot.rs` | **Delete** |

---

## Implementation Verification

Before writing the Rust-side structs, run a Zig comptime probe to dump
the exact memory layout. Add a temporary exported function or comptime
block in `render.zig` that prints/asserts the following:

### Step 1: Probe Style.Color layout

```zig
comptime {
    @compileLog("@sizeOf(Style.Color) =", @sizeOf(terminal.Style.Color));
    @compileLog("@alignOf(Style.Color) =", @alignOf(terminal.Style.Color));

    // Construct known values and inspect bytes
    const none_color: terminal.Style.Color = .none;
    const none_bytes: [@sizeOf(terminal.Style.Color)]u8 = @bitCast(none_color);
    @compileLog("none bytes =", none_bytes);

    const pal_color: terminal.Style.Color = .{ .palette = 0xAB };
    const pal_bytes: [@sizeOf(terminal.Style.Color)]u8 = @bitCast(pal_color);
    @compileLog("palette(0xAB) bytes =", pal_bytes);

    const rgb_color: terminal.Style.Color = .{ .rgb = .{ .r = 0x11, .g = 0x22, .b = 0x33 } };
    const rgb_bytes: [@sizeOf(terminal.Style.Color)]u8 = @bitCast(rgb_color);
    @compileLog("rgb(11,22,33) bytes =", rgb_bytes);
}
```

This tells us:
- Size and alignment of Color
- Whether tag is at offset 0 or at the end
- Payload byte positions
- Any padding bytes

### Step 2: Probe Style layout

```zig
comptime {
    @compileLog("@sizeOf(Style) =", @sizeOf(terminal.Style));
    @compileLog("@alignOf(Style) =", @alignOf(terminal.Style));
    @compileLog("@offsetOf(Style, fg_color) =", @offsetOf(terminal.Style, "fg_color"));
    @compileLog("@offsetOf(Style, bg_color) =", @offsetOf(terminal.Style, "bg_color"));
    @compileLog("@offsetOf(Style, underline_color) =", @offsetOf(terminal.Style, "underline_color"));
    @compileLog("@offsetOf(Style, flags) =", @offsetOf(terminal.Style, "flags"));

    // Full struct byte dump with known values
    const style: terminal.Style = .{
        .fg_color = .{ .palette = 0xAA },
        .bg_color = .{ .rgb = .{ .r = 0x11, .g = 0x22, .b = 0x33 } },
        .underline_color = .none,
        .flags = @bitCast(@as(u16, 0xFFFF)),
    };
    const style_bytes: [@sizeOf(terminal.Style)]u8 = @bitCast(style);
    @compileLog("style bytes =", style_bytes);
}
```

This tells us:
- Total struct size
- Whether Zig reordered the fields
- Padding between fields
- Exact byte positions of every component

### Step 3: Probe page.Cell bit layout

Verify the bit positions assumed by `RawCell`:

```zig
comptime {
    @compileLog("@bitOffsetOf(Cell, content_tag) =", @bitOffsetOf(page.Cell, "content_tag"));
    @compileLog("@bitOffsetOf(Cell, content) =", @bitOffsetOf(page.Cell, "content"));
    @compileLog("@bitOffsetOf(Cell, style_id) =", @bitOffsetOf(page.Cell, "style_id"));
    @compileLog("@bitOffsetOf(Cell, wide) =", @bitOffsetOf(page.Cell, "wide"));
    @compileLog("@bitOffsetOf(Cell, protected) =", @bitOffsetOf(page.Cell, "protected"));
    @compileLog("@bitOffsetOf(Cell, hyperlink) =", @bitOffsetOf(page.Cell, "hyperlink"));
    @compileLog("@bitOffsetOf(Cell, semantic_content) =", @bitOffsetOf(page.Cell, "semantic_content"));

    // Verify enum values
    @compileLog("ContentTag.codepoint =", @intFromEnum(page.Cell.ContentTag.codepoint));
    @compileLog("ContentTag.codepoint_grapheme =", @intFromEnum(page.Cell.ContentTag.codepoint_grapheme));
    @compileLog("ContentTag.bg_color_palette =", @intFromEnum(page.Cell.ContentTag.bg_color_palette));
    @compileLog("ContentTag.bg_color_rgb =", @intFromEnum(page.Cell.ContentTag.bg_color_rgb));
    @compileLog("Wide.narrow =", @intFromEnum(page.Cell.Wide.narrow));
    @compileLog("Wide.spacer_tail =", @intFromEnum(page.Cell.Wide.spacer_tail));
}
```

### Step 4: Probe palette entry layout

```zig
comptime {
    @compileLog("@sizeOf(color.RGB) =", @sizeOf(color.RGB));
    @compileLog("@alignOf(color.RGB) =", @alignOf(color.RGB));

    const rgb: color.RGB = .{ .r = 0xAA, .g = 0xBB, .b = 0xCC };
    const rgb_bytes: [@sizeOf(color.RGB)]u8 = @bitCast(rgb);
    @compileLog("RGB(AA,BB,CC) bytes =", rgb_bytes);
}
```

### Step 5: Construct Rust structs from probe results

Use the byte dumps to construct `CellStyle` and `StyleColor` with
the exact field order, sizes, and padding. Then convert the
`@compileLog` probes into permanent comptime assertions (see "ABI
Safety" section).

### Step 6: End-to-end validation

Write a Zig-side test function that constructs a `Style` with known
values, casts it to `[@sizeOf(Style)]u8`, and returns those bytes
across FFI. On the Rust side, cast the same bytes to `CellStyle` and
assert every accessor returns the expected value. This validates the
full round-trip.

---

## What Becomes Redundant

This rework eliminates several layers that exist only because we
couldn't safely access Zig memory from Rust:

### Types

| Type | Why it existed | Why it's redundant |
|------|---------------|--------------------|
| `FlatCell` (Zig) | Denormalized cell for C ABI crossing | `RawCell` reads `page.Cell` directly; `CellStyle` reads `Style` directly |
| `FlatCell` (Rust) | Mirror of Zig FlatCell | Same |
| `RenderSnapshot` | Owned copy of all render data for lock-free rendering | `RenderFrame` provides direct access; mutex isn't needed for reads |
| `RowSnapshot` | Per-row owned cell data + graphemes | Replaced by `&[RawCell]` + `&[CellStyle]` slices |
| `StyleFlags` type alias (handle.zig) | Used only by `FlatCell` comptime assert | Goes away with `FlatCell` |

### Functions / Methods

| Function | Why it's redundant |
|----------|--------------------|
| `flattenStyleColor()` | Style colors accessed directly via `CellStyle` |
| `TerminalHandle.ensureFlatCells()` | No scratch buffer needed |
| `ghostty_vt_terminal_render_row_cells()` | Replaced by `_row_raw` + `_row_styles` |
| `ghostty_vt_terminal_render_palette_color()` | Replaced by `_palette` (batch) |
| `RenderSnapshot::capture()` | Inlined into prepaint as direct `RenderFrame` access |
| `RenderFrame::row_cells()` | Replaced by `row_raw()` + `row_styles()` |
| `RenderFrame::palette_color()` | Replaced by `palette()` |
| `color::is_bold(&FlatCell)` etc. | Replaced by `CellStyle::is_bold()` etc. |
| `color::resolve_fg(&FlatCell, ...)` | Replaced by `resolve_fg(Option<&CellStyle>, ...)` |
| `color::resolve_bg(&FlatCell, ...)` | Replaced by `resolve_bg(Option<&CellStyle>, ...)` |

### Allocations Eliminated

| Allocation | Per-frame cost | Eliminated by |
|------------|---------------|---------------|
| `flat_cells` scratch buffer | `alloc(FlatCell, cols)` on first use | Direct `page.Cell` pointer |
| `FlatCell` flatten loop | O(cols) per row, all rows | Direct `page.Cell` pointer |
| `RowSnapshot.cells: Vec<FlatCell>` | `to_vec()` per row | Direct slice |
| `RowSnapshot.graphemes: Vec<(u16, Vec<u32>)>` | Clone per grapheme cell | On-demand scratch |
| `palette: [ColorRGB; 256]` copy | 768 bytes per frame | `PaletteColor` sidecar, only on change |
| `RenderSnapshot.rows: Vec<RowSnapshot>` | Vec of Vecs per frame | No snapshot |

### Fields Removed from `TerminalHandle`

| Field | Reason |
|-------|--------|
| `flat_cells: []FlatCell` | No flatten step |
| `grapheme_buf: []u32` | Kept (still needed for u21→u32 widening) |

Note: `grapheme_buf` stays — it's the only remaining scratch buffer,
used for the rare grapheme path where Zig's `[]const u21` must be
widened to `[*]const u32` for Rust.

## Risks and Resolutions

**Zig tagged union ABI drift.** `terminal.Style.Color` is a Zig tagged
union whose in-memory layout is not part of any formal ABI guarantee.
If a future Zig compiler version changes how tagged unions are laid out,
the comptime byte-level assertions will fail at build time. The fix is
mechanical: update `CellStyle`/`StyleColor` to match the new layout.
No runtime corruption is possible — the build simply won't succeed
until the mismatch is fixed.

**`page.Cell` packed struct reordering.** If Ghostty reorders fields
in the `packed struct(u64)`, `RawCell`'s bit extraction methods will
return wrong values. The `@bitOffsetOf` comptime assertions catch this
at build time. Same mechanical fix process.

**Palette padding.** Ghostty's `color.RGB` is `packed struct(u24)` with
`@sizeOf == 4` (1 byte padding). Direct pointer cast to a 3-byte Rust
struct would misalign. The palette sidecar (`[256]PaletteColor`) avoids
this with a trivial conversion that only runs when the palette changes.

**Grapheme scratch buffer.** The `cell_grapheme()` API uses a
per-handle scratch buffer. The returned slice is valid only until the
next `cell_grapheme()` call. The caller must consume it immediately
(which `build_row_runs` does — it pushes grapheme chars into the
cell's text string inline).

---

## Future Optimizations

**CursorState becomes obsolete.** The current `ghostty_vt_terminal_render_cursor()`
function copies cursor data from `RenderState.cursor` into a `CursorState`
struct for FFI. Since `_row_raw()` and `_row_styles()` now provide direct
pointer access to cell and style data, the cursor's `cell` and `style` fields
can be accessed via those functions at the cursor position. The remaining
scalar fields (position, style, visibility) can be exposed via lightweight
getter functions returning raw values directly, eliminating the `CursorState`
struct entirely.
