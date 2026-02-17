# Windows GPUI + Ghostty Terminal Spec

Date: 2026-02-17
Status: Proposed
Owner: ghostty-gpui

## 1. Purpose

Build a fast, clean, GPU-accelerated terminal emulator for Windows using:

- GPUI for UI/window/input/render integration
- Ghostty 1.3.x internal terminal state machine (via custom Zig shim) for emulation
- `portable-pty` for PTY/process integration (Windows-first defaults)

Primary goal: a maintainable architecture that reaches high practical performance without over-engineering.

## 2. Product Goals

### In Scope (v0)

- Windows desktop app with custom titlebar and tabs
- One terminal session per tab
- PowerShell (`powershell.exe`) as default shell
- PTY lifecycle and resize support
- Terminal rendering with dirty-row updates
- Keyboard input including kitty keyboard protocol support
- Scrollback, selection, copy/paste basics
- Tab/window title updates from terminal sequences
- Vendored Ghostty snapshot (1.3.x series), vendored GPUI

### Out of Scope (v0)

- Split panes
- Settings UI
- Fluent-styled context/dropdown menus (planned after core stability)
- Advanced graphics protocols (sixel/kitty images)
- Remote terminal sessions

## 3. Non-Goals

- Pixel-perfect Windows Terminal clone
- Reusing Ghostty's renderer or app runtime directly
- Supporting multiple Zig versions
- Using the official `vt.h` C API as sole integration point (it only exposes parsers)
- Using the `ghostty.h` embedding API (designed for macOS, manages PTY internally)

## 4. Constraints and Decisions

- Ghostty source is vendored snapshot in-repo.
- GPUI is vendored for consistency.
- Zig must be exactly `0.15.2` for Ghostty build compatibility.
- App exits when the last tab closes.
- Default shell is `powershell.exe`.
- No new dependency additions unless justified by measurable need.
- Custom titlebar implementation for maximum control over UI.

## 5. Architecture Overview

### 5.1 High-Level Layers

```text
crates/
  ghostty_vt/    -> Zig shim + FFI + safe Rust wrapper (single crate)
  pty/           -> portable-pty wrapper, process lifecycle
  terminal/      -> session orchestration, input encoding, PTY↔VT bridge
  renderer/      -> GPUI terminal element, input handling, selection
  ui/            -> titlebar, tabs, context menus, reusable components
  app/           -> binary, window lifecycle, tab management
```

### 5.2 Ghostty Integration

The official `libghostty-vt` C API only provides parsers (SGR, OSC, key
encoder, paste safety). It does not expose the full terminal state machine.

We use a custom Zig shim (`ghostty_vt/zig/lib.zig`) that:

1. Imports Ghostty's internal `terminal.Terminal` + `terminal.Stream`
2. Implements a custom `Handler` wiring parser callbacks to terminal state
3. Exports a focused C ABI: `ghostty_vt_terminal_new/feed/resize/free`, row
   dumps, style runs, dirty tracking, cursor queries, key encoding
4. Provides a scalar UTF-8 decoder fallback to avoid the highway SIMD dependency

This gives us the full emulator without Ghostty's renderer, font system,
or app runtime. The tradeoff is coupling to internal APIs — the shim must
be updated on Ghostty version bumps, but the `ghostty_vt` crate boundary
isolates this from the rest of the codebase.

### 5.3 Authoritative State

Authoritative emulation state lives in `ghostty_vt::Terminal` and is owned
on the GPUI foreground thread only.

Rationale:

- Aligns with GPUI's single-foreground-thread model.
- Avoids unsafe cross-thread access to non-`Send` terminal state.
- Keeps rendering and state reads coherent.

Additional ownership rules:

- Viewport position is terminal-owned (Ghostty page list viewport pin);
  renderer does not keep a second scroll-offset model.
- Selection semantics are terminal-owned; renderer consumes per-row
  selection ranges from `RenderState`.

### 5.4 Threading Model

- Foreground/UI thread:
  - owns terminal emulation state
  - drains PTY output messages in a budgeted loop
  - calls `cx.notify()` to schedule repaint
- PTY background thread:
  - blocks on PTY read/write
  - sends bytes/events to UI via bounded channel
  - receives write/resize/close commands

### 5.5 Data Flow

Output path:

1. Shell writes to PTY
2. PTY thread reads bytes
3. Bytes sent to UI channel
4. UI drains bytes, feeds `ghostty_vt::Terminal` (handler fires callbacks
   for title/bell/clipboard; response buffer collects device response bytes)
5. `render_update()` refreshes persistent `RenderState`
6. Response bytes (if any) sent to PTY via write command
7. `cx.notify()` triggers repaint
8. Renderer reads dirty state, row cells, selection, cursor, colors
   directly from `RenderState` (zero-copy, valid until next `render_update()`)

Input path:

1. GPUI key/text event in renderer
2. Normalized event forwarded to terminal session
3. Terminal session encodes VT bytes (incl. kitty protocol)
4. Bytes sent to PTY thread for shell input

## 6. Rendering Design

Chosen strategy: row-run rendering + dirty-row invalidation.

- Pull visible rows only via `TerminalSession` pull API.
- Use style runs from Ghostty terminal for batching.
- Avoid per-cell full redraw loop as primary path.
- Cache shaped lines/runs keyed by row pin identity and font/style key.
- Overlay selection and cursor in paint phase.

Why this choice:

- Matches Ghostty's dirty tracking philosophy.
- Matches proven GPUI patterns in Zed-like terminal rendering.
- Better performance/complexity tradeoff than immediate per-cell repaint.

## 7. PTY and Process Design

- Start with `portable-pty` backend (cross-platform: ConPTY on Windows).
- Strong boundary: PTY crate does not know GPUI or rendering.
- PTY->UI channel is bounded to prevent unbounded memory growth.
- Resize commands are coalesced last-wins.

## 8. Reliability and Performance Requirements

- No UI thread blocking on PTY I/O.
- No unbounded queues/caches.
- Dirty-only redraw for routine output.
- Full redraw only on explicit triggers (font/theme/resize/global dirty).
- Stable resize behavior during high output.

## 9. Testing Strategy

Primary focus: golden VT + PTY integration.

- VT golden tests for escape sequences, style runs, cursor behavior.
- Integration tests with PTY session lifecycle (spawn/write/read/resize/exit).
- Rendering smoke tests for dirty row propagation and cursor/selection overlays.

## 10. Milestones

### v0 - Usable core

- tabs + session lifecycle
- powershell spawn
- input/output + resize
- kitty keyboard encoding path
- dirty-row rendering
- copy/paste + title updates

### v0.5 - Quality pass

- stronger selection behavior
- hyperlink interactions
- IME robustness
- performance profiling and cache tuning

### v1 - UX expansion

- split panes
- richer settings and keymaps
- Fluent-inspired context/dropdown menus

## 11. Acceptance Criteria (v0)

- Can run sustained commands (`cargo build`, long output) without major UI hitching.
- Resizing during output keeps grid coherent and readable.
- Typing latency remains responsive while output stream is active.
- Title/tab updates work from terminal control sequences.
- Closing final tab exits app predictably.

## 12. Risks and Mitigations

1. Zig/toolchain mismatch
   - Mitigation: enforce exact Zig `0.15.2` check in build script with explicit error.

2. Ghostty internal API changes on version bump
   - Mitigation: Zig shim isolates breakage to `ghostty_vt` crate only.

3. Channel backpressure and output bursts
   - Mitigation: bounded channels, budgeted UI drain loop, no byte dropping.

4. Rendering cost spikes
   - Mitigation: run-based shaping cache + dirty row invalidation + viewport culling.

5. Windows resize edge cases
   - Mitigation: coalesced resize policy and explicit PTY/session synchronization.

## 13. Architecture Invariants

1. Terminal emulation state is single-owner on UI thread.
2. PTY operations are isolated to background thread.
3. Cross-thread messages are bytes/control only.
4. Buffers and channels are bounded.
5. Rendering is incremental-first.
6. Input encoding logic resides in `terminal` and is testable.
7. App/window chrome is decoupled from terminal emulation internals.
8. Zig shim is the sole Ghostty integration boundary.
9. FFI render views are valid until next terminal mutation.
10. Ghostty `termio` runtime is not embedded; its IO/order invariants are
    mirrored in Rust `pty`/`terminal` layers.
