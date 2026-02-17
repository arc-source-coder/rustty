# Ghostty Alignment Decisions (2026-02-17)

This document captures architecture decisions made after cross-checking
Ghostty internals in `vendor/ghostty`.

## Decision 1: Viewport Source of Truth

Context:

- Ghostty stores viewport state in terminal core (`PageList.viewport`,
  `viewport_pin`, `viewport_pin_row_offset`).
- `RenderState.update()` reads viewport rows from `getTopLeft(.viewport)`.

Decision:

- Treat terminal viewport state as canonical.
- Do not keep a second Rust-side `scroll_offset` model.

Consequence:

- Fewer coordinate drift bugs for scrolling/search/selection.

## Decision 2: Coordinate Widths at Boundaries

Context:

- Ghostty uses `u16` for cell counts/x and `u32` for absolute y.

Decision:

- Use fixed-width boundary types:
  - viewport/local row+col: `u16`
  - absolute row: `u32`
- Do not expose `usize` in FFI APIs.

Consequence:

- Avoids ABI churn when scrollback/search grows.

## Decision 3: Dirty Model and Row Identity

Context:

- Ghostty uses `RenderState.Dirty` (`false`/`partial`/`full`) plus page/row
  dirty bits and viewport pin changes.

Decision:

- Mirror this dirty model directly.
- Use row pin identity in render caching keys; avoid introducing synthetic
  row generation counters for v0.

Consequence:

- Simpler implementation with behavior proven in Ghostty.

## Decision 4: FFI Lifetime Contract

Context:

- Render getters return borrowed views into shim-owned memory.

Decision:

- Borrowed render views are valid until next terminal mutation.
- No explicit frame lock API.
- Enforce safety in Rust wrapper with borrow lifetimes.

Mutation set:

- `feed`, `resize`, viewport scroll, selection/highlight mutation,
  `render_update`, `free`.

Consequence:

- Zero-copy reads remain practical without lock-heavy C ABI.

## Decision 5: Selection Ownership

Context:

- Ghostty stores canonical selection in `Screen.selection` with tracked pins;
  `RenderState` derives per-row x ranges.

Decision:

- Terminal owns semantic selection.
- Renderer owns only transient gesture state.

Consequence:

- Single source of truth for copy/select/search interactions.

## Decision 6: Mouse Encoding Boundary

Context:

- Ghostty mouse encoding depends on terminal mode/format flags and lives in
  surface/terminal boundary code.

Decision:

- UI emits semantic mouse events.
- Encoding to VT bytes happens in terminal/shim boundary code.

Consequence:

- Avoids protocol drift and duplicated encoding logic.

## Decision 7: Ghostty `termio` Reuse

Context:

- Ghostty `termio` is tightly coupled to `xev` and Ghostty mailbox/runtime
  internals.

Decision:

- Do not embed Ghostty `termio`.
- Mirror its invariants in Rust (`portable-pty` + bounded channels + strict
  write ordering + orderly shutdown/join).

Consequence:

- Keeps architecture maintainable while preserving behavior.

## Decision 8: Config Reload Simplicity

Context:

- Ghostty derives subsystem-specific config snapshots at consumption sites.

Decision:

- Keep reload simple: rebuild derived snapshots, fan out to sessions,
  trigger full redraw.
- No explicit config-version protocol required for v0.

Consequence:

- Extensible model without premature complexity.

## Decision 9: Search Prep (Now) vs Wiring (Later)

Decision:

- Prepare APIs and coordinate model now:
  - highlight set/clear APIs with tagged ranges in absolute coordinates
  - clear viewport/pin-based mapping rules
- Implement dedicated search worker + full wiring later.

Consequence:

- Avoids future rework while keeping v0 scope focused.
