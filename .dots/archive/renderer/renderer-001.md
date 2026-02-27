---
title: Terminal renderer (GPUI Element, row-run painting, dirty tracking, cursor)
status: closed
priority: 1
created-at: "2026-02-23T03:13:50Z"
closed-at: "2026-03-01T06:16:38Z"
blockers:
  - terminal-001
---

GPUI Element/Render implementation for the terminal surface. Paint pipeline:
(1) compute grid size from bounds + font metrics,
(2) call session.begin_frame() for RenderFrame,
(3) check dirty state,
(4) iterate visible rows — skip non-dirty rows,
(5) for each row: extract cells, build style runs, shape text (cache by content hash: wyhash of codepoints + relative clusters + font_index),
(6) paint in order: default bg fill → non-default bg spans → selection overlay → highlight overlays → text runs → cursor overlay.

Cursor rendering: block/bar/underline based on cursor_style, blink timer (visual state only), dim when unfocused.
Full redraw triggers: font/DPI change, theme change, viewport geometry change, viewport pin change. Drop RenderFrame at end of paint (calls render_clear_dirty). Synchronized output: if mode active, skip paint entirely.

Update(terminal-001): TerminalSession now exposes `terminal()` for `begin_frame()` access and `set_cell_size(w, h)` for pixel metrics. The renderer should call `session.set_cell_size()` when font metrics change, and `session.terminal().begin_frame()` to get a `RenderFrame<'_>` for render data.

Refs: docs/architecture/03-rendering.md (full design), docs/architecture/02-data-model.md § Render Data Access, vendor/zed/crates/terminal_view (GPUI element reference), opensrc/repos/MitchForest/rust-terminal (alternate reference).

Update(terminal-001): Refer docs/architecture/10-new-renderer-design.md for the latest design.
