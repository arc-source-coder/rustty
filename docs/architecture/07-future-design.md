# Future Design Considerations

This document records design decisions made now (in v0) to avoid painful
migrations when adding features later. Each section references how
Ghostty handles the feature and what we've done to prepare.

## Split Panes

### How Ghostty Does It

Ghostty uses an **immutable binary tree** (`SplitTree(V)` in
`src/datastruct/split_tree.zig`). Each leaf holds a `*Surface` (terminal).
Operations like split, remove, and resize return a **new tree** — enabling
undo/redo and simple memory management (arena allocator per tree).

Key design choices:

- Binary tree, not grid — arbitrary recursive splits like tmux/vim
- Generic `SplitTree(V)` decoupled from Surface/platform types
- Spatial navigation computes 2D bounding boxes from split ratios, finds
  nearest leaf by direction (up/down/left/right)
- Focus routing: `getActiveSurface()` iterates leaves for the focused one
- Zoom: `zoomed: ?Node.Handle` maximizes a single pane

### What We've Done Now

- `TabContent` enum allows `Terminal(session)` for v0, with
  `PaneTree(PaneNode)` as a future variant — no tab model migration needed
- Stable `TabId` / `SessionId` survive reordering and split operations
- Title derived from active session metadata, not stored on `TabEntry`

### When We Implement

Follow Ghostty's immutable binary tree approach. Key adaptation: our tree
operates on `SessionId` values (not raw pointers), and layout computation
lives in the `ui` crate (not in the tree data structure).

## Search

### How Ghostty Does It

Ghostty runs search on a **dedicated thread** with its own event loop.
The search thread accesses terminal pages under a mutex and produces
`FlattenedHighlight` results that are sent to the renderer via a mailbox.

Key components:

- `SlidingWindow` — circular buffer over `PageFormatter` UTF-8 output,
  case-insensitive ASCII substring search
- `PageListSearch` — reverse iteration over history pages (immutable)
- `ActiveSearch` — forward iteration over the mutable active area
- `ViewportSearch` — fingerprint-based skip when viewport hasn't changed
- Results stored as `RenderState.Highlight` with `search_match` /
  `search_match_selected` tags
- `Terminal.flags.search_viewport_dirty` bridges renderer → search thread

### What We've Done Now

- `ghostty_vt_terminal_set_highlights` / `clear_highlights` stub exports are
  in the shim ABI from v0 (see `06-ghostty-shim.md` § Highlights). The Rust
  side can call them as soon as search is wired with no shim rebuild.
- Viewport position is terminal-owned (Ghostty page list viewport pin), so
  search can map results against canonical terminal coordinates.

### When We Implement

Follow Ghostty's approach: dedicated search thread, `SlidingWindow` over
page text, results as highlights in `RenderState`. The shim will need:

- `ghostty_vt_terminal_set_highlights(handle, tag, ranges_abs, count)` and
  `ghostty_vt_terminal_clear_highlights(handle, tag)`
- Access to `PageFormatter` for text extraction (may need a shim export
  for scrollback line text)

Coordinate rule: search ranges are absolute terminal coordinates
(`x: u16, y: u32`), not viewport-relative offsets.

Search state should live on `TerminalSession` (persists across tab
switches), not on the renderer.

## Tab Reordering / Drag-and-Drop

### How Ghostty Does It

Ghostty delegates tab DnD entirely to **libadwaita's `AdwTabView`** — no
custom logic. Tab identity is index-based (no stable IDs). Moving a tab
to a new window uses `tabViewCreateWindow` which creates a new `Window`
and returns its `tab_view`.

### What We've Done Now

- Stable `TabId` / `SessionId` backed by a module-level `AtomicU64` counter
  (see `02-data-model.md` § Stable Identity). Reordering is just
  `Vec::remove` + `Vec::insert` on the `tabs: Vec<TabId>`.
- `tab_entries: HashMap<TabId, TabEntry>` decouples data from order.

### When We Implement

We'll build custom DnD in GPUI (no libadwaita). The stable ID model means
reorder is `Vec::remove` + `Vec::insert`. Moving tabs between windows is
`remove from source Workspace + insert into target Workspace` — the
`Entity<TerminalSession>` handle transfers cleanly.

## Kitty Graphics Protocol

### How Ghostty Does It

Ghostty has a comprehensive kitty graphics pipeline:

- **Parsing**: APC handler (`apc.zig`) detects kitty graphics, creates
  `kitty_gfx.CommandParser` which parses control pairs + base64 payload
- **Execution**: `kitty/graphics_exec.zig` dispatches commands (query,
  transmit, display, delete, animate)
- **Storage**: `ImageStorage` lives on **each `Screen`** (primary + alt).
  Images stored by ID, placements stored by `(image_id, placement_id)`.
  LRU eviction at 320MB total. `dirty: bool` signals renderer.
- **Rendering**: `renderer/image.zig` maintains a GPU-side state machine
  per image (`pending → ready(Texture) → unload`). Placements sorted by
  z-index into three layers: below-bg, below-text, above-text.

**`ReadonlyHandler` does NOT handle images** — APC events are no-ops.

### What We've Done Now

- Acknowledged that images require a new shim + renderer layer
- Renderer architecture (row-run + overlays) is extensible — image
  placements can be rendered as additional overlay layers

### When We Implement

This is a significant feature requiring:

1. **Shim changes**: Implement APC handler in our `ShimHandler` that
   delegates to `Terminal.kittyGraphics()`. Export image data and
   placement info via C ABI.
2. **Renderer changes**: Add an image layer with GPU texture management.
   Three z-layers (below-bg, below-text, above-text) matching Ghostty.
3. **Storage**: Ghostty's `ImageStorage` on `Screen` handles this
   internally — we just need to extract placement + pixel data through
   the shim.

Note: Ghostty does **not** support sixel. Only kitty graphics protocol.

## Config Hot-Reload

### How Ghostty Does It

Ghostty uses a **single monolithic `Config`** struct but splits it at
consumption time into per-subsystem `DerivedConfig` structs. Each
subsystem (Surface, Renderer, Termio, Font) extracts only the fields it
needs.

Reload triggers: keybind action, SIGUSR2, theme change (soft reload).
No file watcher — reload is explicit.

Key split:

- **Spawn-time only** (never reloaded): `command`, `working-directory`,
  `language`
- **Hot-reloadable**: colors, font, opacity, keybinds, cursor style, etc.
- **Soft reload**: re-evaluates conditionals (dark/light theme) without
  re-reading config files

On reload, changes fan out to all surfaces, which rebuild their
`DerivedConfig` and trigger redraws.

### What We've Done Now

- Config split into `SpawnConfig` (immutable) and `RenderConfig`
  (hot-reloadable), matching Ghostty's spawn-time vs runtime distinction.
- `RenderConfig` sharing mechanism is specified: a GPUI `Model<RenderConfig>`
  held by each session (see `02-data-model.md` § RenderConfig Sharing).
  Reload updates the model and triggers `cx.notify()` on affected sessions.

### When We Implement

- Config change rebuilds per-subsystem derived config snapshots and
  triggers one full redraw.
- `SpawnConfig` is never modified after session creation.

## Multiple Windows

### How Ghostty Does It

Ghostty's core `App` owns a flat list of all `Surface`s. Windowing is
purely the apprt's concern — each `Window` owns an `adw.TabView`.

Window close behavior: when all tabs close, the window closes itself.
At the application level, `quit-after-last-window-closed` (configurable,
default true on Linux, false on macOS) controls whether the app exits.

Tabs can be dragged between windows — GTK's `tabViewCreateWindow` creates
a new window and transfers the tab.

### What We've Done Now

- Each window owns its own `Workspace` (tab list + active tab)
- Window closes itself when `tabs.is_empty()` — no global coordination
- Stable IDs make cross-window tab transfer straightforward

### When We Implement

- v0 is single-window. The `Workspace` model already supports multiple
  windows — just create multiple GPUI windows, each with its own
  `Workspace`.
- "New Window" action creates a new GPUI window + `Workspace`.
- GPUI handles app exit when all windows close (platform default).
