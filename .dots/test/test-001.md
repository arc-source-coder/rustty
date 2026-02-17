---
title: v0 test suite (VT golden tests, PTY integration, renderer smoke tests)
status: open
priority: 2
created-at: "2026-02-23T03:14:39Z"
blockers:
  - ffi-001
  - pty-001
---

VT golden tests (in ghostty_vt): feed known escape sequences → verify row content, styles, colors, cursor position, dirty state. Cover SGR (bold, italic, fg/bg colors, reset), cursor movement (CUP, CUU/CUD/CUF/CUB), mode transitions (alt screen, origin mode), dirty-row extraction. PTY integration tests (in pty): spawn/write/read/exit lifecycle, resize correctness, command ordering guarantees. ConPTY edge cases: UTF-16 chunking across Output boundaries, resize during heavy output, fast close sequences, reliable Exited emission. Renderer smoke tests (in renderer): dirty state propagation (feed → render_update → verify dirty rows), cursor position + style after sequences, selection overlay after set_selection. End-to-end: spawn shell → type command → verify output appears in rendered rows. Refs: docs/architecture/05-roadmap.md § Test Plan, docs/architecture/04-pty-threading.md § ConPTY / Windows Testing Requirements.
