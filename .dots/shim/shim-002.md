---
title: Shim render state + query exports (RenderState, dirty tracking, mode flags, scroll, selection)
status: open
priority: 1
created-at: "2026-02-23T03:13:03Z"
blockers:
  - shim-001
---

Add persistent RenderState to the Zig shim. Export: render_update, render_dirty (0=false/1=partial/2=full), render_row_dirty, render_row_cells, render_row_selection, render_cursor, render_colors, render_clear_dirty, render_rows, render_cols. Export mode flag queries: get_mouse_mode, get_mouse_format, is_bracketed_paste, get_kitty_keyboard_flags. Export viewport scroll: scroll_viewport(delta), scroll_viewport_top, scroll_viewport_bottom. Export selection: set_selection, clear_selection, get_selection_text (extract selected text as bytes for clipboard copy). Export key/mouse encoding: encode_key, encode_mouse. Refs: docs/architecture/06-ghostty-shim.md § RenderState + § Shim C ABI Surface, docs/architecture/03-rendering.md § Dirty and Cache Model, crates/ghostty-vt/zig/ghostty/src/terminal/render.zig.
