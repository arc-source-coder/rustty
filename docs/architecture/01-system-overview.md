# System Overview

> **Update:** `portable-pty` has been replaced with a custom PTY backend
> ported from Alacritty (ConPTY on Windows, openpty on Unix). See
> [the migration plan](../plans/2026-02-24-pty-001-alacritty-backend.md).

> **Update:** The single-thread model (UI + IO on same thread) 
> has been replaced with 3-thread model inspired by Ghostty (UI, IO, PTY) See
> [the migration plan](../plans/2026-02-27-renderer-001-terminal-renderer.md).

## Intent

Build a Windows-first terminal emulator with:

- Ghostty 1.3.x internal terminal state machine via a custom Zig shim for emulation correctness and speed
- GPUI for app shell, rendering, and input handling
- `portable-pty` for PTY/process integration

## Architectural Priorities

1. Correctness first (emulation and IO ordering)
2. Performance second (incremental rendering, bounded queues)
3. Maintainability always (small crate APIs, explicit ownership)

## Workspace Structure

```text
crates/
  app/           -> binary, window lifecycle, tab management
  renderer/      -> GPUI terminal element (render + input + selection)
  terminal/      -> session orchestration, PTY↔VT bridge, input encoding
  pty/           -> portable-pty wrapper + process/session lifecycle
  ghostty_vt/    -> Zig shim build + FFI + safe Rust wrapper (single crate)
  ui/            -> titlebar, tabs, context menus, reusable components
vendor/
  ghostty/       -> pinned Ghostty snapshot (1.3.x)
  gpui/          -> vendored GPUI
```

## Ghostty Integration Strategy

The official Ghostty `libghostty-vt` C API (`vt.h`) only exposes parsers
(SGR, OSC, key encoder, paste checker). It does **not** expose the full
terminal state machine (grid, scrollback, cursor, dirty tracking).

We use a **custom Zig shim** that imports Ghostty's internal Zig modules
directly (`terminal/main.zig`, `input.zig`) and exports a focused C ABI.
This gives us the full terminal emulator without pulling in Ghostty's
renderer, font system, or app runtime.

The shim pattern is proven by the gpui-ghostty reference project. It couples
us to Ghostty internal APIs (not a stable ABI), so the shim must be updated
when upgrading Ghostty versions. The `ghostty_vt` crate isolates this
boundary from the rest of the codebase.

We do not reuse Ghostty's `termio` runtime directly. It is tightly coupled to
Ghostty's own app runtime (`xev`, surface/renderer mailboxes, renderer mutex
coordination). We mirror its IO and ordering invariants using our own
`portable-pty` thread + Rust channels.

A scalar UTF-8 decoder fallback is provided in the shim to avoid depending
on Ghostty's highway (C++ SIMD) dependency. This is a known future
optimization target.

## Ownership Boundaries

### App (`app`)

- Owns window lifecycle.
- Each window owns its own tab list and active tab state.
- Window closes itself when its last tab is removed (no global
  coordination needed — GPUI handles app exit when all windows close).
- Does not parse terminal bytes.

### UI (`ui`)

- Owns custom titlebar with tab strip (Windows Terminal style).
- Owns reusable UI components (menus, command palette, etc.).
- Does not own terminal state.

### Renderer (`renderer`)

- Owns visual/transient UI state (hover, IME preedit, cursor blink state,
  in-progress selection gesture state).
- Implements GPUI `Element`/`Render` for the terminal surface.
- Pulls render data from `terminal::TerminalSession` via pull API.
- Handles input event normalization and forwarding.
- Does not own canonical selection or viewport position.
- Does not own PTY or process handles.

### Terminal (`terminal`)

- Owns terminal session orchestration.
- Owns `ghostty_vt::Terminal` — the authoritative emulation instance.
- Owns canonical selection and viewport position through Ghostty terminal
  data structures (screen/page list).
- Encodes UI input events into VT bytes.
- Drains PTY output, feeds it to the terminal.
- Receives side-effect notifications (title, bell, clipboard) via shim
  callbacks; reads mode flags directly from terminal state.
- Forwards device response bytes back to PTY after each drain cycle.

### PTY (`pty`)

- Owns shell process spawn and PTY read/write/resize.
- Exposes typed commands/events to `terminal`.
- Does not import GPUI or rendering code.

### Ghostty VT (`ghostty_vt`)

- Single crate: Zig build (`build.rs`) + raw FFI bindings + safe Rust wrapper.
- Exports `Terminal` struct with `new`, `feed`, `resize`, `cursor_position`,
  row/style-run dump, dirty tracking, scroll, hyperlink queries.
- Exports key encoding utilities.

## Threading Model

- UI thread (GPUI foreground):
  - terminal emulation ownership
  - update draining and repaint scheduling
- Background thread (PTY):
  - blocking IO and process lifecycle

Invariant: no cross-thread terminal emulation access.

## Event and Data Flow

```text
Keyboard/Text/Mouse (GPUI)
  -> renderer (normalize event)
  -> terminal (encode VT bytes)
  -> pty command channel (Write)
  -> shell process

shell process output
  -> PTY read thread
  -> pty event channel (Bytes)
  -> terminal (feed ghostty_vt; handler fires title/bell/clipboard
     callbacks; response buffer collects device response bytes)
  -> render_update() refreshes persistent RenderState
  -> response bytes (if any) sent to PTY
  -> cx.notify() triggers repaint
  -> renderer reads from RenderState (zero-copy until next update)
```

## Why This Split

- Keeps non-Send emulation state in one place.
- Enables independent testing:
  - PTY integration tests in `pty`
  - encoding, mode scanning, and update tests in `terminal`
  - render behavior tests in `renderer`
- `ui` components are reusable and testable without terminal logic.
- Matches patterns proven in Zed (`terminal` vs `terminal_view`) and
  rust-terminal (`screen` / `renderer` / `pty` / `ui`).

## Alternatives Considered

1. Keep terminal emulation on background worker
   - Pros: UI less stateful
   - Cons: thread safety pressure, complexity in GPUI entity model
   - Decision: reject for v0

2. Single giant crate for everything
   - Pros: fast initial coding
   - Cons: quickly turns into tightly coupled code
   - Decision: reject

3. Per-cell direct painter only
   - Pros: easy prototype
   - Cons: likely performance rewrite later
   - Decision: use run-based renderer from day one

4. Use official `vt.h` C API only
   - Pros: stable API surface
   - Cons: only provides parsers, no terminal state machine
   - Decision: use Zig shim for full emulator access

5. Use `ghostty.h` embedding API
   - Pros: more stable than internal APIs
   - Cons: designed for macOS, owns PTY internally, heavy callback setup
   - Decision: reject — Zig shim gives us exactly the right layer
