# GPU-Accelerated Terminal Renderer

> Supersedes the GPUI-based rendering pipeline described in
> [03-rendering.md](03-rendering.md) and [10-new-renderer-design.md](10-new-renderer-design.md).
>
> Source of truth is the current implementation:
>
> - `vendor/zed/crates/gpui_windows/src/directx_renderer.rs`
> - `crates/renderer/src/gpu/terminal_renderer.rs`
> - `crates/renderer/src/gpu/thread.rs`
> - `crates/renderer/src/terminal_element.rs`

## Goal

Move terminal rendering off the GPUI main thread into a dedicated
renderer thread with custom D3D11 draw calls. GPUI remains responsible
for the window, the DirectComposition tree, and the rest of the app UI.
Rustty owns terminal pixels through a separate composition swap chain
hosted inside a GPUI-managed `CompositionSlot`.

This is no longer a `PaintSurface` or "single swap chain, shared command
list" design. On Windows, the terminal renderer is now a separate
presenter that composes underneath GPUI chrome.

## Motivation

The old terminal path pushed O(cells) primitives into GPUI's scene every
frame: many `paint_quad` calls for backgrounds, repeated text shaping,
selection paths, and cursor overlays. That is workable for document UI,
but it is a poor fit for a dense, rapidly-updating terminal grid.

The GPU renderer changes the ownership boundary:

- GPUI owns window chrome, layout, input routing, and composition.
- Rustty owns terminal rasterization and presentation.
- The terminal becomes a dedicated composed surface instead of a large
  stream of GPUI scene primitives.

### What This Enables

- **Low GPUI scene cost**: GPUI no longer needs to emit terminal cell
  primitives on Windows.
- **Off-thread rendering**: terminal backgrounds, glyphs, selection,
  cursor, and decorations can move to the renderer thread.
- **Renderer-owned caches**: glyph atlases, shaped-run caches, and row
  state can live beside the terminal swap chain instead of in the GPUI
  paint path.
- **True dirty-row rendering**: once the renderer draws real terminal
  content, it can update only changed rows inside a persistent render
  target.
- **Clear composition ownership**: GPUI chrome can stay above the
  terminal while Rustty owns the pixels below it.

## Architecture

### Threading Model

```text
+------------------------------------------------------------------+
|                        GPUI Main Thread                          |
|                                                                  |
|  TerminalView                                                    |
|    -> owns TerminalRenderer                                      |
|    -> creates TerminalElement                                    |
|                                                                  |
|  TerminalElement::prepaint()                                     |
|    -> computes terminal bounds                                   |
|    -> publishes CompositionSlot::set_bounds(...)                 |
|                                                                  |
|  GPUI DirectComposition                                           |
|    -> owns container_visual                                       |
|    -> owns gpui_visual (window swap chain)                        |
|    -> owns slot_visual (external child visual)                    |
|                                                                  |
|  GPUI draw/present                                                |
|    -> paints chrome into gpui_visual                              |
|    -> presents window swap chain                                  |
+-----------------------------------+------------------------------+
                                    |
                                    | CompositionSlotEvent
+-----------------------------------v------------------------------+
|                     Terminal Renderer Thread                     |
|                                                                  |
|  Owned by: TerminalRenderer                                      |
|                                                                  |
|  Owns:                                                           |
|    - renderer D3D11 device                                       |
|    - renderer composition swap chain                             |
|    - current render target view                                  |
|    - slot visual binding / clip / bounds state                   |
|    - future glyph atlas / shaped-run cache / row cache           |
|                                                                  |
|  Loop today:                                                     |
|    1. Wait for slot event or renderer message                    |
|    2. Apply slot recovery / swap-chain / bounds updates          |
|    3. Resize renderer swap chain to slot bounds                  |
|    4. Clear render target yellow                                 |
|    5. Present renderer swap chain                                |
|                                                                  |
|  Loop later:                                                     |
|    -> replace yellow clear with real terminal rendering          |
+------------------------------------------------------------------+

+------------------------------------------------------------------+
|                     IO / Read Threads                            |
|                                                                  |
|  ReadFile -> terminal.feed()                                     |
|  -> wake renderer thread                                         |
|  -> keep PTY / grid resize ownership                             |
+------------------------------------------------------------------+
```

### Ghostty Mapping

```text
Ghostty                          Rustty
----------------------------------------------------------------
Renderer thread (Thread.zig)  ->  Renderer thread (current)
Platform renderer backend     ->  D3D11 + composition swap chain
Window layer / host surface   ->  DirectComposition slot visual
Read thread (Exec.zig)        ->  Read thread (existing)
IO thread                     ->  IO thread (existing)
App thread                    ->  GPUI main thread
```

### Composition Tree And Layering

The current DirectComposition tree on Windows is:

```text
IDCompositionTarget
\- container_visual
   +- slot_visual   -> Rustty terminal composition swap chain
   \- gpui_visual   -> GPUI window swap chain
```

The important layering rule is:

- `slot_visual` sits below `gpui_visual`
- GPUI must preserve transparency in the terminal region
- chrome remains visible because GPUI still paints its own opaque UI

Historically, the slot had to be placed above GPUI so the yellow swap
chain could be seen at all. That was only a workaround for GPUI still
behaving like an opaque full-window layer. The intended architecture is
the underlay model above.

## D3D11 Device Model

### Separate Devices, Separate Swap Chains

Current code uses:

| Resource | GPUI | Rustty renderer |
| --- | --- | --- |
| D3D11 device | Yes | Yes |
| Swap chain | Yes, window swap chain | Yes, composition swap chain |
| Present call | GPUI presents the window | Renderer thread presents terminal content |

This is a valid configuration and should be documented explicitly
because the previous doc described something different.

Important distinctions:

- Multiple swap chains do **not** require multiple devices.
- Multiple devices do **not** imply multiple visible swap chains.
- Rustty currently uses both because the renderer-thread seam was
  implemented for isolation first, not for shared-resource reuse.

### Why Separate Devices Today

- GPUI and Rustty can present independently without sharing immediate
  context state.
- The renderer thread owns its own D3D11 lifetime and swap-chain setup.
- DirectComposition is the composition boundary, so Rustty does not need
  to hand GPUI a shared texture every frame.
- The implementation is easier to reason about while the renderer is
  still in bring-up mode.

### Why Not Reuse One Device Yet

A shared-device design with two swap chains is possible, but it is not
an automatic performance win:

- GPUI and Rustty would still need tighter coordination around device
  context usage.
- The current separation keeps renderer-thread ownership clean.
- The main bottleneck being removed is GPUI scene construction, not
  necessarily D3D11 device creation overhead.

If profiling later shows that sharing one device materially helps, the
composition-slot architecture still allows that change.

### Device-Lost Recovery

Device-loss recovery is part of the current design:

1. GPUI recreates its DirectComposition-backed renderer state.
2. GPUI recreates `slot_visual` and sends `CompositionSlotEvent::SlotRecovered`.
3. Rustty rebinds its terminal swap chain to the recovered visual.
4. Rustty reapplies the last known bounds and clip.
5. The renderer thread presents again on the rebuilt slot.

## GPUI Integration

### The `CompositionSlot` Seam

GPUI exposes a narrow Windows-only seam:

- `Window::acquire_composition_slot(...)`
- `CompositionSlot`
- `CompositionSlotEvent`

That seam gives Rustty exactly what it needs:

- a child visual owned by GPUI's DirectComposition tree
- a way to bind a composition swap chain into it
- a way to publish bounds from GPUI layout to the renderer
- a way to recover the slot after GPUI device loss

This is the key architectural change from the old doc: terminal output
is not inserted back into the GPUI scene graph as a primitive. It is a
separately presented child visual.

### Transparent Terminal Region

The underlay design only works if GPUI does not cover the terminal
region with opaque pixels.

Two conditions are required:

| Requirement | Why |
| --- | --- |
| Window background is transparent | GPUI clears its swap chain with alpha instead of an opaque color. |
| The workspace root does not paint a full-window opaque background | The terminal region must remain a hole that reveals the slot below. |

This is the first semantic ownership split:

- GPUI owns chrome pixels.
- Rustty owns terminal pixels.
- The terminal region on the GPUI swap chain is intentionally left
  transparent.

### What `TerminalElement` Becomes

On Windows, `TerminalElement` is no longer the terminal painter. Its job
is to participate in layout and publish the terminal region to the slot.

Current responsibilities:

- measure cell metrics
- compute grid dimensions from bounds
- publish `CompositionSlot::set_bounds(...)`
- keep hit-testing / layout behavior intact for GPUI

Current non-responsibilities on Windows:

- no terminal text painting
- no terminal background painting
- no cursor painting
- no selection painting

The old GPUI-side terminal rendering code still exists in
`terminal_element.rs`, but the Windows `paint()` path is disabled. That
code is transitional scaffolding to migrate behind the renderer thread.

### TerminalRenderer API Surface

The current renderer-facing API is intentionally narrow:

```rust
pub struct TerminalRenderer {
    slot: CompositionSlot,
    thread: RendererThreadHandle,
}

impl TerminalRenderer {
    pub fn new(window: &Window) -> Result<Self>;
    pub fn sender(&self) -> Sender<RendererMessage>;
    pub fn slot(&self) -> CompositionSlot;
}
```

`TerminalView` owns `TerminalRenderer`. `TerminalSession` only keeps the
sender clone needed by the IO/read side.

For the next slice, `TerminalRenderer::new(...)` should also receive a
cloneable text-system handle so shaping and rasterization can move onto
the renderer thread without widening the GPUI seam further.

## Renderer Thread

### Lifecycle

The renderer thread is created during `TerminalView` setup:

1. acquire a `CompositionSlot` from GPUI
2. create the renderer D3D11 device
3. create the renderer composition swap chain
4. bind that swap chain into the slot
5. start the renderer thread with the slot-event receiver

Dropping `TerminalRenderer` sends `RendererMessage::Quit` and joins the
thread.

### Communication

```text
GPUI main thread  -- CompositionSlotEvent --> renderer thread
IO/read threads   -- RendererMessage      --> renderer thread
renderer thread   -- Present()            --> slot swap chain
```

Current message/event set:

| Source | Event / message | Meaning today |
| --- | --- | --- |
| GPUI | `SetSwapChain` | Bind the latest renderer swap chain to the slot visual. |
| GPUI | `SetBounds` | Update slot offset/clip and resize to match bounds. |
| GPUI | `SlotRecovered` | Reacquire the slot visual after GPUI device loss. |
| GPUI | `SlotDropped` | Drop retained DComp state. |
| IO/read | `Wake` | Ask the renderer thread to draw and present. |
| IO | `Resize` | Still sent, but currently ignored by the renderer thread. |
| View drop | `Quit` | Shut down the thread. |

### Render Loop

Current loop:

```rust
loop {
    select! {
        slot event => {
            handle slot recovery / swap chain / bounds events;
            if target exists and anything changed {
                draw_and_present();
            }
        }
        renderer message => match message {
            Wake => if target exists { draw_and_present() }
            Resize { .. } => {}
            Quit => break,
        }
    }
}
```

Current draw path:

```rust
clear render target yellow;
Present();
```

Target draw path:

```rust
update snapshot of terminal render state;
rebuild dirty rows only;
shape text runs;
rasterize/cache glyphs;
draw backgrounds, glyphs, selection, cursor, decorations;
Present();
```

## Text Shaping and Rasterization

### What We Reuse From GPUI

GPUI already owns the Windows DirectWrite stack. That should remain the
shared source of truth for font discovery, shaping, and glyph
rasterization.

The important design point is not "reuse GPUI paint code." It is:

- reuse GPUI's DirectWrite-backed text system
- move shaping and glyph cache ownership to the renderer thread
- keep terminal rendering out of the GPUI scene path

The renderer should accept a cloneable text-system handle and use it for:

- font resolution
- line shaping
- glyph raster bounds
- glyph rasterization

### What Stays On The Renderer Thread

| Concern | Owner |
| --- | --- |
| Terminal render snapshot | Renderer thread |
| Dirty-row state | Renderer thread |
| Shaped-run cache | Renderer thread |
| Glyph atlas | Renderer thread |
| Background spans / cursor / decorations | Renderer thread |
| DirectWrite platform text system handle | Shared input, consumed by renderer thread |

### Pipeline: Text To Pixels

```text
1. Update snapshot of terminal rows and style spans
2. Build styled runs per dirty row
3. Probe shaped-run cache by run hash
4. Shape cache misses through GPUI's text system
5. Probe glyph atlas for each shaped glyph
6. Rasterize atlas misses through the same text system
7. Draw row backgrounds
8. Draw glyph quads
9. Draw selection, cursor, and decorations
10. Present
```

### Glyph Cache

The renderer thread should own:

- a grayscale atlas for normal terminal text
- a color atlas for emoji / COLR glyphs
- shaped-run cache entries keyed by precomputed run identity

This mirrors the useful parts of Ghostty's design while adapting them to
the DirectComposition swap-chain model.

### Implementation Reference Note (Ghostty)

When current code or old docs are unclear, use Ghostty as the semantic
reference for cache behavior and renderer-thread policy:

- `crates/ghostty-vt/zig/ghostty/src/renderer/Thread.zig`
- `crates/ghostty-vt/zig/ghostty/src/font/shaper/Cache.zig`
- `crates/ghostty-vt/zig/ghostty/src/font/SharedGrid.zig`
- `crates/ghostty-vt/zig/ghostty/src/font/Atlas.zig`

## Dirty-Row Optimization

Dirty-row rendering is one of the main reasons to have a persistent
renderer-owned swap chain at all.

Planned model:

1. renderer keeps row-local state across frames
2. clean rows keep their previous pixels
3. dirty rows rebuild backgrounds and glyphs only for those bands
4. cursor-only frames can update with minimal work

This is the next major optimization area unlocked by the current
composition-slot architecture.

## Resize Flow

Resize now has two related but distinct paths:

### Visual Resize

This is the authoritative path for the terminal swap chain today.

```text
1. GPUI layout changes terminal bounds
2. TerminalElement::prepaint() publishes CompositionSlot::set_bounds(...)
3. GPUI sends CompositionSlotEvent::SetBounds
4. Renderer thread updates slot offset and clip
5. Renderer thread resizes its swap chain to slot bounds
6. Renderer thread presents again
```

### Grid / PTY Resize

The IO thread remains the authority for committed terminal/grid resize.
That path still exists separately from the slot-bounds resize above.

`RendererMessage::Resize` is currently transitional plumbing; it is
still emitted, but the renderer thread does not use it as the real
Windows swap-chain resize trigger anymore.

## Performance Characteristics

### GPUI Main Thread

Expected terminal-related work on the GPUI side is now small:

- terminal layout
- hit-testing
- slot-bound publication
- normal GPUI chrome rendering

The expensive terminal rasterization work should no longer happen in the
GPUI paint path on Windows.

### Renderer Thread

The renderer thread becomes the hot path for:

- terminal snapshotting
- dirty-row rebuilds
- shaping-cache probes
- atlas-cache probes
- glyph/background draw submission

That is a better match for terminal workloads than forcing the same work
through GPUI scene construction every frame.

## File Map

### Current Source Of Truth

| File | Responsibility |
| --- | --- |
| `vendor/zed/crates/gpui_windows/src/directx_renderer.rs` | GPUI DirectComposition tree, `gpui_visual`, `slot_visual`, slot recovery. |
| `crates/renderer/src/gpu/terminal_renderer.rs` | Renderer device and composition swap-chain creation, slot acquisition, thread startup. |
| `crates/renderer/src/gpu/thread.rs` | Renderer-thread event loop, slot binding, bounds/clip handling, resize, yellow present loop. |
| `crates/renderer/src/terminal_view.rs` | Owns `TerminalRenderer` and attaches the renderer sender to the session. |
| `crates/renderer/src/terminal_element.rs` | Publishes slot bounds and retains terminal layout logic during the migration. |
| `crates/terminal/src/io_thread.rs` | Continues waking the renderer and sending legacy resize messages. |

### Likely Next Additions

| Area | Likely home |
| --- | --- |
| Render-frame snapshot type | `crates/renderer/src/gpu/` |
| Glyph atlas and cache | `crates/renderer/src/gpu/` |
| Shaped-run cache | `crates/renderer/src/gpu/` |
| Renderer-side row builder | `crates/renderer/src/gpu/` |

## Risks and Mitigations

**Transparent-hole layering is still a single-GPUI-visual design.**
This is the right immediate fix, but it is not yet a full multi-layer
chrome/content split. Mitigation: keep the composition seam narrow and
move only terminal pixels out of GPUI for now.

**Separate devices may duplicate some setup cost.**
That is acceptable while the architecture is stabilizing. Mitigation:
shared-device / two-swap-chain reuse remains an option later if
profiling justifies it.

**Terminal mutex contention still matters.**
The renderer thread will contend with the read thread for terminal
state. Mitigation: snapshot render state quickly and release the lock
before shaping and rasterization.

**Legacy GPUI-side rendering code can become stale.**
Some old layout/cache structures still live in `terminal_element.rs`.
Mitigation: move that state behind the renderer thread as the real draw
pipeline lands.

**Atlas memory can grow under wide glyph sets.**
Mitigation: start with grow-only atlases, then add paging or compaction
only if real workloads demand it.

## Future Optimizations

These are unlocked by the current architecture but are not required for
the first real terminal renderer:

1. **Dirty-row render-state caching:** keep row-local background and
   glyph data so cursor-only and partial-dirty frames avoid rebuilds.

2. **Fixed-bucket shaped-run cache:** follow Ghostty's hash-addressed
   cache policy so repeated runs do not reshaped through DirectWrite.

3. **Separate grayscale and color atlases:** keep normal text fast while
   still supporting emoji and COLR glyphs correctly.

4. **Renderer-owned font-variant cache:** prebuild the common bold,
   italic, and bold-italic combinations used by terminal styling.

5. **Possible device reuse later:** if profiling shows benefit, revisit
   a single-device / two-swap-chain design without changing the
   composition-slot architecture itself.
