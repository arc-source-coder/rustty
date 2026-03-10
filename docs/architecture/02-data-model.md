# Data Model

> **IMPORTANT:** The FFI design is superseded by the design in [12-zero-copy-ffi-rework.md](12-zero-copy-ffi-rework.md)

> **Update:** The single-thread model (UI + IO on same thread)
> has been replaced with 3-thread model inspired by Ghostty (UI, IO, PTY) See
> [the migration plan](../plans/2026-02-27-renderer-001-terminal-renderer.md).

## Design Rules

- Keep terminal emulation state opaque behind `terminal::TerminalSession`.
- Keep UI/visual state separate from emulation state.
- Use explicit message enums for cross-thread communication.
- Use stable newtypes (`TabId`, `SessionId`) for identity — not vector
  indices or GPUI `EntityId`.

## Stable Identity

```rust
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TabId(u64);

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct SessionId(u64);
```

Generated via a module-level `AtomicU64` monotonic counter at creation time:

```rust
fn next_id() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}
```

These survive reordering, drag-and-drop, and (later) serialization for
session restore. Must be allocated at object creation — never derive from
`Vec` index or GPUI `EntityId`.

## App-Level Models

```rust
pub struct Workspace {
    pub tabs: Vec<TabId>,
    pub tab_entries: HashMap<TabId, TabEntry>,
    pub active_tab: TabId,
}

pub struct TabEntry {
    pub content: TabContent,
}

pub enum TabContent {
    Terminal(Entity<TerminalSession>),
    // Future: PaneTree(PaneNode) for split panes
}
```

Notes:

- `tabs` defines ordering. `tab_entries` holds data. `active_tab` is by ID.
- Tab title is derived from the active session's metadata (single source
  of truth), not stored separately on `TabEntry`.
- Window closes itself when `tabs.is_empty()`.
- `TabContent` enum allows future split panes without migrating the tab
  model.

## Terminal Session Model

```rust
pub struct TerminalSession {
    id: SessionId,
    terminal: ghostty_vt::Terminal,
    size: GridSize,
    spawn_config: SpawnConfig,
    pty_tx: Sender<PtyCommand>,
    metadata: SessionMetadata,
    process_state: ProcessState,
}
```

Notes:

- `terminal` is authoritative emulation state (on UI thread only).
- Mode flags (bracketed paste, mouse reporting, kitty keyboard, cursor
  visibility, etc.) are read directly from `terminal.modes` /
  `terminal.flags` via the Zig shim — not tracked separately. See
  `06-ghostty-shim.md` § Terminal Mode Flags for the full list.
- Title is updated via the shim handler's `window_title` callback,
  not by scanning PTY output bytes.

### Session Metadata

```rust
pub struct SessionMetadata {
    pub title: Option<String>,
    pub cwd: Option<PathBuf>,
    pub bell_count: u32,
    pub has_unread_output: bool,
}
```

Populated by shim callbacks (`window_title`, `report_pwd`, `bell`).
Tab title is derived from `metadata.title`, falling back to
`spawn_config.shell_program`.

### Process State

```rust
pub enum ProcessState {
    Running,
    Exited(ExitStatus),
    Error(String),
}
```

## Coordinate Types

```rust
pub struct GridSize {
    pub cols: u16,
    pub rows: u16,
}

/// Visible viewport coordinate (always within current viewport bounds).
pub struct ViewCoord {
    pub x: u16,
    pub y: u16,
}

/// Scrollback-aware absolute coordinate in terminal space.
/// Mirrors Ghostty's shape: x is cell count, y can exceed viewport height.
pub struct GridCoord {
    pub x: u16,
    pub y: u32,
}
```

Rules:

- Keep `u16` for viewport-local row/col and grid dimensions.
- Keep `u32` for absolute row coordinates in scrollback-aware operations.
- Do not expose `usize` in FFI structs or messages.

## Viewport Ownership

Viewport position is owned by Ghostty terminal state (`PageList.viewport`,
`viewport_pin`, `viewport_pin_row_offset`) and is the only source of truth.

- Rust does not keep a separate `scroll_offset` model.
- Scroll commands call shim viewport APIs (`scroll_viewport`, `top`,
  `bottom`) and then trigger `render_update()`.
- Render rows are interpreted relative to terminal viewport state. No
  `y + scroll_offset` math exists in Rust-side model code.

Coordinate mapping:

- Mouse gestures arrive in view coordinates (pixel -> `ViewCoord`).
- Selection/search/highlight state is stored in absolute `GridCoord` space.
- Conversions use terminal viewport origin/pins from current render state.

## Selection Ownership

Canonical selection is terminal-owned, matching Ghostty's `Screen.selection`
model.

- `terminal` owns semantic selection bounds and rectangular mode.
- `renderer` only owns transient interaction state (drag anchor, hover).
- Renderer reads per-row selection ranges from `RenderState` after
  `render_update()`.

## RenderConfig Sharing

`RenderConfig` is shared across sessions via a GPUI `Model<RenderConfig>` on
the UI thread. Sessions read it as a snapshot; config reload replaces the
model value and calls `cx.notify()` to trigger redraws.

This matches Ghostty's pattern (each subsystem gets a **deeply-copied
`DerivedConfig` by value** at reload time) adapted to GPUI's entity model:

```rust
// In app/workspace — one config model per window (or app-global).
let render_config: Model<RenderConfig> = cx.new_model(|_| RenderConfig::default());

// Each TerminalSession holds a read handle.
pub struct TerminalSession {
    render_config: Model<RenderConfig>,   // read-only snapshot on demand
    // ...
}
```

On config reload: update `render_config` model, call `cx.notify()` on all
sessions. Sessions pull the new snapshot at the start of the next render
cycle. `SpawnConfig` is never touched after session creation.

## PTY Channel Bounds

Both channels carry concrete capacity limits — no unbounded queues:

```rust
// PTY → UI: bytes and lifecycle events.
// 64 messages, matching Ghostty's BlockingQueue capacity for all mailboxes.
// At a typical 64 KB chunk this is ~4 MB max in-flight — sufficient
// backpressure without excessive memory. If full, PTY read thread blocks.
const PTY_EVENT_CHANNEL_CAPACITY: usize = 64;

// UI → PTY: write/resize/close commands.
// Lower bound is fine — resize is coalesced and writes are batched per tick.
const PTY_COMMAND_CHANNEL_CAPACITY: usize = 64;
```

These constants live in the `pty` crate and are referenced by `terminal`
when constructing the channels. Do not use unbounded channels.

## PTY Model

```rust
pub enum PtyCommand {
    Write(Vec<u8>),
    Resize(GridSize),
    Close,
}

pub enum PtyEvent {
    Output(Vec<u8>),
    Exited(ExitStatus),
    Error(String),
}
```

Invariants:

- Write commands preserve byte ordering.
- Resize is last-wins coalesced in PTY layer.
- PTY event channel is bounded.

## Input Model

```rust
pub enum TerminalInput {
    Text(String),
    Key(NormalizedKeyEvent),
    Paste(String),
    Mouse(NormalizedMouseEvent),
    FocusChanged(bool),
}
```

Encoding boundary:

- `renderer` normalizes GPUI events into `TerminalInput`.
- `terminal` encodes `TerminalInput` into VT bytes.

## Render Data Access and Frame Lifetime Safety

The renderer pulls data from the persistent `RenderState` during
`render()`. Since both emulation and rendering run on the GPUI foreground
thread, no concurrent access occurs.

### Frame Guard Pattern

To prevent accidental retention of Zig-owned memory (cell slices, style
data, hyperlink bytes), all render data access goes through a borrow
guard:

```rust
impl TerminalSession {
    /// Borrow render state for the current frame.
    /// While the returned `RenderFrame` exists, `render_update()` cannot
    /// be called (enforced by `&self` borrow on single-threaded UI).
    pub fn begin_frame(&self) -> RenderFrame<'_> {
        RenderFrame { session: self }
    }
}

pub struct RenderFrame<'a> {
    session: &'a TerminalSession,
}

impl<'a> RenderFrame<'a> {
    pub fn dirty(&self) -> DirtyState { ... }
    pub fn row_dirty(&self, y: u16) -> bool { ... }
    pub fn row_cells(&self, y: u16) -> &[Cell] { ... }
    pub fn row_selection(&self, y: u16) -> Option<(u16, u16)> { ... }
    pub fn cursor(&self) -> CursorState { ... }
    pub fn colors(&self) -> &Colors { ... }
}

impl<'a> Drop for RenderFrame<'a> {
    fn drop(&mut self) {
        self.session.terminal.render_clear_dirty();
    }
}
```

Invariants:

- No `render_update()` while any `RenderFrame` exists (enforced by
  `&self` borrow — `render_update()` takes `&mut self`).
- Renderer must not store any row/cell references outside the paint call.
- If caching is needed across frames, cache **owned** shaped text keyed
  by `(viewport_row, row_pin_identity, font_key, theme_key)`, never
  borrowed slices.

## FFI Lifetime and Thread Contract

To keep zero-copy reads safe without lock-heavy APIs:

- All render pointers/slices returned by shim are borrowed views, valid
  until the next terminal mutation.
- Terminal mutation includes: `feed`, `resize`, viewport scroll commands,
  selection/highlight mutation, `render_update`, and `free`.
- `ghostty_vt::Terminal` is single-thread-owned (UI thread). No concurrent
  or reentrant mutation while borrowed views are active.
- Rust safe wrapper enforces this with `RenderFrame<'_>` borrow semantics;
  no explicit frame lock API is required.

## Device Responses

Many TUI programs (neofetch, htop, fish, etc.) probe terminal capabilities
at startup by sending DA (Device Attributes), DSR (Device Status Report),
DECRPM, kitty keyboard query, and similar requests. If the terminal does
not respond, programs fall back to degraded mode or stall on timeouts.

The shim's `ResponseBuffer` collects response bytes during `feed()`. After
each drain cycle, the `terminal` crate reads pending response bytes and
sends them to the PTY via `PtyCommand::Write`. This keeps the response
path synchronous with emulation and avoids separate callback plumbing.

Priority for v0: `device_attributes`, `device_status`, `request_mode`
(DECRPM), `size_report`, `kitty_keyboard_query`, and `enquiry`.
Later: `xtversion`, XTGETTCAP, and other optional queries.

## Config Model

Config is split into spawn-time immutable config and runtime-reloadable
config. This follows Ghostty's `DerivedConfig` pattern where each
subsystem extracts only the fields it needs.

### SpawnConfig (immutable after session creation)

```rust
pub struct SpawnConfig {
    pub initial_cols: u16,
    pub initial_rows: u16,
    pub max_scrollback_lines: usize,
    pub shell_program: String,      // powershell.exe in v0
    pub shell_args: Vec<String>,
    pub term: String,               // xterm-256color
    pub color_term: String,         // truecolor
}
```

### RenderConfig (hot-reloadable, shared across sessions)

```rust
pub struct RenderConfig {
    pub font_family: String,
    pub font_size: f32,
    pub theme: ThemeColors,
    pub cursor_style: CursorStyle,
    pub cursor_blink: bool,
}
```

In v0, `RenderConfig` is Rust defaults only (no config file). When config
hot-reload is added, we keep behavior intentionally simple:

- Rebuild per-subsystem derived config snapshots (renderer/terminal/input).
- Broadcast updated derived config to live sessions.
- Force one full redraw after apply.

No explicit config-version protocol is required in v0. `SpawnConfig`
fields (shell, TERM, etc.) are never reloaded — they only apply to newly
spawned sessions, matching Ghostty's behavior.
