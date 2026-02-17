---
title: Scrollback + viewport (terminal-owned viewport, scroll APIs, wheel/keyboard scroll)
status: open
priority: 2
created-at: "2026-02-23T03:14:37Z"
blockers:
  - renderer-001
---

Terminal-owned viewport — no Rust-side scroll_offset model. Scroll commands: mouse wheel delta → ghostty_vt_terminal_scroll_viewport(delta). Keyboard: Page Up/Down, Shift+PgUp/PgDn, Home/End for top/bottom. After scroll: render_update() detects viewport pin change → dirty becomes .full → cx.notify() → full repaint. Scroll-to-bottom on new output: if viewport was at bottom before output, auto-scroll (check viewport mode). If user has scrolled up, do NOT auto-scroll (preserve reading position). Scrollbar indicator: optional for v0, but consider a minimal position indicator overlay. Refs: docs/architecture/02-data-model.md § Viewport Ownership, docs/architecture/03-rendering.md § Scroll Handling, docs/architecture/08-ghostty-alignment-decisions.md § Decision 1, docs/architecture/06-ghostty-shim.md § Scroll.
