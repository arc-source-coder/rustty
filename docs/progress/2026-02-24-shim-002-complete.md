# shim-002: Render State + Query Exports — Complete

**Date:** 2026-02-24
**Status:** ✅ Done — `cargo test -p ghostty_vt` passes all 18 tests on Windows (MSVC)

---

## What was built

Exported RenderState lifecycle, dirty tracking, cell data (flat C struct), mode flag queries, viewport scroll, selection, and key/mouse encoding from the Zig shim.

### Exported C ABI

| Function | Signature |
| --- | --- |
| `ghostty_vt_terminal_get_mouse_mode` | `(ptr) -> u8` |
| `ghostty_vt_terminal_get_mouse_format` | `(ptr) -> u8` |
| `ghostty_vt_terminal_is_bracketed_paste` | `(ptr) -> u8` |
| `ghostty_vt_terminal_get_kitty_keyboard_flags` | `(ptr) -> u8` |
| `ghostty_vt_terminal_scroll_viewport` | `(ptr, delta: i32) -> void` |
| `ghostty_vt_terminal_scroll_viewport_top` | `(ptr) -> void` |
| `ghostty_vt_terminal_scroll_viewport_bottom` | `(ptr) -> void` |
| `ghostty_vt_terminal_render_update` | `(ptr) -> c_int` |
| `ghostty_vt_terminal_render_dirty` | `(ptr) -> u8` |
| `ghostty_vt_terminal_render_clear_dirty` | `(ptr) -> void` |
| `ghostty_vt_terminal_render_rows` | `(ptr) -> u16` |
| `ghostty_vt_terminal_render_cols` | `(ptr) -> u16` |
| `ghostty_vt_terminal_render_row_dirty` | `(ptr, row: u16) -> u8` |
| `ghostty_vt_terminal_render_cursor` | `(ptr, out: *CursorState) -> c_int` |
| `ghostty_vt_terminal_render_colors` | `(ptr, out: *ColorState) -> c_int` |
| `ghostty_vt_terminal_render_palette_color` | `(ptr, index: u8, out: *ColorRGB) -> c_int` |
| `ghostty_vt_terminal_render_row_cells` | `(ptr, row: u16, out_len: *u16) -> ?[*]FlatCell` |
| `ghostty_vt_terminal_render_cell_grapheme` | `(ptr, row, col: u16, out_len: *u8) -> ?[*]u32` |
| `ghostty_vt_terminal_render_row_selection` | `(ptr,, start_x, row: u16 end_x: *u16) -> u8` |
| `ghostty_vt_terminal_set_selection` | `(ptr, start_x, start_y, end_x, end_y, rectangular) -> c_int` |
| `ghostty_vt_terminal_clear_selection` | `(ptr) -> void` |
| `ghostty_vt_terminal_get_selection_text` | `(ptr, out_len: *usize) -> ?[*]u8` |
| `ghostty_vt_bytes_free` | `(bytes: ?[*]u8, len: usize) -> void` |
| `ghostty_vt_terminal_encode_key` | `(ptr, key, mods, action, text_ptr, text_len, buf, buf_len) -> usize` |
| `ghostty_vt_terminal_encode_mouse` | `(ptr, button, action, mods, x, y, buf, buf_len) -> usize` |

### Files

- `crates/ghostty-vt/zig/lib.zig` — All render, mode, scroll, selection, and encoding exports
- `crates/ghostty-vt/zig/input.zig` — Key encoding export (fixed IO bug)
- `crates/ghostty-vt/src/lib.rs` — Rust FFI declarations + structs (CursorState, ColorRGB, ColorState, FlatCell)
- `crates/ghostty-vt/src/tests/*.rs` — Integration tests for all exports

### Tests

All 18 tests pass:
- `test_new_free`, `test_feed_ascii`, `test_bell_callback`, `test_resize` (core)
- `test_mode_flags_default`, `test_bracketed_paste_enabled`, `test_mouse_mode_enabled` (modes)
- `test_scroll_viewport` (scroll)
- `test_render_update_empty`, `test_render_partial_dirty`, `test_render_cursor`, `test_render_colors`, `test_render_palette`, `test_render_row_cells`, `test_render_styled_cell`, `test_render_row_selection_none` (render)
- `test_selection_set_clear` (selection)
- `test_encode_key_basic` (key encoding)

---

## Key decisions & resolutions

### std.Io.Writer vs std.io (Open Question #1)

Fixed the key encoding IO bug by using `std.Io.Writer.fixed()` directly instead of `std.io.fixedBufferStream().writer().any()`. The latter returns `*Io.DeprecatedWriter` which is incompatible with `key_encode.encode()` expecting `*std.Io.Writer`. This matches Ghostty's own C API implementation in `terminal/c/key_encode.zig`.

### selectionString allocator (Open Question #2)

Used `handle.alloc` (smp_allocator) for allocation in `get_selection_text`, matching `ghostty_vt_bytes_free` which also uses smp_allocator. Both are stateless globals, so they match.

### FlatCell ABI stability (Open Questions #4, #5)

Verified `@sizeOf(FlatCell) == 28` and `@alignOf(FlatCell) == 4` with compile-time assertions. The `style_flags` uses `@bitCast` from Ghostty's packed Style.Flags (u16) - intentionally fragile but simple/fast per the plan.

### Key enum values (Open Question #6)

Fixed the test to use the correct enum value (58 for `Key.enter`). The plan's placeholder value (0x28/40) was incorrect.

### fbs.pos field (Open Question #7)

Used `writer.end` field from `std.Io.Writer.fixed()` instead of `fbs.pos` from the deprecated FixedBufferStream.

---

## What's next

The shim now provides the complete C ABI surface needed by `ffi-001`:
- **FFI bindings + safe Rust wrapper** — Build the ergonomic `Terminal` struct and `RenderFrame` borrow guard
- **PTY integration** — Connect the shim to actual PTY input/output
