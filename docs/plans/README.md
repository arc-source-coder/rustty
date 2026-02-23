# Implementation Plans Index

Plans needed before implementation. Each plan should detail the implementation
approach, API surfaces, key decisions, and acceptance criteria for a specific
area. Plans are ordered by dependency — earlier plans must be completed (or at
least designed) before later ones can begin.

Track progress with `dot list` / `dot show <id>`.

---

## Phase 0 — Foundation

### Plan 01: Zig Shim Build System + Core Shim (`shim-001`)

Scope: `crates/ghostty-vt` (Zig side + build.rs)

Set up `build.rs` to compile the Zig shim against vendored Ghostty 1.3.x
source. Write the core Zig shim: `TerminalHandle` struct, `ShimHandler` with
`ReadonlyHandler` delegation, scalar UTF-8 fallback, lifecycle exports
(`new`/`free`), `feed`, `resize`. Zig 0.15.2 version guard.

Key references:

- `docs/architecture/06-ghostty-shim.md` (full shim design, ABI surface, handler architecture)
- `docs/architecture/01-system-overview.md` § Ghostty Integration Strategy
- `opensrc/repos/Xuanwo/gpui-ghostty` (reference shim for 1.2.x — adapt, don't copy)
- `crates/ghostty-vt/zig/ghostty/src/terminal/` (internal APIs we import)

### Plan 02: Shim Render State + Query Exports (`shim-002`)

Scope: `crates/ghostty-vt` (Zig side)

Add persistent `RenderState` to the shim. Export: `render_update`,
`render_dirty`, `render_row_dirty`, `render_row_cells`, `render_row_selection`,
`render_cursor`, `render_colors`, `render_clear_dirty`. Export mode flag
queries (`mouse_mode`, `mouse_format`, `bracketed_paste`,
`kitty_keyboard_flags`). Export viewport scroll APIs (`scroll_viewport`,
`top`, `bottom`). Export selection APIs (`set_selection`, `clear_selection`, `get_selection_text`).

Key references:

- `docs/architecture/06-ghostty-shim.md` § RenderState, § Shim C ABI Surface
- `docs/architecture/03-rendering.md` § Dirty and Cache Model
- `crates/ghostty-vt/zig/ghostty/src/terminal/render.zig` (RenderState implementation)

### Plan 03: FFI Bindings + Safe Rust Wrapper (`ffi-001`)

Scope: `crates/ghostty-vt` (Rust side)

Write raw FFI declarations for the C ABI exports (manual `extern "C"` or
bindgen). Build the safe `ghostty_vt::Terminal` Rust wrapper with
`RenderFrame<'_>` borrow guard for lifetime safety. Implement `new`, `feed`,
`resize`, `render_update`, row/cell access, cursor, colors, dirty tracking,
mode flag queries, key encoding. Write smoke tests: feed bytes, read rows,
verify dirty state.

Key references:

- `docs/architecture/02-data-model.md` § Render Data Access and Frame Lifetime Safety
- `docs/architecture/06-ghostty-shim.md` § FFI Lifetime and Thread Contract
- `docs/architecture/08-ghostty-alignment-decisions.md` § Decision 4: FFI Lifetime Contract

### Plan 04: PTY Crate (`pty-001`)

Scope: `crates/pty`

Wrap `portable-pty` with typed `PtyCommand`/`PtyEvent` enums. Bounded channels
(64 capacity). Spawn PowerShell, read/write/resize, process lifecycle. Shutdown
ordering: signal stop → unblock read → drain → emit Exited → join threads.
Resize coalescing (last-wins). Integration tests for spawn/read/write/resize/exit.

Key references:

- `docs/architecture/04-pty-threading.md` (full PTY design)
- `docs/architecture/02-data-model.md` § PTY Channel Bounds, § PTY Model

---

## Phase 1 — First Vertical Slice

### Plan 05: Terminal Session Core (`terminal-001`)

Scope: `crates/terminal`

`TerminalSession` as GPUI Entity: owns `ghostty_vt::Terminal` + PTY channels.
Budgeted drain loop (2ms wall-clock). Side-effect queue (`SideEffect` enum:
TitleChanged, Bell, ClipboardWrite, ClipboardRead — processed after drain).
Device response forwarding (ResponseBuffer → PtyCommand::Write, before user
input). `SessionMetadata` + `ProcessState`. `SpawnConfig` + `RenderConfig`
model types. Wire `cx.notify()` for repaint scheduling. Synchronized output
mode handling (defer notify, 1s safety timer).

Key references:

- `docs/architecture/02-data-model.md` § Terminal Session Model, § Device Responses
- `docs/architecture/04-pty-threading.md` § Budgeted Drain Policy, § Write Ordering Rules
- `docs/architecture/03-rendering.md` § Synchronized Output

### Plan 06: Input Encoding (`terminal-002`)

Scope: `crates/terminal` + `crates/renderer` boundary

`TerminalInput` enum (Text/Key/Paste/Mouse/FocusChanged). GPUI event
normalization in renderer → `TerminalInput` dispatch to session. Key encoding
via shim `encode_key` (uses `Options.fromTerminal()` internally — full protocol
awareness). Mouse encoding via shim `encode_mouse`. Paste handling with
bracketed paste mode check. Focus change reporting.

Key references:

- `docs/architecture/06-ghostty-shim.md` § Key Encoding and Kitty Keyboard Protocol
- `docs/architecture/06-ghostty-shim.md` § Mouse Encoding Boundary
- `docs/architecture/02-data-model.md` § Input Model
- `docs/architecture/04-pty-threading.md` § Mouse Encoding Boundary

### Plan 07: Terminal Renderer (`renderer-001`)

Scope: `crates/renderer`

GPUI `Element` (or `Render` on a wrapper) for the terminal surface. Row-run
painting pipeline: pull visible rows from `TerminalSession.begin_frame()`,
batch text by style runs, paint in order (default bg → non-default bg spans →
selection overlay → highlight overlays → text runs → cursor overlay). Font
metrics → grid size calculation. Shaped-run cache keyed by content hash
(codepoints + relative clusters + font index). Dirty-row skip path. Full
redraw triggers (font/DPI/theme/viewport geometry change). Cursor rendering
(block/bar/underline, blink state, focus state).

Key references:

- `docs/architecture/03-rendering.md` (full rendering design)
- `docs/architecture/02-data-model.md` § Render Data Access and Frame Lifetime Safety
- `vendor/zed/crates/terminal_view` (GPUI terminal element reference)
- `opensrc/repos/MitchForest/rust-terminal` (alternate GPUI terminal reference)

### Plan 08: App Shell + Window Lifecycle (`app-001`)

Scope: `crates/app`

`Workspace` struct: `tabs: Vec<TabId>`, `tab_entries: HashMap<TabId, TabEntry>`,
`active_tab: TabId`. `TabContent::Terminal(Entity<TerminalSession>)`. `TabId` /
`SessionId` via `AtomicU64` counter. Single-tab bootstrap: open window → spawn
terminal → render. Window close on last tab removed. GPUI app exit when all
windows close. Wire `RenderConfig` as shared `Model<RenderConfig>`.

Key references:

- `docs/architecture/02-data-model.md` § App-Level Models, § Stable Identity
- `docs/architecture/01-system-overview.md` § Ownership Boundaries
- `docs/architecture/02-data-model.md` § RenderConfig Sharing

---

## Phase 2 — Tabs and App Shell

### Plan 09: Custom Titlebar + Tab Strip (`ui-001`)

Scope: `crates/ui` + `crates/app`

Windows Terminal-style custom titlebar with integrated tab strip. Tab
components: tab label (title from session metadata, fallback to shell program),
close button, active indicator. New tab button (+). Tab switching (click,
Ctrl+Tab / Ctrl+Shift+Tab). Tab close (middle-click, close button, Ctrl+W).
App-level keybindings (Ctrl+T new tab, Ctrl+W close, etc.). Focus transfer on
tab switch. Drag-to-reorder prep (stable IDs make it Vec::remove + Vec::insert).

Key references:

- `docs/reference/windows-terminal-tab-titlebar.png` (design reference)
- `docs/reference/excalidraw-mockup.png` (layout mockup)
- `docs/architecture/07-future-design.md` § Tab Reordering / Drag-and-Drop
- `vendor/zed/crates/ui` (GPUI UI component patterns)

---

## Phase 3 — v0 Quality Features

### Plan 10: Selection + Copy/Paste (`feature-001`)

Scope: `crates/renderer` + `crates/terminal`

Selection gestures: click to position cursor, drag to select, double-click
for word, triple-click for line. Terminal-owned selection via shim
(`set_selection`/`clear_selection`). Per-row selection ranges from
`RenderState` after `render_update()`. Selection rendering as overlay
(independent from text cache). Clipboard: copy selected text, paste with
bracketed paste check. Rectangular selection mode.

Key references:

- `docs/architecture/02-data-model.md` § Selection Ownership
- `docs/architecture/06-ghostty-shim.md` § Selection
- `docs/architecture/08-ghostty-alignment-decisions.md` § Decision 5: Selection Ownership

### Plan 11: Scrollback + Viewport (`feature-002`)

Scope: `crates/renderer` + `crates/terminal`

Terminal-owned viewport (no Rust-side scroll_offset). Scroll commands via shim
APIs (`scroll_viewport(delta)`, `scroll_viewport_top`, `scroll_viewport_bottom`).
Mouse wheel → scroll. Keyboard scroll (Page Up/Down, Shift+PgUp/PgDn). Render
after scroll: `render_update()` sees viewport pin change → `.full` dirty.
Scroll-to-bottom on new output when at bottom. Optional scrollbar indicator.

Key references:

- `docs/architecture/02-data-model.md` § Viewport Ownership
- `docs/architecture/03-rendering.md` § Scroll Handling
- `docs/architecture/08-ghostty-alignment-decisions.md` § Decision 1: Viewport Source of Truth
- `docs/architecture/06-ghostty-shim.md` § Scroll

### Plan 12: Device Responses (`terminal-003`)

Scope: `crates/ghostty-vt` (shim handler) + `crates/terminal`

Implement device response generation in the shim handler's `ResponseBuffer`:
v0 required responses — DA (device attributes), DSR (device status report),
DECRPM (request mode), kitty keyboard query, size report, ENQ. Wire response
buffer read + `PtyCommand::Write` forwarding in the drain loop (high priority,
before user input). Test with programs that probe capabilities (fastfetch, htop,
fish shell).

Key references:

- `docs/architecture/02-data-model.md` § Device Responses
- `docs/architecture/04-pty-threading.md` § Device Response Scope, § Write Ordering Rules
- `docs/architecture/06-ghostty-shim.md` § ShimHandler (response buffer design)

### Plan 13: v0 Test Suite (`test-001`)

Scope: cross-cutting

VT golden tests: escape sequences (SGR, cursor movement, mode transitions),
style run extraction, dirty-row behavior. PTY integration tests:
spawn/write/read/resize/exit, ConPTY edge cases (UTF-16 chunking, resize during
output, fast close, process exit detection). Renderer smoke tests: dirty
propagation, cursor/selection overlays, tab switch + focus. End-to-end: spawn
shell → type command → verify output rendered.

Key references:

- `docs/architecture/05-roadmap.md` § Test Plan
- `docs/architecture/04-pty-threading.md` § ConPTY / Windows Testing Requirements

---

## Dependency Graph

```text
Plan 01 (Zig Shim Core)
  └─→ Plan 02 (Render State Exports)
       └─→ Plan 03 (FFI + Rust Wrapper)
            ├─→ Plan 05 (Terminal Session) ←── Plan 04 (PTY) [independent]
            │    ├─→ Plan 06 (Input Encoding)
            │    ├─→ Plan 07 (Renderer)
            │    │    └─→ Plan 08 (App Shell)
            │    │         └─→ Plan 09 (Titlebar + Tabs)
            │    ├─→ Plan 10 (Selection) ←── Plan 07
            │    ├─→ Plan 11 (Scrollback) ←── Plan 07
            │    └─→ Plan 12 (Device Responses)
            └─→ Plan 13 (Test Suite) ←── Plan 04
```

## Resolved Decisions

- **Shell**: `powershell.exe` for v0. Support for `pwsh.exe`, `cmd.exe`, etc. later.
- **IME**: Deferred to v0.5 — complex, Ghostty itself is still working on this.
- **Error UX**: Show error message in the terminal area (like Windows Terminal).
- **Environment variables**: Inherit parent process env. Override TERM and
  COLORTERM from `SpawnConfig`. No issues with this approach.
- **Resize debounce**: Decide when we can test visually. Architecture says
  last-wins coalescing; specific timing TBD.
- **Selection text extraction**: `get_selection_text` added to shim-002 ABI.

## Still Open

- **App-level keybindings**: Where do shortcuts like Ctrl+T, Ctrl+W live?
  Plan 09 covers this but the binding system itself may need design.
- **Font fallback chain**: Cascadia Mono → Consolas → system monospace.
  Quality-of-life feature — implement when renderer is working.
