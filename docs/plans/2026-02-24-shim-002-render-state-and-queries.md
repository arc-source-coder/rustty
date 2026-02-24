# shim-002: Render State + Query Exports

**Goal:** Export RenderState lifecycle, dirty tracking, cell data (flat C struct), mode flag queries, viewport scroll, selection, and key/mouse encoding from the Zig shim — completing the full C ABI surface needed by `ffi-001`.

**Architecture:** Each functional group adds `export fn` functions to `lib.zig` that unwrap the opaque `TerminalHandle`, access Ghostty internals, and return data through C-safe types. Cell data is flattened from Ghostty's `MultiArrayList(Cell)` into a contiguous `[*]FlatCell` array per row. Mouse encoding is implemented in the shim (~60 lines) to match key encoding's pattern of encapsulating protocol details at the Zig boundary.

**Tech Stack:** Zig 0.15.2, Ghostty 1.3.x internals (`terminal.RenderState`, `terminal.Terminal`, `input.key_encode`), C ABI FFI

**Refs:**

- `docs/architecture/06-ghostty-shim.md` — RenderState, Shim C ABI Surface, Key Encoding
- `docs/architecture/03-rendering.md` — Dirty and Cache Model
- `crates/ghostty-vt/zig/ghostty/src/terminal/render.zig` — RenderState, Cell, Row, Dirty, Colors, Cursor
- `crates/ghostty-vt/zig/ghostty/src/terminal/Terminal.zig` — flags, modes, ScrollViewport
- `crates/ghostty-vt/zig/ghostty/src/input/key_encode.zig` — encode(), Options.fromTerminal()
- `crates/ghostty-vt/zig/ghostty/src/Surface.zig:3591-3815` — mouseReport() (reference for encode_mouse)

---

## Conventions

All new exports follow the existing pattern in `lib.zig`:

- Handle is `?*anyopaque`, unwrapped with `@ptrCast(@alignCast(ptr.?))` after null check
- Use `callconv(.c)` (lowercase, Zig 0.15.2)
- Error returns: `c_int` (0=ok, 1=null handle, 2=internal error)
- Build: `cargo test -p ghostty_vt` runs the full suite (Zig build + Rust tests)
- Lint: `ziglint crates/ghostty-vt/zig/` + `zig fmt crates/ghostty-vt/zig/lib.zig`

Each part is independently implementable and testable. Parts should be done in order (earlier parts are prerequisites for later ones, except Parts 2 and 3 which are independent).

---

## Part 1: Mode Flag Queries

**Goal:** Export direct reads of terminal mode/flag state. Simplest possible exports — warm-up.

**Files:**

- Modify: `crates/ghostty-vt/zig/lib.zig`
- Modify: `crates/ghostty-vt/src/lib.rs`

### Step 1: Add Zig exports

```zig
// crates/ghostty-vt/zig/lib.zig — append after ghostty_vt_terminal_resize

/// Returns mouse event mode: 0=none, 1=x10, 2=normal, 3=button, 4=any
export fn ghostty_vt_terminal_get_mouse_mode(ptr: ?*anyopaque) callconv(.c) u8 {
    if (ptr == null) return 0;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    return @intFromEnum(handle.terminal_inst.flags.mouse_event);
}

/// Returns mouse format: 0=x10, 1=utf8, 2=sgr, 3=urxvt, 4=sgr_pixels
export fn ghostty_vt_terminal_get_mouse_format(ptr: ?*anyopaque) callconv(.c) u8 {
    if (ptr == null) return 0;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    return @intFromEnum(handle.terminal_inst.flags.mouse_format);
}

/// Returns 1 if bracketed paste mode is active, 0 otherwise
export fn ghostty_vt_terminal_is_bracketed_paste(ptr: ?*anyopaque) callconv(.c) u8 {
    if (ptr == null) return 0;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    return @intFromBool(handle.terminal_inst.modes.get(.bracketed_paste));
}

/// Returns kitty keyboard flags as a u8 bitfield (5 bits used)
export fn ghostty_vt_terminal_get_kitty_keyboard_flags(ptr: ?*anyopaque) callconv(.c) u8 {
    if (ptr == null) return 0;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    return @bitCast(handle.terminal_inst.screens.active.kitty_keyboard.current());
}
```

### Step 2: Add Rust extern declarations

```rust
// crates/ghostty-vt/src/lib.rs — inside the unsafe extern "C" block

pub fn ghostty_vt_terminal_get_mouse_mode(terminal: *mut c_void) -> u8;
pub fn ghostty_vt_terminal_get_mouse_format(terminal: *mut c_void) -> u8;
pub fn ghostty_vt_terminal_is_bracketed_paste(terminal: *mut c_void) -> u8;
pub fn ghostty_vt_terminal_get_kitty_keyboard_flags(terminal: *mut c_void) -> u8;
```

### Step 3: Add Rust tests

```rust
#[test]
fn test_mode_flags_default() {
    let ptr = unsafe { ghostty_vt_terminal_new(80, 24) };
    // Defaults: no mouse, no bracketed paste, no kitty flags
    assert_eq!(unsafe { ghostty_vt_terminal_get_mouse_mode(ptr) }, 0);
    assert_eq!(unsafe { ghostty_vt_terminal_get_mouse_format(ptr) }, 0);
    assert_eq!(unsafe { ghostty_vt_terminal_is_bracketed_paste(ptr) }, 0);
    assert_eq!(unsafe { ghostty_vt_terminal_get_kitty_keyboard_flags(ptr) }, 0);
    unsafe { ghostty_vt_terminal_free(ptr) };
}

#[test]
fn test_bracketed_paste_enabled() {
    let ptr = unsafe { ghostty_vt_terminal_new(80, 24) };
    // CSI ?2004h enables bracketed paste
    let seq = b"\x1b[?2004h";
    unsafe { ghostty_vt_terminal_feed(ptr, seq.as_ptr(), seq.len()) };
    assert_eq!(unsafe { ghostty_vt_terminal_is_bracketed_paste(ptr) }, 1);
    // CSI ?2004l disables it
    let seq_off = b"\x1b[?2004l";
    unsafe { ghostty_vt_terminal_feed(ptr, seq_off.as_ptr(), seq_off.len()) };
    assert_eq!(unsafe { ghostty_vt_terminal_is_bracketed_paste(ptr) }, 0);
    unsafe { ghostty_vt_terminal_free(ptr) };
}

#[test]
fn test_mouse_mode_enabled() {
    let ptr = unsafe { ghostty_vt_terminal_new(80, 24) };
    // CSI ?1003h enables any-event mouse tracking
    let seq = b"\x1b[?1003h";
    unsafe { ghostty_vt_terminal_feed(ptr, seq.as_ptr(), seq.len()) };
    assert_eq!(unsafe { ghostty_vt_terminal_get_mouse_mode(ptr) }, 4); // any=4
    // CSI ?1006h enables SGR format
    let seq2 = b"\x1b[?1006h";
    unsafe { ghostty_vt_terminal_feed(ptr, seq2.as_ptr(), seq2.len()) };
    assert_eq!(unsafe { ghostty_vt_terminal_get_mouse_format(ptr) }, 2); // sgr=2
    unsafe { ghostty_vt_terminal_free(ptr) };
}
```

### Step 4: Build and test

```bash
cargo test -p ghostty_vt
```

---

## Part 2: Viewport Scroll

**Goal:** Export viewport scrolling commands. The terminal owns viewport state; these mutate it.

**Files:**

- Modify: `crates/ghostty-vt/zig/lib.zig`
- Modify: `crates/ghostty-vt/src/lib.rs`

**Depends on:** Part 1 (convention established, not functionally required)

### Step 1: Add Zig exports

```zig
/// Scroll the viewport by delta rows (negative = up/towards history, positive = down)
export fn ghostty_vt_terminal_scroll_viewport(ptr: ?*anyopaque, delta: i32) callconv(.c) void {
    if (ptr == null) return;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    handle.terminal_inst.scrollViewport(.{ .delta = @intCast(delta) });
}

/// Scroll the viewport to the top of scrollback
export fn ghostty_vt_terminal_scroll_viewport_top(ptr: ?*anyopaque) callconv(.c) void {
    if (ptr == null) return;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    handle.terminal_inst.scrollViewport(.top);
}

/// Scroll the viewport to the bottom (active area)
export fn ghostty_vt_terminal_scroll_viewport_bottom(ptr: ?*anyopaque) callconv(.c) void {
    if (ptr == null) return;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    handle.terminal_inst.scrollViewport(.bottom);
}
```

### Step 2: Add Rust extern declarations

```rust
pub fn ghostty_vt_terminal_scroll_viewport(terminal: *mut c_void, delta: i32);
pub fn ghostty_vt_terminal_scroll_viewport_top(terminal: *mut c_void);
pub fn ghostty_vt_terminal_scroll_viewport_bottom(terminal: *mut c_void);
```

### Step 3: Add Rust tests

```rust
#[test]
fn test_scroll_viewport() {
    let ptr = unsafe { ghostty_vt_terminal_new(80, 24) };
    // Generate scrollback: 50 newlines pushes content above viewport
    let newlines = "\n".repeat(50);
    unsafe { ghostty_vt_terminal_feed(ptr, newlines.as_ptr(), newlines.len()) };
    // These should not crash — scroll up, to top, to bottom
    unsafe { ghostty_vt_terminal_scroll_viewport(ptr, -5) };
    unsafe { ghostty_vt_terminal_scroll_viewport_top(ptr) };
    unsafe { ghostty_vt_terminal_scroll_viewport_bottom(ptr) };
    unsafe { ghostty_vt_terminal_free(ptr) };
}
```

### Step 4: Build and test

```bash
cargo test -p ghostty_vt
```

---

## Part 3: Render Core — Dimensions, Dirty, Update, Clear

**Goal:** Export `render_update`, `render_dirty`, `render_clear_dirty`, `render_rows`, `render_cols`. These are the frame lifecycle functions the Rust `terminal` crate calls every drain cycle.

**Files:**

- Modify: `crates/ghostty-vt/zig/lib.zig`
- Modify: `crates/ghostty-vt/src/lib.rs`

**Depends on:** Part 1

### Step 1: Add Zig exports

```zig
/// Update the persistent RenderState from current terminal state.
/// Returns 0 on success, 1 if null handle, 2 on allocation error.
export fn ghostty_vt_terminal_render_update(ptr: ?*anyopaque) callconv(.c) c_int {
    if (ptr == null) return 1;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    handle.render_state.update(handle.alloc, &handle.terminal_inst) catch return 2;
    return 0;
}

/// Returns dirty state: 0=false, 1=partial, 2=full
export fn ghostty_vt_terminal_render_dirty(ptr: ?*anyopaque) callconv(.c) u8 {
    if (ptr == null) return 0;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    return switch (handle.render_state.dirty) {
        .false => 0,
        .partial => 1,
        .full => 2,
    };
}

/// Clear the dirty state (call after rendering)
export fn ghostty_vt_terminal_render_clear_dirty(ptr: ?*anyopaque) callconv(.c) void {
    if (ptr == null) return;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    handle.render_state.dirty = .false;
    // Also clear per-row dirty flags
    for (handle.render_state.row_data.items(.dirty)) |*d| {
        d.* = false;
    }
}

/// Returns number of rows in the current render state
export fn ghostty_vt_terminal_render_rows(ptr: ?*anyopaque) callconv(.c) u16 {
    if (ptr == null) return 0;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    return handle.render_state.rows;
}

/// Returns number of columns in the current render state
export fn ghostty_vt_terminal_render_cols(ptr: ?*anyopaque) callconv(.c) u16 {
    if (ptr == null) return 0;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    return handle.render_state.cols;
}

/// Returns 1 if the given row is dirty, 0 otherwise
export fn ghostty_vt_terminal_render_row_dirty(ptr: ?*anyopaque, row: u16) callconv(.c) u8 {
    if (ptr == null) return 0;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    if (row >= handle.render_state.rows) return 0;
    return @intFromBool(handle.render_state.row_data.items(.dirty)[row]);
}
```

### Step 2: Add Rust extern declarations

```rust
pub fn ghostty_vt_terminal_render_update(terminal: *mut c_void) -> c_int;
pub fn ghostty_vt_terminal_render_dirty(terminal: *mut c_void) -> u8;
pub fn ghostty_vt_terminal_render_clear_dirty(terminal: *mut c_void);
pub fn ghostty_vt_terminal_render_rows(terminal: *mut c_void) -> u16;
pub fn ghostty_vt_terminal_render_cols(terminal: *mut c_void) -> u16;
pub fn ghostty_vt_terminal_render_row_dirty(terminal: *mut c_void, row: u16) -> u8;
```

### Step 3: Add Rust tests

```rust
#[test]
fn test_render_update_empty() {
    let ptr = unsafe { ghostty_vt_terminal_new(80, 24) };
    let rc = unsafe { ghostty_vt_terminal_render_update(ptr) };
    assert_eq!(rc, 0);
    // First update is always full dirty
    assert_eq!(unsafe { ghostty_vt_terminal_render_dirty(ptr) }, 2);
    assert_eq!(unsafe { ghostty_vt_terminal_render_rows(ptr) }, 24);
    assert_eq!(unsafe { ghostty_vt_terminal_render_cols(ptr) }, 80);
    unsafe { ghostty_vt_terminal_render_clear_dirty(ptr) };
    assert_eq!(unsafe { ghostty_vt_terminal_render_dirty(ptr) }, 0);
    unsafe { ghostty_vt_terminal_free(ptr) };
}

#[test]
fn test_render_partial_dirty() {
    let ptr = unsafe { ghostty_vt_terminal_new(80, 24) };
    // First update → full dirty
    unsafe { ghostty_vt_terminal_render_update(ptr) };
    unsafe { ghostty_vt_terminal_render_clear_dirty(ptr) };
    // Feed some text — only touched rows should be dirty
    let text = b"Hello";
    unsafe { ghostty_vt_terminal_feed(ptr, text.as_ptr(), text.len()) };
    unsafe { ghostty_vt_terminal_render_update(ptr) };
    let dirty = unsafe { ghostty_vt_terminal_render_dirty(ptr) };
    assert!(dirty > 0); // partial or full
    // Row 0 should be dirty (cursor starts at 0,0)
    assert_eq!(unsafe { ghostty_vt_terminal_render_row_dirty(ptr, 0) }, 1);
    unsafe { ghostty_vt_terminal_free(ptr) };
}
```

### Step 4: Build and test

```bash
cargo test -p ghostty_vt
```

---

## Part 4: Render Core — Cursor and Colors

**Goal:** Export cursor state and color palette as C-safe structs.

**Files:**

- Modify: `crates/ghostty-vt/zig/lib.zig`
- Modify: `crates/ghostty-vt/src/lib.rs`

**Depends on:** Part 3

### Step 1: Define C-safe cursor and color structs in Zig

```zig
// crates/ghostty-vt/zig/lib.zig — add above the export functions

/// C-safe cursor state
const CursorState = extern struct {
    /// Cursor position in viewport coordinates. If not visible in viewport,
    /// x and y are set to active-area coordinates and in_viewport is 0.
    x: u16,
    y: u16,
    in_viewport: u8,
    /// Visual style: 0=bar, 1=block, 2=underline, 3=block_hollow
    style: u8,
    visible: u8,
    blinking: u8,
    password_input: u8,
    /// 1 if cursor is on the tail half of a wide char
    wide_tail: u8,
};

/// C-safe color triplet
const ColorRGB = extern struct {
    r: u8,
    g: u8,
    b: u8,
};

/// C-safe terminal color state
const ColorState = extern struct {
    background: ColorRGB,
    foreground: ColorRGB,
    /// Cursor color; if has_cursor_color is 0, cursor_color is undefined
    cursor_color: ColorRGB,
    has_cursor_color: u8,
};
```

### Step 2: Add Zig exports

```zig
/// Get cursor state from the current render state
export fn ghostty_vt_terminal_render_cursor(ptr: ?*anyopaque, out: ?*CursorState) callconv(.c) c_int {
    if (ptr == null or out == null) return 1;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    const c = &handle.render_state.cursor;
    const result = out.?;

    if (c.viewport) |vp| {
        result.x = vp.x;
        result.y = vp.y;
        result.in_viewport = 1;
        result.wide_tail = @intFromBool(vp.wide_tail);
    } else {
        result.x = c.active.x;
        result.y = @intCast(c.active.y);
        result.in_viewport = 0;
        result.wide_tail = 0;
    }

    result.style = switch (c.visual_style) {
        .bar => 0,
        .block => 1,
        .underline => 2,
        .block_hollow => 3,
    };
    result.visible = @intFromBool(c.visible);
    result.blinking = @intFromBool(c.blinking);
    result.password_input = @intFromBool(c.password_input);
    return 0;
}

/// Get terminal colors from the current render state.
/// Palette access is separate (256 entries is too large for a return struct).
export fn ghostty_vt_terminal_render_colors(ptr: ?*anyopaque, out: ?*ColorState) callconv(.c) c_int {
    if (ptr == null or out == null) return 1;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    const colors = &handle.render_state.colors;
    const result = out.?;

    result.background = .{ .r = colors.background.r, .g = colors.background.g, .b = colors.background.b };
    result.foreground = .{ .r = colors.foreground.r, .g = colors.foreground.g, .b = colors.foreground.b };

    if (colors.cursor) |cc| {
        result.cursor_color = .{ .r = cc.r, .g = cc.g, .b = cc.b };
        result.has_cursor_color = 1;
    } else {
        result.has_cursor_color = 0;
    }

    return 0;
}

/// Get a palette color by index (0–255). Returns the RGB via out pointer.
export fn ghostty_vt_terminal_render_palette_color(
    ptr: ?*anyopaque,
    index: u8,
    out: ?*ColorRGB,
) callconv(.c) c_int {
    if (ptr == null or out == null) return 1;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    const rgb = handle.render_state.colors.palette[index];
    const result = out.?;
    result.* = .{ .r = rgb.r, .g = rgb.g, .b = rgb.b };
    return 0;
}
```

### Step 3: Add Rust extern declarations and repr(C) types

```rust
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CursorState {
    pub x: u16,
    pub y: u16,
    pub in_viewport: u8,
    pub style: u8,
    pub visible: u8,
    pub blinking: u8,
    pub password_input: u8,
    pub wide_tail: u8,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ColorRGB {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct ColorState {
    pub background: ColorRGB,
    pub foreground: ColorRGB,
    pub cursor_color: ColorRGB,
    pub has_cursor_color: u8,
}

// In the extern block:
pub fn ghostty_vt_terminal_render_cursor(
    terminal: *mut c_void,
    out: *mut CursorState,
) -> c_int;
pub fn ghostty_vt_terminal_render_colors(
    terminal: *mut c_void,
    out: *mut ColorState,
) -> c_int;
pub fn ghostty_vt_terminal_render_palette_color(
    terminal: *mut c_void,
    index: u8,
    out: *mut ColorRGB,
) -> c_int;
```

### Step 4: Add Rust tests

```rust
#[test]
fn test_render_cursor() {
    let ptr = unsafe { ghostty_vt_terminal_new(80, 24) };
    unsafe { ghostty_vt_terminal_render_update(ptr) };
    let mut cursor = CursorState::default();
    let rc = unsafe { ghostty_vt_terminal_render_cursor(ptr, &mut cursor) };
    assert_eq!(rc, 0);
    assert_eq!(cursor.x, 0);
    assert_eq!(cursor.y, 0);
    assert_eq!(cursor.in_viewport, 1);
    assert_eq!(cursor.visible, 1);
    assert_eq!(cursor.style, 1); // block
    unsafe { ghostty_vt_terminal_free(ptr) };
}

#[test]
fn test_render_colors() {
    let ptr = unsafe { ghostty_vt_terminal_new(80, 24) };
    unsafe { ghostty_vt_terminal_render_update(ptr) };
    let mut colors = ColorState::default();
    let rc = unsafe { ghostty_vt_terminal_render_colors(ptr, &mut colors) };
    assert_eq!(rc, 0);
    // Default colors should be set (exact values depend on Ghostty defaults)
    unsafe { ghostty_vt_terminal_free(ptr) };
}

#[test]
fn test_render_palette() {
    let ptr = unsafe { ghostty_vt_terminal_new(80, 24) };
    unsafe { ghostty_vt_terminal_render_update(ptr) };
    let mut color = ColorRGB::default();
    let rc = unsafe { ghostty_vt_terminal_render_palette_color(ptr, 1, &mut color) };
    assert_eq!(rc, 0);
    // Palette index 1 is red in default xterm palette
    assert_eq!(color.r, 205);
    assert_eq!(color.g, 0);
    assert_eq!(color.b, 0);
    unsafe { ghostty_vt_terminal_free(ptr) };
}
```

### Step 5: Build and test

```bash
cargo test -p ghostty_vt
```

---

## Part 5: Render Core — Row Cells (FlatCell)

**Goal:** Export row cell data as a contiguous array of flat C structs. This is the most complex part — the renderer iterates these to build style runs.

**Files:**

- Modify: `crates/ghostty-vt/zig/lib.zig`
- Modify: `crates/ghostty-vt/src/lib.rs`

**Depends on:** Part 3

### Design: FlatCell Layout

Each `RenderState.Cell` has:

- `raw: page.Cell` — packed u64 with codepoint (u21), content_tag, wide, style_id, etc.
- `grapheme: []u21` — extra codepoints for grapheme clusters
- `style: Style` — fg/bg/underline colors + flags (bold, italic, etc.)

We flatten this into a C-safe struct. Grapheme data is provided as a
separate codepoint array per cell (pointer + length). This is safe because
the grapheme data lives in the RenderState's per-row arena until the next
`render_update()`.

### Step 1: Define FlatCell struct in Zig

```zig
/// C-safe flattened cell for rendering
const FlatCell = extern struct {
    /// Primary codepoint (0 = empty cell)
    codepoint: u32,
    /// Number of extra codepoints in the grapheme cluster (0 for simple chars)
    grapheme_len: u8,
    /// Wide property: 0=narrow, 1=wide, 2=spacer_tail, 3=spacer_head
    wide: u8,

    // --- Style (resolved from style.Style) ---
    /// Foreground color type: 0=none/default, 1=palette, 2=rgb
    fg_color_type: u8,
    fg_r: u8,
    fg_g: u8,
    fg_b: u8,
    fg_palette: u8,

    /// Background color type: 0=none/default, 1=palette, 2=rgb
    /// Note: for bg_color_palette/bg_color_rgb content_tags, bg is set
    /// from the cell content directly (not from style).
    bg_color_type: u8,
    bg_r: u8,
    bg_g: u8,
    bg_b: u8,
    bg_palette: u8,

    /// Underline color type: 0=none, 1=palette, 2=rgb
    ul_color_type: u8,
    ul_r: u8,
    ul_g: u8,
    ul_b: u8,
    ul_palette: u8,

    /// Style flags packed into a u16 matching Zig's Style.Flags layout:
    /// bit 0: bold, 1: italic, 2: faint, 3: blink, 4: inverse,
    /// 5: invisible, 6: strikethrough, 7: overline
    /// bits 8-10: underline (0=none,1=single,2=double,3=curly,4=dotted,5=dashed)
    style_flags: u16,

    _padding: [2]u8,

    const StyleFlags = @TypeOf((@as(terminal.Style, .{})).flags);
    // Verify ABI stability at compile time
    comptime {
        std.debug.assert(@sizeOf(FlatCell) == 28);
        std.debug.assert(@alignOf(FlatCell) == 4);
        // Verify style_flags bitcast stays u16-sized without referencing
        // private Ghostty internals by name.
        std.debug.assert(@bitSizeOf(StyleFlags) == 16);
    }
};
```

### Step 2: Add helper to flatten a style color

```zig
fn flattenStyleColor(c: terminal.Style.Color) struct { color_type: u8, r: u8, g: u8, b: u8, palette: u8 } {
    return switch (c) {
        .none => .{ .color_type = 0, .r = 0, .g = 0, .b = 0, .palette = 0 },
        .palette => |p| .{ .color_type = 1, .r = 0, .g = 0, .b = 0, .palette = p },
        .rgb => |rgb| .{ .color_type = 2, .r = rgb.r, .g = rgb.g, .b = rgb.b, .palette = 0 },
    };
}
```

### Step 3: Add row cells export

```zig
/// Get flattened cell data for a row. Returns pointer to `cols` FlatCell entries.
/// The returned pointer is valid until the next render_update() or terminal mutation.
/// Returns null if row is out of bounds.
///
/// Grapheme codepoints (for cells with grapheme_len > 0) can be retrieved
/// via ghostty_vt_terminal_render_cell_grapheme().
export fn ghostty_vt_terminal_render_row_cells(
    ptr: ?*anyopaque,
    row: u16,
    out_len: ?*u16,
) callconv(.c) ?[*]const FlatCell {
    if (ptr == null) return null;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    if (row >= handle.render_state.rows) return null;

    const cells = handle.render_state.row_data.items(.cells)[row];
    const raws = cells.items(.raw);
    const styles = cells.items(.style);
    const graphemes = cells.items(.grapheme);

    // Flatten into the pre-allocated flat_cells buffer
    const cols = handle.render_state.cols;
    if (cols == 0) return null;

    // Ensure flat_cells buffer is large enough
    handle.ensureFlatCells(cols) catch return null;

    for (0..cols) |i| {
        const raw = raws[i];
        const style: terminal.Style = styles[i];
        const fg = flattenStyleColor(style.fg_color);
        var bg = flattenStyleColor(style.bg_color);

        // Handle bg_color_palette and bg_color_rgb content tags
        // where the background comes from the cell content, not style
        switch (raw.content_tag) {
            .bg_color_palette => {
                bg = .{ .color_type = 1, .r = 0, .g = 0, .b = 0, .palette = raw.content.color_palette };
            },
            .bg_color_rgb => {
                const c = raw.content.color_rgb;
                bg = .{ .color_type = 2, .r = c.r, .g = c.g, .b = c.b, .palette = 0 };
            },
            else => {},
        }
        const ul = flattenStyleColor(style.underline_color);

        handle.flat_cells[i] = .{
            .codepoint = switch (raw.content_tag) {
                .codepoint, .codepoint_grapheme => raw.content.codepoint,
                .bg_color_palette, .bg_color_rgb => 0,
            },
            .grapheme_len = if (raw.content_tag == .codepoint_grapheme)
                @intCast(graphemes[i].len)
            else
                0,
            .wide = @intFromEnum(raw.wide),

            .fg_color_type = fg.color_type,
            .fg_r = fg.r,
            .fg_g = fg.g,
            .fg_b = fg.b,
            .fg_palette = fg.palette,

            .bg_color_type = bg.color_type,
            .bg_r = bg.r,
            .bg_g = bg.g,
            .bg_b = bg.b,
            .bg_palette = bg.palette,

            .ul_color_type = ul.color_type,
            .ul_r = ul.r,
            .ul_g = ul.g,
            .ul_b = ul.b,
            .ul_palette = ul.palette,

            // Populate style_flags by bitcasting Ghostty's packed flags.
            // Fragility: this assumes Ghostty keeps the same bit layout/order
            // for Style.flags; if that changes in a Ghostty update, this
            // export must be updated (or switched to explicit bit packing).
            .style_flags = @bitCast(style.flags),
            ._padding = .{ 0, 0 },
        };
    }

    if (out_len) |len| len.* = cols;
    return handle.flat_cells.ptr;
}

/// Get grapheme codepoints for a cell. Returns pointer to grapheme_len u32 values.
/// Only valid for cells where grapheme_len > 0.
export fn ghostty_vt_terminal_render_cell_grapheme(
    ptr: ?*anyopaque,
    row: u16,
    col: u16,
    out_len: ?*u8,
) callconv(.c) ?[*]const u32 {
    if (ptr == null) return null;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    if (row >= handle.render_state.rows) return null;

    const cells = handle.render_state.row_data.items(.cells)[row];
    if (col >= cells.len) return null;

    const raw = cells.items(.raw)[col];
    if (raw.content_tag != .codepoint_grapheme) return null;

    const grapheme = cells.items(.grapheme)[col];
    if (out_len) |len| len.* = @intCast(grapheme.len);
    // u21 and u32 have different sizes, so we need a cast.
    // The grapheme data lives in the row's arena, valid until next update.
    // We can't directly cast []u21 to [*]u32 — need the grapheme_buf.
    handle.copyGraphemeToBuf(grapheme) catch return null;
    return handle.grapheme_buf.ptr;
}
```

### Step 4: Add flat_cells and grapheme_buf to TerminalHandle

```zig
// Add to TerminalHandle struct:
flat_cells: []FlatCell = &.{},
grapheme_buf: []u32 = &.{},

// Add methods to TerminalHandle:
fn ensureFlatCells(self: *TerminalHandle, cols: u16) !void {
    if (self.flat_cells.len >= cols) return;
    if (self.flat_cells.len > 0) self.alloc.free(self.flat_cells);
    self.flat_cells = try self.alloc.alloc(FlatCell, cols);
}

fn copyGraphemeToBuf(self: *TerminalHandle, grapheme: []const u21) !void {
    if (self.grapheme_buf.len < grapheme.len) {
        if (self.grapheme_buf.len > 0) self.alloc.free(self.grapheme_buf);
        self.grapheme_buf = try self.alloc.alloc(u32, grapheme.len);
    }
    for (grapheme, 0..) |cp, i| {
        self.grapheme_buf[i] = cp;
    }
}

// Update deinit to free these:
fn deinit(self: *TerminalHandle) void {
    const alloc = self.alloc;
    if (self.flat_cells.len > 0) alloc.free(self.flat_cells);
    if (self.grapheme_buf.len > 0) alloc.free(self.grapheme_buf);
    self.render_state.deinit(alloc);
    self.stream.deinit();
    self.terminal_inst.deinit(alloc);
    self.* = undefined;
    alloc.destroy(self);
}
```

### Step 5: Add row selection export

```zig
/// Get selection range for a row. Returns 1 if row has a selection, 0 otherwise.
/// When returning 1, start_x and end_x are set to the selection column range.
export fn ghostty_vt_terminal_render_row_selection(
    ptr: ?*anyopaque,
    row: u16,
    start_x: ?*u16,
    end_x: ?*u16,
) callconv(.c) u8 {
    if (ptr == null) return 0;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    if (row >= handle.render_state.rows) return 0;

    const sel = handle.render_state.row_data.items(.selection)[row];
    if (sel) |range| {
        if (start_x) |sx| sx.* = range[0];
        if (end_x) |ex| ex.* = range[1];
        return 1;
    }
    return 0;
}
```

### Step 6: Add Rust types and extern declarations

```rust
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FlatCell {
    pub codepoint: u32,
    pub grapheme_len: u8,
    pub wide: u8,
    pub fg_color_type: u8,
    pub fg_r: u8,
    pub fg_g: u8,
    pub fg_b: u8,
    pub fg_palette: u8,
    pub bg_color_type: u8,
    pub bg_r: u8,
    pub bg_g: u8,
    pub bg_b: u8,
    pub bg_palette: u8,
    pub ul_color_type: u8,
    pub ul_r: u8,
    pub ul_g: u8,
    pub ul_b: u8,
    pub ul_palette: u8,
    /// Style flags bitfield (matches Ghostty Style.Flags packed u16):
    /// bit 0: bold, 1: italic, 2: faint, 3: blink, 4: inverse,
    /// 5: invisible, 6: strikethrough, 7: overline
    /// bits 8-10: underline (0=none,1=single,2=double,3=curly,4=dotted,5=dashed)
    pub style_flags: u16,
    pub _padding: [u8; 2],
}

// Verify ABI matches Zig side
const _: () = assert!(std::mem::size_of::<FlatCell>() == 28);
const _: () = assert!(std::mem::align_of::<FlatCell>() == 4);

// In the extern block:
pub fn ghostty_vt_terminal_render_row_cells(
    terminal: *mut c_void,
    row: u16,
    out_len: *mut u16,
) -> *const FlatCell;
pub fn ghostty_vt_terminal_render_cell_grapheme(
    terminal: *mut c_void,
    row: u16,
    col: u16,
    out_len: *mut u8,
) -> *const u32;
pub fn ghostty_vt_terminal_render_row_selection(
    terminal: *mut c_void,
    row: u16,
    start_x: *mut u16,
    end_x: *mut u16,
) -> u8;
```

### Step 7: Add Rust tests

```rust
#[test]
fn test_render_row_cells() {
    let ptr = unsafe { ghostty_vt_terminal_new(80, 24) };
    let text = b"ABC";
    unsafe { ghostty_vt_terminal_feed(ptr, text.as_ptr(), text.len()) };
    unsafe { ghostty_vt_terminal_render_update(ptr) };

    let mut len: u16 = 0;
    let cells = unsafe { ghostty_vt_terminal_render_row_cells(ptr, 0, &mut len) };
    assert!(!cells.is_null());
    assert_eq!(len, 80);

    let cells_slice = unsafe { std::slice::from_raw_parts(cells, len as usize) };
    assert_eq!(cells_slice[0].codepoint, b'A' as u32);
    assert_eq!(cells_slice[1].codepoint, b'B' as u32);
    assert_eq!(cells_slice[2].codepoint, b'C' as u32);
    assert_eq!(cells_slice[0].wide, 0); // narrow
    assert_eq!(cells_slice[0].grapheme_len, 0);
    unsafe { ghostty_vt_terminal_free(ptr) };
}

#[test]
fn test_render_styled_cell() {
    let ptr = unsafe { ghostty_vt_terminal_new(80, 24) };
    // SGR 1 (bold) + SGR 31 (red fg) + "X"
    let seq = b"\x1b[1;31mX";
    unsafe { ghostty_vt_terminal_feed(ptr, seq.as_ptr(), seq.len()) };
    unsafe { ghostty_vt_terminal_render_update(ptr) };

    let mut len: u16 = 0;
    let cells = unsafe { ghostty_vt_terminal_render_row_cells(ptr, 0, &mut len) };
    let cells_slice = unsafe { std::slice::from_raw_parts(cells, len as usize) };
    assert_eq!(cells_slice[0].codepoint, b'X' as u32);
    // Bold flag should be set (bit 0)
    assert!(cells_slice[0].style_flags & 1 != 0);
    // Foreground should be palette color (red = palette index 1)
    assert_eq!(cells_slice[0].fg_color_type, 1); // palette
    assert_eq!(cells_slice[0].fg_palette, 1); // red
    unsafe { ghostty_vt_terminal_free(ptr) };
}

#[test]
fn test_render_row_selection_none() {
    let ptr = unsafe { ghostty_vt_terminal_new(80, 24) };
    unsafe { ghostty_vt_terminal_render_update(ptr) };
    let mut sx: u16 = 0;
    let mut ex: u16 = 0;
    let has = unsafe { ghostty_vt_terminal_render_row_selection(ptr, 0, &mut sx, &mut ex) };
    assert_eq!(has, 0); // no selection
    unsafe { ghostty_vt_terminal_free(ptr) };
}
```

### Step 8: Build and test

```bash
cargo test -p ghostty_vt
```

**Note:** The exact palette index for SGR 31 (red) depends on Ghostty's
style representation. If Ghostty resolves named colors to palette indices
at parse time, `fg_palette` will be 1. Verify against zigdoc or a quick
test and adjust the assertion if needed.

---

## Part 6: Selection

**Goal:** Export selection set/clear/get_text. Selection is set via viewport coordinates, stored on the active screen, and picked up by RenderState on the next update.

**Files:**

- Modify: `crates/ghostty-vt/zig/lib.zig`
- Modify: `crates/ghostty-vt/src/lib.rs`

**Depends on:** Part 3

### Step 1: Add Zig exports

```zig
/// Set a selection on the terminal. Coordinates are in viewport space (0-indexed).
/// rectangular: 1 for block/rectangle selection, 0 for normal.
/// Returns 0 on success, 1 if null, 2 if coordinates can't be pinned.
export fn ghostty_vt_terminal_set_selection(
    ptr: ?*anyopaque,
    start_x: u16,
    start_y: u32,
    end_x: u16,
    end_y: u32,
    rectangular: u8,
) callconv(.c) c_int {
    if (ptr == null) return 1;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    const screen = handle.terminal_inst.screens.active;

    const start_pin = screen.pages.pin(.{ .viewport = .{
        .x = start_x,
        .y = start_y,
    } }) orelse return 2;

    const end_pin = screen.pages.pin(.{ .viewport = .{
        .x = end_x,
        .y = end_y,
    } }) orelse return 2;

    const sel = terminal.Selection.init(start_pin, end_pin, rectangular != 0);
    screen.select(sel) catch return 2;
    return 0;
}

/// Clear any active selection.
export fn ghostty_vt_terminal_clear_selection(ptr: ?*anyopaque) callconv(.c) void {
    if (ptr == null) return;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    handle.terminal_inst.screens.active.clearSelection();
}

/// Get the selected text as a UTF-8 null-terminated string.
/// Returns a pointer to the string, or null if no selection or error.
/// The caller must free the returned pointer with ghostty_vt_bytes_free().
export fn ghostty_vt_terminal_get_selection_text(
    ptr: ?*anyopaque,
    out_len: ?*usize,
) callconv(.c) ?[*]const u8 {
    if (ptr == null) return null;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    const screen = handle.terminal_inst.screens.active;

    const sel = screen.selection orelse return null;

    const text = screen.selectionString(handle.alloc, .{ .sel = sel }) catch return null;
    if (out_len) |len| len.* = text.len;
    return text.ptr;
}

/// Free a byte buffer returned by get_selection_text.
export fn ghostty_vt_bytes_free(bytes: ?[*]const u8, len: usize) callconv(.c) void {
    if (bytes == null) return;
    const alloc = std.heap.smp_allocator;
    // selectionString returns a [:0]const u8, so actual allocation is len+1
    const slice = @as([*]u8, @constCast(bytes.?))[0 .. len + 1];
    alloc.free(slice);
}
```

### Step 2: Add Rust extern declarations

```rust
// In the extern block:
pub fn ghostty_vt_terminal_set_selection(
    terminal: *mut c_void,
    start_x: u16,
    start_y: u32,
    end_x: u16,
    end_y: u32,
    rectangular: u8,
) -> c_int;
pub fn ghostty_vt_terminal_clear_selection(terminal: *mut c_void);
pub fn ghostty_vt_terminal_get_selection_text(
    terminal: *mut c_void,
    out_len: *mut usize,
) -> *const u8;
pub fn ghostty_vt_bytes_free(bytes: *const u8, len: usize);
```

### Step 3: Add Rust tests

```rust
#[test]
fn test_selection_set_clear() {
    let ptr = unsafe { ghostty_vt_terminal_new(80, 24) };
    let text = b"Hello, World!";
    unsafe { ghostty_vt_terminal_feed(ptr, text.as_ptr(), text.len()) };

    // Set selection covering "Hello"
    let rc = unsafe { ghostty_vt_terminal_set_selection(ptr, 0, 0, 4, 0, 0) };
    assert_eq!(rc, 0);

    // Update render state to pick up selection
    unsafe { ghostty_vt_terminal_render_update(ptr) };

    // Row 0 should have a selection
    let mut sx: u16 = 0;
    let mut ex: u16 = 0;
    let has = unsafe { ghostty_vt_terminal_render_row_selection(ptr, 0, &mut sx, &mut ex) };
    assert_eq!(has, 1);
    assert_eq!(sx, 0);
    assert_eq!(ex, 4);

    // Get selection text
    let mut len: usize = 0;
    let text_ptr = unsafe { ghostty_vt_terminal_get_selection_text(ptr, &mut len) };
    assert!(!text_ptr.is_null());
    let selected = unsafe { std::str::from_utf8(std::slice::from_raw_parts(text_ptr, len)).unwrap() };
    assert_eq!(selected, "Hello");
    unsafe { ghostty_vt_bytes_free(text_ptr, len) };

    // Clear selection
    unsafe { ghostty_vt_terminal_clear_selection(ptr) };
    unsafe { ghostty_vt_terminal_render_update(ptr) };
    let has = unsafe { ghostty_vt_terminal_render_row_selection(ptr, 0, &mut sx, &mut ex) };
    assert_eq!(has, 0);

    unsafe { ghostty_vt_terminal_free(ptr) };
}
```

### Step 4: Build and test

```bash
cargo test -p ghostty_vt
```

**Note:** The exact selected text may include or exclude the end column
depending on Ghostty's selection semantics (inclusive vs exclusive end).
Verify with a test run and adjust the end coordinate if needed.

---

## Part 7: Key Encoding

**Goal:** Export key encoding that uses `Options.fromTerminal()` to automatically handle all protocol selection (legacy, kitty, xterm modifyOtherKeys).

**Files:**

- Modify: `crates/ghostty-vt/zig/lib.zig`
- Modify: `crates/ghostty-vt/src/lib.rs`

**Depends on:** Part 1

### Step 1: Add Zig export

The key encoder writes to an `std.Io.Writer`. We provide a stack buffer
and write into it, returning the number of bytes written.

```zig
const key_encode = @import("ghostty/src/input/key_encode.zig");
const input_key = @import("ghostty/src/input/key.zig");

/// Encode a key event using the terminal's current mode state.
/// Returns the number of bytes written to buf (0 = no output for this event).
/// Returns 0 if handle is null or buf is null.
export fn ghostty_vt_terminal_encode_key(
    ptr: ?*anyopaque,
    /// Ghostty Key enum value (c_int)
    key_val: c_int,
    /// Modifier bitfield (Mods packed u16)
    mods: u16,
    /// Action: 0=release, 1=press, 2=repeat
    action: u8,
    /// UTF-8 text generated by the key event (for kitty protocol)
    text_ptr: ?[*]const u8,
    text_len: usize,
    /// Output buffer
    buf: ?[*]u8,
    buf_len: usize,
) callconv(.c) usize {
    if (ptr == null or buf == null) return 0;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));

    const opts = key_encode.Options.fromTerminal(&handle.terminal_inst);

    const event: input_key.KeyEvent = .{
        .key = @enumFromInt(key_val),
        .mods = @bitCast(mods),
        .action = @enumFromInt(action),
        .utf8 = if (text_ptr) |p| p[0..text_len] else "",
    };

    // Write into a fixed buffer writer backed by the caller's buffer
    var fbs = std.io.fixedBufferStream(buf.?[0..buf_len]);
    var writer = fbs.writer().any();
    key_encode.encode(&writer, event, opts) catch return 0;
    return fbs.pos;
}
```

### Step 2: Add Rust extern declaration

```rust
pub fn ghostty_vt_terminal_encode_key(
    terminal: *mut c_void,
    key: c_int,
    mods: u16,
    action: u8,
    text_ptr: *const u8,
    text_len: usize,
    buf: *mut u8,
    buf_len: usize,
) -> usize;
```

### Step 3: Add Rust test

```rust
#[test]
fn test_encode_key_basic() {
    let ptr = unsafe { ghostty_vt_terminal_new(80, 24) };
    let mut buf = [0u8; 128];
    // Encode 'a' press (Key::a = 0x61 in Ghostty's Key enum... needs verification)
    // For now test with a simple Enter key which should produce \r
    // Key::enter value needs to be looked up from the Key enum
    let n = unsafe {
        ghostty_vt_terminal_encode_key(
            ptr,
            0x28, // Key.enter — verify via zigdoc
            0,    // no mods
            1,    // press
            std::ptr::null(),
            0,
            buf.as_mut_ptr(),
            buf.len(),
        )
    };
    // Enter should produce \r (0x0D) in legacy mode
    assert!(n > 0);
    unsafe { ghostty_vt_terminal_free(ptr) };
}
```

### Step 4: Build and test

```bash
cargo test -p ghostty_vt
```

**Note:** The Key enum integer values must be looked up from
`ghostty/src/input/key.zig`. Use `zigdoc` to check. The Rust side
will define matching constants in `ffi-001`.

---

## Part 8: Mouse Encoding

**Goal:** Export mouse event encoding. Re-implements the protocol logic from `Surface.zig:mouseReport()` as a pure function that writes VT bytes to a caller-provided buffer.

**Files:**

- Modify: `crates/ghostty-vt/zig/lib.zig`
- Modify: `crates/ghostty-vt/src/lib.rs`

**Depends on:** Part 1

### Step 1: Add Zig mouse encoding export

The logic is extracted from `Surface.zig:3591-3815`. We only need the
byte generation, not the event filtering (that's the Rust terminal crate's
job — it decides _whether_ to report, the shim decides _how_).

```zig
/// Encode a mouse event using the terminal's current mouse format.
/// button: 0=left, 1=middle, 2=right, 3=release/none, 64=scroll_up, 65=scroll_down,
///         66-67=scroll_left/right, 128-129=back/forward
/// action: 0=press, 1=release, 2=motion
/// mods: shift=bit0, alt=bit1, ctrl=bit2 (simplified, not full Mods)
/// x, y: 0-indexed viewport cell coordinates
/// Returns bytes written to buf (0 = nothing to encode or error).
export fn ghostty_vt_terminal_encode_mouse(
    ptr: ?*anyopaque,
    button: u8,
    action: u8,
    mods: u8,
    x: u16,
    y: u16,
    buf: ?[*]u8,
    buf_len: usize,
) callconv(.c) usize {
    if (ptr == null or buf == null) return 0;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));

    const mouse_format = handle.terminal_inst.flags.mouse_format;
    const mouse_event = handle.terminal_inst.flags.mouse_event;

    // Build the button code
    var button_code: u8 = button;
    if (action == 1 and mouse_format != .sgr and mouse_format != .sgr_pixels) {
        // Release is always 3 in non-SGR formats
        button_code = 3;
    }

    // Add modifier bits (only if not x10 mode)
    if (mouse_event != .x10) {
        if (mods & 1 != 0) button_code += 4; // shift
        if (mods & 2 != 0) button_code += 8; // alt
        if (mods & 4 != 0) button_code += 16; // ctrl
    }

    // Motion flag
    if (action == 2) button_code += 32;

    var fbs = std.io.fixedBufferStream(buf.?[0..buf_len]);
    const writer = fbs.writer();

    switch (mouse_format) {
        .x10 => {
            if (x > 222 or y > 222) return 0;
            writer.writeAll("\x1b[M") catch return 0;
            writer.writeByte(32 + button_code) catch return 0;
            writer.writeByte(32 + @as(u8, @intCast(x)) + 1) catch return 0;
            writer.writeByte(32 + @as(u8, @intCast(y)) + 1) catch return 0;
        },
        .utf8 => {
            writer.writeAll("\x1b[M") catch return 0;
            writer.writeByte(32 + button_code) catch return 0;
            var tmp: [4]u8 = undefined;
            var n = std.unicode.utf8Encode(@intCast(32 + x + 1), &tmp) catch return 0;
            writer.writeAll(tmp[0..n]) catch return 0;
            n = std.unicode.utf8Encode(@intCast(32 + y + 1), &tmp) catch return 0;
            writer.writeAll(tmp[0..n]) catch return 0;
        },
        .sgr => {
            const final: u8 = if (action == 1) 'm' else 'M';
            std.fmt.format(writer.any(), "\x1b[<{d};{d};{d}{c}", .{
                button_code,
                @as(u32, x) + 1,
                @as(u32, y) + 1,
                final,
            }) catch return 0;
        },
        .urxvt => {
            std.fmt.format(writer.any(), "\x1b[{d};{d};{d}M", .{
                @as(u16, 32) + button_code,
                @as(u32, x) + 1,
                @as(u32, y) + 1,
            }) catch return 0;
        },
        .sgr_pixels => {
            // For pixel mode, x/y are passed as cell coords.
            // The caller (Rust terminal crate) should convert to pixels
            // before calling if pixel mode is active.
            const final: u8 = if (action == 1) 'm' else 'M';
            std.fmt.format(writer.any(), "\x1b[<{d};{d};{d}{c}", .{
                button_code,
                @as(u32, x) + 1,
                @as(u32, y) + 1,
                final,
            }) catch return 0;
        },
    }

    return fbs.pos;
}
```

### Step 2: Add Rust extern declaration

```rust
pub fn ghostty_vt_terminal_encode_mouse(
    terminal: *mut c_void,
    button: u8,
    action: u8,
    mods: u8,
    x: u16,
    y: u16,
    buf: *mut u8,
    buf_len: usize,
) -> usize;
```

### Step 3: Add Rust tests

```rust
#[test]
fn test_encode_mouse_sgr() {
    let ptr = unsafe { ghostty_vt_terminal_new(80, 24) };
    // Enable SGR mouse: CSI ?1003h (any-event) + CSI ?1006h (SGR format)
    let seq = b"\x1b[?1003h\x1b[?1006h";
    unsafe { ghostty_vt_terminal_feed(ptr, seq.as_ptr(), seq.len()) };

    let mut buf = [0u8; 64];
    // Left button press at (5, 10)
    let n = unsafe {
        ghostty_vt_terminal_encode_mouse(ptr, 0, 0, 0, 5, 10, buf.as_mut_ptr(), buf.len())
    };
    assert!(n > 0);
    let output = std::str::from_utf8(&buf[..n]).unwrap();
    // SGR format: \x1b[<0;6;11M (button 0, 1-indexed coords)
    assert_eq!(output, "\x1b[<0;6;11M");

    // Left button release at (5, 10) — SGR uses 'm' for release
    let n = unsafe {
        ghostty_vt_terminal_encode_mouse(ptr, 0, 1, 0, 5, 10, buf.as_mut_ptr(), buf.len())
    };
    let output = std::str::from_utf8(&buf[..n]).unwrap();
    assert_eq!(output, "\x1b[<0;6;11m");

    unsafe { ghostty_vt_terminal_free(ptr) };
}

#[test]
fn test_encode_mouse_x10() {
    let ptr = unsafe { ghostty_vt_terminal_new(80, 24) };
    // Enable X10 mouse: CSI ?9h
    let seq = b"\x1b[?9h";
    unsafe { ghostty_vt_terminal_feed(ptr, seq.as_ptr(), seq.len()) };

    let mut buf = [0u8; 64];
    // Left button press at (0, 0)
    let n = unsafe {
        ghostty_vt_terminal_encode_mouse(ptr, 0, 0, 0, 0, 0, buf.as_mut_ptr(), buf.len())
    };
    assert!(n > 0);
    assert_eq!(n, 6); // \x1b[M + button + x + y
    assert_eq!(buf[0], 0x1b);
    assert_eq!(buf[1], b'[');
    assert_eq!(buf[2], b'M');
    assert_eq!(buf[3], 32); // button 0 + 32
    assert_eq!(buf[4], 33); // x=0 + 32 + 1
    assert_eq!(buf[5], 33); // y=0 + 32 + 1

    unsafe { ghostty_vt_terminal_free(ptr) };
}
```

### Step 4: Build and test

```bash
cargo test -p ghostty_vt
```

---

## Part 9: Final Lint + Verification

**Goal:** Run lints, verify all tests pass, close the issue.

### Step 1: Lint Zig code

```bash
ziglint crates/ghostty-vt/zig/
zig fmt crates/ghostty-vt/zig/lib.zig
```

### Step 2: Run full test suite

```bash
cargo test -p ghostty_vt
```

### Step 3: Close the issue

```bash
dot close shim-002
```

---

## Feasible Optimization Follow-up

Ghostty's `RenderState.update()` includes a noted optimization opportunity in
selection handling (`containedRowCached` still recomputes per-row state).
This is feasible now, but out of scope for shim-002 because the shim only
exports render data and does not alter Ghostty's internal selection model.

Add follow-up issue/task:

- Cache selection row bounds and derived point metadata once per update pass,
  then reuse in row iteration to avoid repeated recomputation on partial redraws.
- Add a focused perf benchmark over large rectangular + multi-line selections
  to verify reduced render_update CPU time.

---

## Unresolved Questions

1. **`std.Io.Writer` vs `std.io` in Zig 0.15.2:** The key encoder uses
   `std.Io.Writer` (capital I). Need to verify if `fixedBufferStream`
   returns a compatible writer type. Use `zigdoc std.io.fixedBufferStream`
   to confirm the API.

2. **`selectionString` allocator:** `get_selection_text` uses `handle.alloc`
   (smp_allocator) but `ghostty_vt_bytes_free` also uses smp_allocator.
   These must match — verify that smp_allocator is stateless (it is; it's
   a global).

3. **Selection end coordinate semantics:** Ghostty selections may be
   start-inclusive, end-inclusive or end-exclusive. The test in Part 6
   assumes `(0,0)-(4,0)` selects "Hello" (5 chars). If Ghostty uses
   exclusive end, adjust to `(0,0)-(5,0)`.

4. **`page.Cell.content` access:** The `content` field is a packed union.
   Accessing `.codepoint` when `content_tag` is `.bg_color_palette` is
   technically accessing inactive union fields. Verify Zig 0.15.2 allows
   this or use the switch as shown.

5. **`style_flags` bitcast stability:** We `@bitCast` Ghostty's packed
   style flags directly. This is intentionally fragile but fast/simple;
   if Ghostty changes the flag layout/order, we must update FlatCell
   and the Rust-side bit documentation (or switch to explicit bit packing).

6. **Key enum values:** The Rust tests use placeholder integer values for
   Key enum entries. These must be looked up from `ghostty/src/input/key.zig`
   using `zigdoc`. The `ffi-001` task will define Rust-side constants.

7. **`fbs.pos` field name:** Verify the field name for current position on
   `fixedBufferStream` in Zig 0.15.2 — might be `.pos` or
   `.getPos()`/`.bytes_written`. Check with `zigdoc`.
