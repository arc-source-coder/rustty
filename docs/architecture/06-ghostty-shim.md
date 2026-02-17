# Ghostty Zig Shim Design

## Purpose

Document the exact Ghostty 1.3.x internal APIs the Zig shim consumes, the
C ABI it exports, and how it differs from the gpui-ghostty reference
(which targets Ghostty 1.2.x).

## Ghostty API Landscape

Ghostty exposes three integration surfaces. Only one is suitable for us.

### `vt.h` (libghostty-vt)

Location: `vendor/ghostty/include/ghostty/vt.h`

Provides parsers only:

- SGR parser (style attribute parsing)
- OSC parser (operating system commands)
- Key encoder (kitty keyboard protocol encoding)
- Paste safety checker

Does **not** provide: `Terminal`, `Screen`, grid, scrollback, cursor,
dirty tracking, or any terminal state machine. Cannot be used as the sole
integration point.

We **do** use the key encoding from this API — it is stable and well-tested.

### `ghostty.h` (Embedding API)

Location: `vendor/ghostty/include/ghostty.h`

Full app runtime designed for macOS embedding:

- Owns PTY lifecycle internally
- Owns renderer, surfaces, config system
- Platform enum only has macOS/iOS entries
- Heavy callback setup (`ghostty_runtime_config_s`)
- Creates `ghostty_app_t` / `ghostty_surface_t` opaque handles

Rejected: designed for platforms that want Ghostty-as-a-complete-component
including its renderer. Fighting its assumptions would be more work than
using internals directly.

### Internal Zig Modules (What We Use)

Location: `vendor/ghostty/src/terminal/`

Provides the full terminal emulator as composable Zig modules:

- `terminal.Terminal` — full state machine (grid, scrollback, cursor, modes,
  colors, charsets, kitty keyboard state, mouse mode flags)
- `terminal.Stream(Handler)` — VT parser that dispatches actions to a Handler
- `terminal.RenderState` — incremental render state extraction (new in 1.3.x)
- `terminal.Screen` / `terminal.ScreenSet` — primary and alternate screen
- `terminal.Selection` — selection model
- `terminal.PageList` / `terminal.Page` — memory-efficient page storage

This gives us the full emulator without Ghostty's renderer, font system,
or app runtime. The tradeoff is coupling to internal APIs — the shim must
be updated on Ghostty version bumps, but the `ghostty_vt` crate boundary
isolates this from the rest of the codebase.

## RenderState (New in Ghostty 1.3.x)

Location: `vendor/ghostty/src/terminal/render.zig`

This is the single biggest advantage over the gpui-ghostty reference.
`RenderState` was added specifically for extracting renderable data from
terminal state without cloning the entire screen.

### What It Provides

```zig
pub const RenderState = struct {
    rows: CellCountInt,
    cols: CellCountInt,
    colors: Colors,          // bg/fg/cursor/palette, reverse-color aware
    cursor: Cursor,          // position, style, blink, visibility, password_input
    row_data: MultiArrayList(Row),  // per-row: cells, selection, highlights, dirty
    dirty: Dirty,            // .false | .partial | .full
    screen: ScreenSet.Key,   // primary vs alternate
};
```

Per-row data:

- `cells: MultiArrayList(Cell)` — raw cell + resolved grapheme + resolved style
- `selection: ?[2]CellCountInt` — x-range of selection within the row
- `highlights: ArrayList(Highlight)` — tagged highlight ranges (search, etc.)
- `dirty: bool` — whether the row changed since last read
- `pin: PageList.Pin` — page location (not safe to dereference after update)
- `raw: page.Row` — raw row metadata

### Lifecycle

Ghostty's renderer (`renderer/generic.zig:217`) holds a **persistent**
`RenderState` as a struct field, initialized to `.empty`. Each frame:

1. Lock terminal mutex
2. Call `self.terminal_state.update(self.alloc, state.terminal)`
3. Check `self.terminal_state.dirty` to decide what to repaint
4. Unlock mutex
5. Use row data for rendering
6. Set `self.terminal_state.dirty = .false` when done

Periodic GC: every ~100,000 frames (~12 min at 120Hz), the renderer
`deinit`s and resets to `.empty` to reclaim memory from historically-large
frames.

### Dirty Tracking

`RenderState.update()` determines dirty state automatically:

- **Full redraw** triggers: screen switch, terminal dirty flags, screen dirty
  flags, dimension change, viewport pin change
- **Partial dirty**: individual rows marked dirty by page/row dirty bits
- **Not dirty**: no changes since last update

After update, `RenderState.dirty` is one of:

- `.false` — skip rendering entirely
- `.partial` — only repaint rows where `row.dirty == true`
- `.full` — repaint everything (colors/dimensions/screen changed)

The update clears terminal and screen dirty flags automatically.

### Viewport Source of Truth

Ghostty stores viewport state in terminal core (`PageList`), not in the
renderer:

- `viewport` mode (`active`/`top`/`pin`)
- `viewport_pin`
- `viewport_pin_row_offset` cache

`RenderState.update()` reads rows relative to `getTopLeft(.viewport)`.
When viewport pin changes, dirty becomes `.full`.

### Selection Integration

`RenderState.update()` reads selection from `Screen.selection` (line 566
of render.zig). Selection is **not set through RenderState** — it is set
directly on the terminal's active screen:

```zig
handle.terminal.screens.active.selection = Selection.init(start, end, rectangular);
```

On the next `update()`, RenderState iterates viewport rows and populates
`row.selection` with the x-range `[start_x, end_x]` for each row that
intersects the selection. It caches the selection pins to avoid expensive
recalculation when only some rows are dirty.

Shim exports for selection:

```
ghostty_vt_terminal_set_selection(handle, start_x, start_y_u32, end_x, end_y_u32, rectangular)
ghostty_vt_terminal_clear_selection(handle)
```

These write to `handle.terminal.screens.active.selection`. The renderer
then sees selection ranges in the row data after the next `update()`.

## Key Encoding and Kitty Keyboard Protocol

Location: `vendor/ghostty/src/input/key_encode.zig`

### The Problem

Key encoding depends on terminal state: the running program negotiates
modes (DEC cursor keys, keypad application mode, kitty keyboard protocol)
that change how key events are serialized to VT bytes.

The gpui-ghostty reference only encodes named keys with hard-coded
`alt_esc_prefix = true` and ignores kitty keyboard flags entirely. This
is incomplete.

### The Solution: `Options.fromTerminal()`

The key encoder takes an `Options` struct:

```zig
pub const Options = struct {
    cursor_key_application: bool,    // DEC mode 1
    keypad_key_application: bool,    // DEC mode 66
    ignore_keypad_with_numlock: bool,// DEC mode 1035
    alt_esc_prefix: bool,            // DEC mode 1036
    modify_other_keys_state_2: bool, // xterm modifyOtherKeys mode 2
    kitty_flags: KittyFlags,         // kitty keyboard protocol flags
    macos_option_as_alt: OptionAsAlt,// macOS-specific, always .false for us
};
```

And provides a constructor that reads all of these from terminal state:

```zig
pub fn fromTerminal(t: *const Terminal) Options {
    return .{
        .alt_esc_prefix = t.modes.get(.alt_esc_prefix),
        .cursor_key_application = t.modes.get(.cursor_keys),
        .keypad_key_application = t.modes.get(.keypad_keys),
        .ignore_keypad_with_numlock = t.modes.get(.ignore_keypad_with_numlock),
        .modify_other_keys_state_2 = t.flags.modify_other_keys_2,
        .kitty_flags = t.screens.active.kitty_keyboard.current(),
        .macos_option_as_alt = .false,
    };
}
```

The encoder then dispatches to either kitty protocol or legacy encoding
based on `kitty_flags`.

### Shim Export

A single function that encodes using the terminal's current state:

```
ghostty_vt_terminal_encode_key(
    handle,
    key: u32,           // ghostty key enum value
    mods: u16,          // modifier bitfield
    action: u8,         // press/release/repeat
    text_ptr, text_len, // composing text (for kitty protocol)
    buf, buf_len,       // output buffer
) -> usize             // bytes written (0 = no output)
```

By calling `Options.fromTerminal()` internally, the shim handles all
protocol selection. The Rust `terminal` crate does not need to separately
query kitty flags or mode state — it calls `encode_key()` and gets the
correct encoding for whatever the running program has negotiated.

## Terminal Mode Flags — Direct Access

The gpui-ghostty reference scans raw PTY output bytes for CSI mode
sequences to track bracketed paste, mouse reporting, etc. This is fragile.

With internal access, we read mode flags directly from the terminal:

| Flag                   | Access                                                    |
| ---------------------- | --------------------------------------------------------- |
| Bracketed paste        | `terminal.modes.get(.bracketed_paste)`                    |
| Mouse event mode       | `terminal.flags.mouse_event` (none/x10/normal/button/any) |
| Mouse format           | `terminal.flags.mouse_format` (x10/utf8/sgr/urxvt/pixels) |
| Mouse shift capture    | `terminal.flags.mouse_shift_capture`                      |
| Kitty keyboard flags   | `terminal.screens.active.kitty_keyboard.current()`        |
| modifyOtherKeys mode 2 | `terminal.flags.modify_other_keys_2`                      |
| Cursor visible         | `terminal.modes.get(.cursor_visible)`                     |
| Cursor blinking        | `terminal.modes.get(.cursor_blinking)`                    |
| Reverse colors         | `terminal.modes.get(.reverse_colors)`                     |
| Alt screen active      | `terminal.screens.active_key`                             |
| Synchronized output    | `terminal.modes.get(.synchronized_output)`                |

Shim exports for commonly needed flags:

```
ghostty_vt_terminal_get_mouse_mode(handle) -> u8
ghostty_vt_terminal_get_mouse_format(handle) -> u8
ghostty_vt_terminal_is_bracketed_paste(handle) -> bool
ghostty_vt_terminal_get_kitty_keyboard_flags(handle) -> u32
```

These replace byte scanning entirely.

## Coordinate and Width Model

Ghostty terminal internals use asymmetric coordinate widths:

- Grid dimensions and x coordinates use `CellCountInt` (`u16`)
- Absolute y coordinates use `u32`
- Some internal totals/offsets use `usize`

Shim boundary rules:

- FFI APIs use fixed-width integers only (`u16` / `u32` / `u64`)
- Do not expose `usize` in C ABI structs
- Selection/search-related y coordinates are `u32`

## Shim C ABI Surface

### Lifecycle

```
ghostty_vt_terminal_new(cols, rows) -> *handle | null
ghostty_vt_terminal_free(handle)
ghostty_vt_terminal_set_callbacks(handle, callbacks_struct)
```

### Feed and Resize

```
ghostty_vt_terminal_feed(handle, bytes, len) -> i32
ghostty_vt_terminal_resize(handle, cols, rows) -> i32
```

### Render State

```
ghostty_vt_terminal_render_update(handle) -> i32
ghostty_vt_terminal_render_dirty(handle) -> u8          // 0=false, 1=partial, 2=full
ghostty_vt_terminal_render_rows(handle) -> u16
ghostty_vt_terminal_render_cols(handle) -> u16
ghostty_vt_terminal_render_cursor(handle, out_cursor) -> bool
ghostty_vt_terminal_render_colors(handle, out_colors)
ghostty_vt_terminal_render_row_dirty(handle, y) -> bool
ghostty_vt_terminal_render_row_cells(handle, y, out_cells, out_len) -> i32
ghostty_vt_terminal_render_row_selection(handle, y, out_start, out_end) -> bool
ghostty_vt_terminal_render_clear_dirty(handle)
```

### Selection

```
ghostty_vt_terminal_set_selection(handle, start_x, start_y_u32, end_x, end_y_u32, rectangular)
ghostty_vt_terminal_clear_selection(handle)
```

### Highlights (Search / Decorations)

Export these stubs from v0. The Rust side calls them when search is wired;
the shim writes to `RenderState` highlight lists and the renderer sees tagged
ranges in `row.highlights` after the next `render_update()`.

```
// tag: 0 = search_match, 1 = search_match_selected (matches RenderState.Highlight tags)
// ranges: pointer to array of { start_x: u16, start_y: u32, end_x: u16, end_y: u32 }
// count: number of ranges
ghostty_vt_terminal_set_highlights(handle, tag: u8, ranges, count: usize) -> i32
ghostty_vt_terminal_clear_highlights(handle, tag: u8) -> i32
```

All coordinates are absolute terminal coordinates (`GridCoord` space), not
viewport-relative. Ghostty's `RenderState.update()` maps them to per-row
x-ranges automatically during the next render cycle.

### Scroll

```
ghostty_vt_terminal_scroll_viewport(handle, delta) -> i32
ghostty_vt_terminal_scroll_viewport_top(handle) -> i32
ghostty_vt_terminal_scroll_viewport_bottom(handle) -> i32
```

### Query

```
ghostty_vt_terminal_cursor_position(handle, out_col, out_row) -> bool
ghostty_vt_terminal_hyperlink_at(handle, col, row) -> bytes_t
```

### Mode Flags

```
ghostty_vt_terminal_get_mouse_mode(handle) -> u8
ghostty_vt_terminal_get_mouse_format(handle) -> u8
ghostty_vt_terminal_is_bracketed_paste(handle) -> bool
ghostty_vt_terminal_get_kitty_keyboard_flags(handle) -> u32
```

### Key Encoding

```
ghostty_vt_terminal_encode_key(handle, key, mods, action, text, text_len, buf, buf_len) -> usize
```

### Mouse Encoding (Recommended)

```
ghostty_vt_terminal_encode_mouse(handle, event, button, mods, x, y, pixel_x, pixel_y, buf, buf_len) -> usize
```

Rationale: mouse report encoding depends on terminal mode/format flags and
modifier handling. Keep this logic in one terminal/shim boundary instead of
duplicating protocol details in renderer/UI code.

### Colors

```
ghostty_vt_terminal_set_default_colors(handle, fg_r, fg_g, fg_b, bg_r, bg_g, bg_b)
```

### Memory

```
ghostty_vt_bytes_free(bytes)
```

## FFI Lifetime and Thread Contract

All borrowed render data follows one explicit epoch rule:

- Pointers/slices returned by render getters are valid until the next
  terminal mutation (`feed`, `resize`, viewport scroll, selection/highlight
  mutation, `render_update`, `free`).
- Terminal handle is single-thread-owned by UI thread.
- No reentrant mutation from callbacks while handling `feed()`.

Rust safe wrappers enforce this with borrow lifetimes (`RenderFrame<'_>`);
the C ABI relies on this documented contract.

## TerminalHandle Layout

```zig
const TerminalHandle = struct {
    alloc: Allocator,
    terminal: terminal.Terminal,
    stream: terminal.Stream(*ShimHandler),
    handler: ShimHandler,
    render_state: terminal.RenderState,
};
```

The `ShimHandler` owns the `ReadonlyHandler`, `Callbacks`, and
`ResponseBuffer` internally (see appendix). The `RenderState` is
persistent — allocated once at `new()`, updated each frame via
`render_state.update(alloc, &terminal)`, and freed at `free()`.

## Communication Between Crates

```
renderer → terminal:  pull API (session.render_state(), session.cursor(), etc.)
terminal → pty:       PtyCommand channel (Write/Resize/Close)
pty → terminal:       PtyEvent channel (Output/Exited/Error)
terminal → renderer:  cx.notify() triggers repaint
terminal → app:       title/bell/clipboard via GPUI events or callbacks
```

The `terminal` crate:

1. Drains `PtyEvent::Output` bytes
2. Calls `ghostty_vt.feed(bytes)` — handler mutates terminal state and
   fires callbacks for title/bell/clipboard
3. Calls `ghostty_vt.render_update()` — updates persistent RenderState
4. Calls `cx.notify()` to schedule repaint

The `renderer` crate:

1. Receives repaint via GPUI render cycle
2. Checks `render_dirty()` — skips if `.false`
3. Iterates rows, checks per-row dirty flags
4. Pulls cell data, selection ranges, cursor state
5. Calls `render_clear_dirty()` when done

## Differences from gpui-ghostty Reference

| Area           | gpui-ghostty (1.2.x)                           | Our approach (1.3.x)                                   |
| -------------- | ---------------------------------------------- | ------------------------------------------------------ |
| Render data    | Manual pin iteration + byte serialization      | `RenderState.update()` — Ghostty does the work         |
| Handler        | Custom 21-method Handler                       | Thin wrapper delegating to ReadonlyHandler (see below)  |
| Key encoding   | Named keys only, ignores terminal mode         | `Options.fromTerminal()` — full protocol-aware         |
| Mode flags     | Byte scanning PTY output                       | Direct read from `terminal.modes` / `terminal.flags`   |
| Selection      | Not handled in shim                            | Set on `Screen.selection`, RenderState picks it up     |
| Dirty tracking | Manual `isDirty()` per pin, byte serialization | `RenderState.dirty` enum + per-row dirty bools         |
| UTF-8 fallback | Scalar decoder (same)                          | Scalar decoder (same for now, ideally upgrade to SIMD) |

---

## Appendix: Stream Handler Architecture (Exploration Notes)

This section documents how Ghostty's own apps handle the Stream handler,
for future reference when deciding on our handler strategy.

### ReadonlyHandler

Location: `vendor/ghostty/src/terminal/stream_readonly.zig`

`ReadonlyHandler` is a `Stream` handler that processes **all state-mutating
actions** against a `Terminal` instance. It handles ~60+ action types:

- Print, cursor movement, erase, scroll, insert/delete
- Mode set/reset/save/restore (including alt screen switching)
- Kitty keyboard push/pop/set
- Charsets, attributes, hyperlinks
- Semantic prompts, color operations
- DECALN, full reset

It **ignores** actions that require a response or have side-effects outside
the terminal state:

- `bell` — no audible/visual bell
- `window_title` — no title update
- `clipboard_contents` — no clipboard access
- `show_desktop_notification` — no notification
- `report_pwd` — no pwd tracking
- `device_attributes` — no DA response
- `device_status` — no DSR response
- `request_mode` — no DECRPM response
- `kitty_keyboard_query` — no query response
- `xtversion` — no version response
- `size_report` — no size response

Intended consumers: replay tooling, CI log viewers, PaaS builder output.

### StreamHandler (What Ghostty's Apps Actually Use)

Location: `vendor/ghostty/src/termio/stream_handler.zig`

`StreamHandler` is the full handler used by Ghostty's macOS and GTK apps.
It handles everything `ReadonlyHandler` does, plus:

**Side-effect dispatching via mailboxes:**

- `bell` → `surfaceMessageWriter(.ring_bell)` — surface mailbox
- `window_title` → `surfaceMessageWriter(.{ .set_title = buf })` — title with
  empty-title-resets-to-pwd behavior
- `clipboard_contents` → `surfaceMessageWriter(.clipboard_read/write)` — OSC 52
- `show_desktop_notification` → surface message
- `report_pwd` → `surfaceMessageWriter(.{ .pwd_change = ... })` — directory tracking
- `mouse_shape` → `surfaceMessageWriter(.{ .set_mouse_shape = ... })` — cursor shape

**Device response writing via termio mailbox:**

- `device_attributes` → writes DA response bytes back to PTY
- `device_status` → writes DSR response bytes back to PTY
- `request_mode` → writes DECRPM response
- `kitty_keyboard_query` → writes current flags
- `xtversion` → writes version string
- `size_report` → writes terminal size
- `enquiry` → writes ENQ response

**Additional state management:**

- APC handler for kitty graphics protocol
- DCS handler for XTGETTCAP etc.
- Default cursor style tracking (CSI q integration with config)
- Config-driven behavior (OSC color report format, clipboard access policy)

The `surfaceMessageWriter` pushes messages to an `apprt.surface.Mailbox`
which crosses from the IO thread to the UI thread. The `messageWriter`
sends to the `termio.Mailbox` for responses that go back through the PTY.

### Why We Cannot Use StreamHandler Directly

StreamHandler is deeply coupled to Ghostty's application infrastructure.
Its struct fields require live instances of:

- `apprt.surface.Mailbox` — platform-native surface (macOS NSView / GTK
  widget), selected at compile time, no Windows variant exists
- `termio.Mailbox` — Ghostty's IO thread message queue for writing
  device responses back through Ghostty's own PTY layer
- `*renderer.State` + mutex — shared render lock with deadlock-avoidance
  unlock/relock patterns specific to Ghostty's two-thread architecture
- `xev.Async` — libxev event loop wakeup primitive (epoll/kqueue), we
  use GPUI's `cx.notify()` instead
- `configpkg.Config` fields — Ghostty's config system for OSC color
  report format, clipboard policy, cursor defaults, enquiry response

To use StreamHandler, you'd need to either fake Ghostty's entire runtime
or actually port Ghostty to Windows. At that point you're not building
ghostty-gpui, you're building Ghostty for Windows.

### Our Approach: Thin Wrapper Handler

We write a custom `Handler` in the Zig shim that:

1. **Delegates all actions to `ReadonlyHandler.vt()`** for state mutation
   (ReadonlyHandler no-ops on side-effect actions, so this is always safe)
2. **Additionally intercepts side-effect actions** to fire C callbacks
3. **Incrementally adds device responses** by writing response bytes to
   a buffer that the Rust side reads back and sends to the PTY

This is ~30–50 lines of Zig beyond what ReadonlyHandler already provides.

```zig
const ShimHandler = struct {
    readonly: stream_readonly.Handler,
    callbacks: Callbacks,
    response_buf: ResponseBuffer,

    pub fn vt(
        self: *ShimHandler,
        comptime action: Action.Tag,
        value: Action.Value(action),
    ) !void {
        // Always delegate to ReadonlyHandler for state mutation.
        // Safe for all actions — it no-ops on side-effect ones.
        try self.readonly.vt(action, value);

        // Then handle side-effects that ReadonlyHandler ignores.
        switch (action) {
            .bell => if (self.callbacks.bell) |cb| cb(self.callbacks.userdata),
            .window_title => if (self.callbacks.title) |cb|
                cb(self.callbacks.userdata, value.title.ptr, value.title.len),
            .clipboard_contents => if (self.callbacks.clipboard) |cb|
                cb(self.callbacks.userdata, ...),

            // Device responses — write to buffer, Rust reads after feed()
            .device_attributes => self.writeDA(value),
            .device_status => self.writeDSR(value),
            .kitty_keyboard_query => self.writeKittyQuery(),

            else => {},  // No additional handling needed
        }
    }
};
```

**Why this works well:**

- **Correctness**: ReadonlyHandler handles all ~60 state-mutation actions
  correctly, tracking upstream automatically on version bumps. We never
  duplicate state-mutation code.
- **Safety**: Calling ReadonlyHandler first for every action means we can
  never accidentally miss a state mutation that's coupled to a side-effect
  action (if Ghostty adds one in the future).
- **Incremental**: Device responses can be added one at a time. Programs
  have timeouts for missing DA/DSR responses, so the terminal works before
  they're all implemented.
- **Minimal surface**: The wrapper switch only lists ~10 side-effect
  actions. Everything else falls through to ReadonlyHandler via `else`.

**Phased delivery:**

- **v0**: Callbacks for `window_title` and `bell`. Working terminal with
  title updates.
- **v0.1**: Device responses (`device_attributes`, `device_status`,
  `kitty_keyboard_query`) via response buffer. Unlocks programs that
  probe terminal capabilities (neofetch, htop, etc.).
- **Later**: APC handler (kitty graphics), DCS handler (XTGETTCAP,
  DECRQSS), clipboard (OSC 52), notifications.
