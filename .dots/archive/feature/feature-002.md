---
title: Scrollback + viewport (terminal-owned viewport, scroll APIs, wheel/keyboard scroll)
status: closed
priority: 2
created-at: "2026-02-23T03:14:37Z"
closed-at: "2026-03-07T10:43:36Z"
blockers:
  - app-001
  - terminal-002
---

Terminal-owned viewport — no Rust-side scroll_offset model. Scroll commands: mouse wheel delta → ghostty_vt_terminal_scroll_viewport(delta). Keyboard: Page Up/Down, Shift+PgUp/PgDn, Home/End for top/bottom. After scroll: render_update() detects viewport pin change → dirty becomes .full → cx.notify() → full repaint. Scroll-to-bottom on new output behavior: if viewport was at bottom before output, auto-scroll (check viewport mode). If user has scrolled up, do NOT auto-scroll (preserve reading position). Scrollbar indicator: simple overlay scrollbar - based off Windows Fluent design and Zed's rounded scrollbar. Refs: docs/architecture/02-data-model.md § Viewport Ownership, docs/architecture/03-rendering.md § Scroll Handling, docs/architecture/08-ghostty-alignment-decisions.md § Decision 1, docs/architecture/06-ghostty-shim.md § Scroll.
