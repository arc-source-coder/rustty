---
title: Selection + copy/paste (gestures, terminal-owned selection, clipboard)
status: open
priority: 2
created-at: "2026-02-23T03:14:37Z"
blockers:
  - app-001
---

Selection gestures in renderer:
- click to clear selection + position cursor
- click-drag to select range
- double-click for word selection
- triple-click for line selection. 
- ctrl+a/cmd-a (mac) for selecting currently typed shell input
- ctrl+x/cmd+x (mac) for cutting input (only in case of shell input)
- ctrl+c/cmd-c (mac) for copying selection (on windows, when no selection Ctrl+C is signal)
- ctrl+v/cmd+v (mac) for pasting from clipboard
 
Renderer owns transient gesture state (drag anchor, current mouse position).
On gesture complete/update:
- call terminal session set_selection(start, end, rectangular).
- Terminal forwards to shim: ghostty_vt_terminal_set_selection → writes to Screen.selection. 
- Next render_update() populates per-row selection x-ranges in RenderState.
- Renderer reads row.selection and paints selection overlay (independent from text layout cache).
- Copy: extract selected text from terminal (need shim export for selection text), write to system clipboard.
- Paste: read system clipboard, check bracketed_paste mode, send via PtyCommand::Write. Rectangular selection mode (Alt+drag).

Refs: docs/architecture/02-data-model.md § Selection Ownership, docs/architecture/06-ghostty-shim.md § Selection, docs/architecture/08-ghostty-alignment-decisions.md § Decision 5.
