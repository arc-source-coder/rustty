# GPU-Accelerated Terminal Renderer

> Supersedes the GPUI-based rendering pipeline described in
> [03-rendering.md](03-rendering.md) and [10-new-renderer-design.md](10-new-renderer-design.md).

## Goal

Move terminal rendering off the GPUI main thread into a dedicated
renderer thread with custom D3D11 draw calls. GPUI composites the
result as a single textured quad via its existing `PaintSurface`
scene primitive.

## Motivation

The current renderer pushes O(cells) primitives into GPUI's scene
every frame — hundreds of `paint_quad` calls for cell backgrounds,
dozens of `shape_line` calls for text runs, plus selection paths and
cursor overlays. Each primitive requires a `Vec::push`, buffer upload,
and draw call inside GPUI's `DirectXRenderer`. Even with dirty-row
caching, a full-dirty frame (80×50 grid) produces ~500–2000 scene
primitives.

This is fundamentally inefficient because GPUI's scene model is
designed for document/app UIs — sorted primitive batches with
interleaved draw order — not for rendering a dense, rapidly-updating
character grid. The overhead is in the scene machinery itself, not in
the GPU work.

A custom renderer eliminates this entirely: the renderer thread
produces a finished texture, and GPUI draws it as one primitive.

### What This Enables

- **O(1) scene cost**: one `PaintSurface` primitive per frame instead
  of O(cells).
- **Off-thread rendering**: all terminal rasterization (backgrounds,
  glyphs, cursor, selection) moves to the renderer thread. The GPUI
  main thread only blits a texture.
- **Custom glyph cache**: renderer-owned atlas on its own device
  context. No contention with GPUI's atlas.
- **True dirty-row optimization**: only re-render changed rows into
  the texture. GPUI's scene system forced full scene rebuilds because
  `paint()` runs from scratch each frame.
- **Shaped-run cache**: cache DirectWrite shaping results keyed by
  content hash, reuse across rows and frames.

## Architecture

### Threading Model

```
┌────────────────────────────────────────────────────────────────┐
│                      GPUI Main Thread                          │
│                                                                │
│  TerminalElement::paint()                                      │
│    → window.paint_surface(bounds, terminal_surface)            │
│    → 1 primitive in scene                                      │
│                                                                │
│  DirectXRenderer::draw_surfaces()                              │
│    → execute renderer's command list                           │
│    → sample shared texture as textured quad                    │
│    → present (single swap chain)                               │
└───────────────────────────┬────────────────────────────────────┘
                            │ shared device
                            │ (ID3D11Device — thread-safe)
┌───────────────────────────┴────────────────────────────────────┐
│                     Renderer Thread                            │
│                                                                │
│  Owns:                                                         │
│    - Deferred context (ID3D11DeviceContext — deferred)         │
│    - Render target texture (ID3D11Texture2D)                   │
│    - Glyph cache / atlas                                       │
│    - Shaped-run cache                                          │
│    - Mailbox (resize, focus, config changes)                   │
│                                                                │
│  Loop:                                                         │
│    1. Wait for wakeup (terminal data or mailbox message)       │
│    2. Lock terminal mutex → read grid state → unlock           │
│    3. Record draw commands into deferred context:              │
│       - Clear / background fill                                │
│       - Cell background quads (dirty rows only)                │
│       - Glyph sprites from cache (dirty rows only)             │
│       - Selection overlay                                      │
│       - Cursor                                                 │
│    4. Finalize command list                                    │
│    5. Publish command list + texture handle                    │
│    6. Request GPUI frame                                       │
└────────────────────────────────────────────────────────────────┘

┌────────────────────────────────────────────────────────────────┐
│                       Read Thread                              │
│                                                                │
│  ReadFile → lock terminal → feed() → unlock                    │
│  → wake renderer thread (mailbox / condvar)                    │
└────────────────────────────────────────────────────────────────┘
```

### Ghostty Mapping

```
Ghostty                          Rustty
────────────────────────────────────────────────────────────────
Renderer thread (Thread.zig)  →  Renderer thread (new)
  Metal.zig / OpenGL.zig      →  D3D11 deferred context
  IOSurfaceLayer / CAMetal    →  Shared texture + PaintSurface
  xev event loop              →  Mailbox + condvar
Read thread (Exec.zig)        →  Read thread (existing)
IO thread (Thread.zig)        →  IO thread (existing)
App thread (SwiftUI/AppKit)   →  GPUI main thread
```

## D3D11 Device Model

### Shared Device, Separate Contexts

The renderer thread shares GPUI's `ID3D11Device` (which is
thread-safe for resource creation) but uses its own **deferred
context** for recording draw commands.

```
ID3D11Device (shared, thread-safe)
├── Immediate context (GPUI main thread — owns this exclusively)
│   └── Executes renderer's command list
│   └── Samples texture in draw_surfaces()
│   └── Presents swap chain
└── Deferred context (renderer thread — owns this exclusively)
    └── Records all terminal draw commands
    └── Produces ID3D11CommandList
```

**Why deferred context, not a separate device:**

- Resource creation on the shared device means textures, buffers,
  and SRVs are directly usable by both contexts — no
  `SHARED_KEYEDMUTEX`, no `OpenSharedResource`, no cross-device
  handle management.
- One device = one set of pipeline state objects, one adapter, one
  memory pool. Simpler resource lifecycle and device-lost recovery.
- The deferred context records CPU-side command buffers on the
  renderer thread. `ExecuteCommandList` on the main thread submits
  them to the GPU in a single call — this is fast (handing a
  pre-built buffer to the driver, not re-doing work).

**Why not a separate device:**

A separate device provides true GPU-parallel submission, but:
- Requires `D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX` on all shared
  textures, plus explicit acquire/release synchronization.
- Two devices = two sets of pipeline state, two memory pools.
- Device-lost recovery must handle two devices.
- For terminal rendering workloads (quads + glyph sprites), GPU
  submission is not the bottleneck — CPU-side scene construction is.
  Deferred context eliminates that bottleneck.

**Upgrade path:** if profiling shows GPU submission contention
(unlikely for terminal workloads), switching to a separate device
is mechanical — the renderer thread's internals don't change, only
resource creation and synchronization.

### Device-Lost Recovery

When GPUI detects device-lost:
1. GPUI recreates its `DirectXDevices` (existing flow).
2. GPUI notifies the renderer thread via mailbox message.
3. Renderer thread recreates its deferred context, textures, glyph
   cache, and pipeline state from the new device.
4. Renderer thread sets a generation counter; GPUI discards stale
   command lists / texture references from the old generation.

## GPUI Integration

### The PaintSurface Seam

GPUI already has the infrastructure for external texture compositing:

- `Scene.surfaces: Vec<PaintSurface>` — stores surface primitives
- `Primitive::Surface(PaintSurface)` — scene primitive variant
- `BatchIterator` — interleaves surfaces with other primitives by
  draw order
- `DirectXRenderer::draw_surfaces()` — exists as a stub on Windows

This architecture completes the existing abstraction rather than
inventing a new one.

### Changes to GPUI

#### 1. Extend `PaintSurface` for Windows

Currently `PaintSurface` holds a `CVPixelBuffer` behind
`#[cfg(target_os = "macos")]`. Add Windows support:

```rust
#[derive(Clone, Debug)]
pub struct PaintSurface {
    pub order: DrawOrder,
    pub bounds: Bounds<ScaledPixels>,
    pub content_mask: ContentMask<ScaledPixels>,
    #[cfg(target_os = "macos")]
    pub image_buffer: CVPixelBuffer,
    #[cfg(target_os = "windows")]
    pub surface: WindowsSurface,
}
```

`WindowsSurface` carries everything GPUI needs to draw the terminal
texture:

```rust
/// Handle to an externally-rendered texture + its pending command list.
pub struct WindowsSurface {
    /// Command list recorded by the renderer thread's deferred context.
    /// Executed by the main thread before sampling the texture.
    pub command_list: Option<ID3D11CommandList>,
    /// The texture the command list rendered into.
    pub texture: ID3D11Texture2D,
    /// Pre-created SRV for sampling in the fragment shader.
    pub srv: ID3D11ShaderResourceView,
    /// Pixel dimensions of the texture.
    pub size: Size<DevicePixels>,
}
```

#### 2. Enable `Window::paint_surface` on Windows

Currently `#[cfg(target_os = "macos")]` only. Add a Windows variant:

```rust
#[cfg(target_os = "windows")]
pub fn paint_surface(&mut self, bounds: Bounds<Pixels>, surface: WindowsSurface) {
    self.invalidator.debug_assert_paint();
    let scale_factor = self.scale_factor();
    let bounds = bounds.scale(scale_factor);
    let content_mask = self.content_mask().scale(scale_factor);
    self.next_frame.scene.insert_primitive(PaintSurface {
        order: 0,
        bounds,
        content_mask,
        surface,
    });
}
```

#### 3. Implement `DirectXRenderer::draw_surfaces`

Replace the empty stub:

```rust
fn draw_surfaces(&mut self, surfaces: &[PaintSurface]) -> Result<()> {
    if surfaces.is_empty() {
        return Ok(());
    }
    let devices = self.devices.as_ref().context("devices missing")?;
    let resources = self.resources.as_ref().context("resources missing")?;

    for surface in surfaces {
        // Execute the renderer thread's recorded commands.
        // This submits pre-built GPU work — no CPU-heavy processing.
        if let Some(cmd_list) = &surface.surface.command_list {
            unsafe {
                devices.device_context.ExecuteCommandList(cmd_list, true);
            }
            // Restore render target after ExecuteCommandList
            // (it resets device context state).
            unsafe {
                devices.device_context.OMSetRenderTargets(
                    Some(slice::from_ref(&resources.render_target_view)),
                    None,
                );
                devices.device_context
                    .RSSetViewports(Some(slice::from_ref(&resources.viewport)));
            }
        }

        // Draw the texture as a quad using the existing sprite pipeline.
        // Reuses the polychrome sprite pipeline (or a dedicated surface
        // pipeline) with the surface's SRV.
        self.draw_surface_quad(surface)?;
    }
    Ok(())
}
```

### What TerminalElement Becomes

The current `TerminalElement` does all rendering work in `prepaint`
(grid reading, text run building, cursor shaping) and `paint`
(hundreds of `paint_quad` / `shape_line` calls). With the GPU
renderer, it becomes trivial:

```rust
impl Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = Bounds<Pixels>;

    fn request_layout(&mut self, ...) -> (LayoutId, ()) {
        // Same as today — request full-size layout.
        let mut style = Style::default();
        style.size.width = Length::Definite(DefiniteLength::Fraction(1.0));
        style.size.height = Length::Definite(DefiniteLength::Fraction(1.0));
        (window.request_layout(style, None, cx), ())
    }

    fn prepaint(&mut self, ..., bounds: Bounds<Pixels>, ...) -> Bounds<Pixels> {
        self.bounds_out.set(Some(bounds));
        let grid = compute_grid(bounds.size, self.cell_metrics);
        // Notify session of grid size for PTY resize.
        self.session.update(cx, |s, _| {
            s.request_resize(grid.cols, grid.rows, ...);
        });
        // Notify renderer thread of bounds change if needed.
        self.renderer.send_resize(bounds.size);
        bounds
    }

    fn paint(&mut self, ..., bounds: &mut Bounds<Pixels>, window: &mut Window, ...) {
        // Get the latest completed frame from the renderer thread.
        let surface = self.renderer.take_surface();
        window.paint_surface(*bounds, surface);
    }
}
```

All grid reading, text run building, glyph shaping, background rect
computation, selection path building, and cursor rendering move to
the renderer thread.

## Renderer Thread

### Lifecycle

The renderer thread is spawned by `TerminalSession` alongside the
read thread and IO thread. It is joined during shutdown after the
read thread exits.

### Communication

```
Read thread  ──wakeup──►  Renderer thread
GPUI thread  ──mailbox──►  Renderer thread
                           (resize, focus, font change, config change)
Renderer thread ──surface──► GPUI thread
                           (WindowsSurface in Arc<Mutex<Option<_>>>)
Renderer thread ──wakeup──► GPUI thread
                           (request frame via callback)
```

**Wakeup mechanism:** The read thread signals the renderer thread
after `feed()` completes, using a `Condvar` or Win32 event. The
renderer thread wakes, checks for new terminal state, and records
a new frame if dirty.

**Mailbox:** A bounded channel or ring buffer carrying:

```rust
enum RendererMessage {
    /// Terminal grid size changed. Recreate render target.
    Resize { size: Size<DevicePixels> },
    /// Focus state changed (affects cursor blink).
    FocusChanged { focused: bool },
    /// Font configuration changed. Rebuild glyph cache.
    FontChanged { family: Arc<str>, size: Pixels },
    /// Color scheme / palette changed. Rebuild palette cache.
    ColorsChanged,
    /// Device lost. Recreate all GPU resources from new device.
    DeviceLost { device: ID3D11Device },
    /// Shutdown.
    Quit,
}
```

**Surface handoff:** The renderer thread publishes completed frames
to a shared slot:

```rust
/// Shared between renderer thread (producer) and GPUI thread (consumer).
struct SurfaceSlot {
    /// The latest rendered frame. Renderer writes, GPUI reads.
    surface: Mutex<Option<WindowsSurface>>,
    /// Renderer signals after publishing a new frame.
    /// GPUI thread polls this to know when to request a frame.
    frame_ready: AtomicBool,
}
```

The renderer thread records draw commands into its deferred context,
finalizes the command list, stores it in the slot, sets `frame_ready`,
and requests a GPUI frame. The GPUI thread takes the surface during
`paint()`.

### Render Loop

```
loop {
    // 1. Wait for wakeup (terminal data or mailbox).
    wait_for_wakeup();

    // 2. Process mailbox messages (resize, font change, etc.).
    drain_mailbox();

    // 3. Lock terminal, read state.
    let frame = terminal.lock().render_frame();
    let dirty = frame.dirty();
    // Terminal mutex released here.

    if dirty == Clean && !force_redraw {
        continue;  // Nothing to render.
    }

    // 4. Record draw commands on deferred context.
    deferred_ctx.begin();  // bind render target, clear

    // Cell backgrounds — dirty rows only.
    for y in 0..rows {
        if !force_full && !frame.row_dirty(y) { continue; }
        draw_row_backgrounds(y, &frame);
    }

    // Glyph sprites — dirty rows only.
    for y in 0..rows {
        if !force_full && !frame.row_dirty(y) { continue; }
        draw_row_glyphs(y, &frame);
    }

    // Selection overlay.
    draw_selection(&frame);

    // Cursor.
    draw_cursor(&frame);

    // 5. Finalize command list.
    let cmd_list = deferred_ctx.finish_command_list();

    // 6. Publish to GPUI.
    surface_slot.publish(WindowsSurface {
        command_list: Some(cmd_list),
        texture: render_target.clone(),
        srv: render_target_srv.clone(),
        size: current_size,
    });

    // 7. Request GPUI repaint.
    request_frame();
}
```

## Text Shaping and Rasterization

### What We Reuse From GPUI

GPUI's text pipeline has two layers:

1. **`PlatformTextSystem`** (`Arc<dyn PlatformTextSystem>`) — the
   platform-specific backend. On Windows, this is
   `DirectWriteTextSystem`, which wraps `IDWriteFactory5`, font
   collections, font fallback chains, and the shaping/rasterization
   logic. This trait is **`Send + Sync`** and already stored as an
   `Arc`.

2. **`TextSystem`** — the GPUI-level wrapper that adds font
   resolution caching, fallback font stacks, and line wrapping.
   Not `Send + Sync` (uses `RwLock` internally but is tied to
   GPUI's frame lifecycle).

The `PlatformTextSystem` trait provides everything the renderer
thread needs:

```rust
pub trait PlatformTextSystem: Send + Sync {
    fn font_id(&self, descriptor: &Font) -> Result<FontId>;
    fn font_metrics(&self, font_id: FontId) -> FontMetrics;
    fn glyph_for_char(&self, font_id: FontId, ch: char) -> Option<GlyphId>;
    fn glyph_raster_bounds(&self, params: &RenderGlyphParams) -> Result<Bounds<DevicePixels>>;
    fn rasterize_glyph(
        &self, params: &RenderGlyphParams, raster_bounds: Bounds<DevicePixels>,
    ) -> Result<(Size<DevicePixels>, Vec<u8>)>;
    fn layout_line(&self, text: &str, font_size: Pixels, runs: &[FontRun]) -> LineLayout;
    // ...
}
```

- **`layout_line`**: Takes text + font runs → returns `LineLayout`
  with `Vec<ShapedRun>`, each containing `Vec<ShapedGlyph>` (glyph
  IDs + positions). Internally creates an `IDWriteTextLayout`, sets
  font collections/fallbacks/features per run, calls `Draw` with a
  custom `IDWriteTextRenderer` callback to capture shaped glyph
  output. This is the full DirectWrite shaping pipeline.

- **`rasterize_glyph`**: Takes a `RenderGlyphParams` (font ID, glyph
  ID, font size, subpixel variant, scale factor) → returns pixel
  data (CPU-side bitmap). Internally calls DirectWrite's
  `CreateGlyphRunAnalysis` + `CreateAlphaTexture`. This produces
  the actual pixels we upload to our glyph atlas.

- **`font_id`**: Resolves a `Font` descriptor (family, weight, style)
  to an opaque `FontId`. Handles system and custom font collections.

### How We Share It

Since `PlatformTextSystem` is `Send + Sync` and already `Arc`-wrapped,
sharing is straightforward:

```
GPUI Platform::text_system() → Arc<dyn PlatformTextSystem>
                                    │
                    ┌───────────────┼───────────────┐
                    │               │               │
              GPUI TextSystem   Renderer Thread   (future consumers)
              (font caching,    (shaping, raster,
               line wrapping)    glyph cache)
```

We add a method to expose the `Arc` to the renderer thread:

```rust
// In gpui or gpui_platform — a way to get the shared text system.
// The Arc<dyn PlatformTextSystem> is cloned at renderer thread spawn
// and owned by the renderer thread for its lifetime.
impl TerminalRenderer {
    pub fn new(
        text_system: Arc<dyn PlatformTextSystem>,
        device: ID3D11Device,
        // ...
    ) -> Self { ... }
}
```

**No new GPUI methods or traits.** The `Arc` is obtained from
`Platform::text_system()` at renderer thread spawn time and cloned
into the thread. All calls on the `PlatformTextSystem` are
thread-safe by trait bound.

### What Stays on the Renderer Thread

The renderer thread calls `PlatformTextSystem` methods but manages
its own caches:

| Concern | Who | Why |
|---------|-----|-----|
| Font resolution (`font_id`) | Renderer thread via `PlatformTextSystem` | Thread-safe, cached internally by DW |
| Text shaping (`layout_line`) | Renderer thread via `PlatformTextSystem` | Thread-safe, produces `LineLayout` |
| Glyph rasterization (`rasterize_glyph`) | Renderer thread via `PlatformTextSystem` | Thread-safe, produces CPU pixel data |
| Glyph atlas (GPU texture) | Renderer thread (owns exclusively) | Upload rasterized bitmaps to atlas texture |
| Shaped-run cache | Renderer thread (owns exclusively) | Content-hash-keyed, avoids re-shaping |
| Font metrics cache | Renderer thread (owns exclusively) | Avoids repeated `font_metrics` calls |

### Pipeline: Text → Pixels on Screen

```
1. Terminal grid cell → extract text + style
2. Build style run: (text, font_family, weight, style, size)
3. Check shaped-run cache:
   key = hash(text, font_family, weight, style, size)
   hit  → Vec<ShapedGlyph> (glyph IDs + positions)
   miss → call text_system.layout_line() → cache result
4. For each ShapedGlyph, check glyph atlas:
   key = (glyph_id, font_size, subpixel_variant, scale_factor)
   hit  → atlas tile (UV coordinates)
   miss → call text_system.rasterize_glyph()
          → CPU bitmap (grayscale or ClearType subpixel)
          → upload to atlas texture on GPU
          → record atlas tile
5. Record instanced draw call: glyph sprites from atlas
   → deferred context draws textured quads at glyph positions
```

Steps 1–4 are CPU work on the renderer thread. Step 5 is GPU
command recording on the deferred context. The atlas texture lives
on the shared `ID3D11Device` and is written via the deferred
context.

### Subpixel / ClearType Rendering

`rasterize_glyph` internally calls DirectWrite's
`CreateGlyphRunAnalysis` with a rendering mode. The rendering mode
is determined by `recommended_rendering_mode` (also on the trait)
or can be overridden. Since we call `rasterize_glyph` ourselves,
we control:

- **Rendering mode**: ClearType (subpixel), grayscale, or aliased.
- **Measuring mode**: natural, GDI-classic, or GDI-natural.
- **Gamma correction**: applied during rasterization.

The rasterized bitmap format differs by mode:
- **Grayscale**: 1 byte per pixel (alpha coverage).
- **ClearType**: 3 bytes per pixel (RGB subpixel coverage).

The glyph atlas stores whichever format we choose. The fragment
shader in `draw_row_glyphs` samples accordingly — a simple alpha
blend for grayscale, or per-channel blending for ClearType.

### Glyph Cache

The renderer thread maintains its own glyph atlas:

- **Atlas texture**: created on the shared `ID3D11Device`, drawn into
  via the deferred context.
- **Key**: `(glyph_id, font_size, subpixel_position, scale_factor)`.
- **Eviction**: LRU or generational. Terminal workloads have a small
  working set (typically <500 unique glyphs).
- **Separate from GPUI's atlas**: no contention, no cross-thread
  locking. GPUI's `DirectXAtlas` is never touched by the renderer
  thread.

### Shaped-Run Cache

Shaped text runs are cached keyed by content hash (matching
Ghostty's model from [03-rendering.md](03-rendering.md)):

```
run_hash = hash(utf8_text, font_family, font_weight, font_style, font_size)
shaped_run = shape_cache.get(run_hash) or shape_and_insert()
```

This cache lives entirely on the renderer thread. No cross-thread
sharing needed.

## Dirty-Row Optimization

With a persistent render target texture, true dirty-row rendering
becomes possible:

1. The render target texture persists across frames (not cleared
   each frame).
2. On partial-dirty frames, only the dirty rows' regions are
   re-rendered.
3. The deferred context records draw commands only for dirty row
   bands — clear the row band, draw backgrounds, draw glyphs.
4. Clean rows retain their pixels from the previous frame.

This is a significant improvement over the current model where
GPUI's `paint()` rebuilds the entire scene from scratch and
`pre_draw()` clears the swap chain every frame.

## Resize Flow

Resize is initiated by GPUI and flows through the system:

```
1. GPUI window resize callback fires
2. TerminalElement::prepaint() detects new bounds
3. Sends RendererMessage::Resize to renderer thread
4. Renderer thread:
   a. Releases old render target texture + SRV
   b. Creates new render target at new size
   c. Creates new SRV
   d. Sets force_full = true (next frame redraws everything)
   e. Publishes new frame
5. PTY resize flows through existing IoMsg::Resize path
```

## Performance Characteristics

### Main Thread Cost Per Frame

| Operation | Cost |
|-----------|------|
| `TerminalElement::request_layout` | 1 Taffy node — ~ns |
| `TerminalElement::prepaint` | Store bounds, check resize — ~ns |
| `TerminalElement::paint` | 1 `Vec::push` (PaintSurface) — ~ns |
| `Scene::finish` sort | 1-element surfaces vec — no-op |
| `ExecuteCommandList` | Submit pre-built command buffer — ~µs |
| `draw_surface_quad` | 1 textured quad draw call — ~µs |

**Total main-thread terminal cost: low single-digit µs.**

### Renderer Thread Cost Per Frame

| Operation | Cost |
|-----------|------|
| Terminal mutex lock + `render_frame()` | ~10–50 µs |
| Dirty-row scan + background quads | O(dirty_rows × cols) |
| Glyph cache lookups + sprite draws | O(dirty_cells) |
| Selection + cursor overlays | O(1) |
| `FinishCommandList` | ~µs |

Steady-state (cursor blink only): ~10 µs.
Full-dirty (80×50): ~100–500 µs (vs. ~1–3 ms in current pipeline).

## File Map

### New Files

| File | Purpose |
|------|---------|
| `crates/renderer/src/gpu/mod.rs` | Module root, `TerminalRenderer` public API |
| `crates/renderer/src/gpu/thread.rs` | Renderer thread loop, mailbox, wakeup |
| `crates/renderer/src/gpu/pipeline.rs` | D3D11 pipeline state (shaders, vertex layouts, buffers) |
| `crates/renderer/src/gpu/glyph_cache.rs` | Glyph atlas and shaped-run cache |
| `crates/renderer/src/gpu/surface.rs` | `WindowsSurface`, `SurfaceSlot`, handoff logic |

### Modified

| File | Change |
|------|--------|
| `vendor/zed/crates/gpui/src/scene.rs` | Add `#[cfg(target_os = "windows")]` field to `PaintSurface` |
| `vendor/zed/crates/gpui/src/window.rs` | Add `paint_surface` for Windows |
| `vendor/zed/crates/gpui/src/elements/surface.rs` | Add `WindowsSurface` variant to `SurfaceSource` |
| `vendor/zed/crates/gpui_windows/src/directx_renderer.rs` | Implement `draw_surfaces()` — execute command list + draw textured quad |
| `crates/renderer/src/terminal_element.rs` | Gut prepaint/paint — delegate to `TerminalRenderer` |
| `crates/terminal/src/session.rs` | Spawn renderer thread, wire mailbox, join on shutdown |

### Deleted (eventually)

| File | Why |
|------|-----|
| `crates/renderer/src/text_runs.rs` | Text run building moves to renderer thread |
| `crates/renderer/src/cursor.rs` | Cursor rendering moves to renderer thread |
| `crates/renderer/src/color.rs` | Color resolution moves to renderer thread |

## Risks and Mitigations

**Deferred context overhead.** D3D11 deferred contexts have known
overhead vs. immediate context for the same draw calls (~10–30%
CPU cost increase for command recording). For terminal workloads
(small number of draw calls, simple geometry), this overhead is
negligible. If profiling shows otherwise, upgrade to a separate
device (mechanical change — renderer internals don't change).

**`ExecuteCommandList` resets device state.** After executing a
command list, the immediate context's state (render targets, viewports,
blend state, etc.) is reset to defaults. `draw_surfaces()` must
restore GPUI's render target and viewport after execution. This is
already handled in the `draw_surfaces` implementation above.

**Terminal mutex contention.** The renderer thread competes with the
read thread for `Mutex<Terminal>`. This is the same contention model
as Ghostty and the current architecture — no regression. The
renderer thread holds the lock only for `render_frame()` (fast:
calls `render_update()` + returns detached `RenderFrame`), then
reads grid data lock-free via the `RenderFrame` zero-copy API.

**Glyph cache memory.** A 2048×2048 RGBA atlas is 16 MB. Terminal
workloads typically use <500 unique glyphs, fitting comfortably.
For CJK-heavy workloads, implement LRU eviction or a second atlas
page.

**GPUI frame coupling.** The terminal can only appear on screen when
GPUI presents. At display refresh rate (vsync), this is a non-issue —
both would present at the same cadence regardless.

## Future Optimizations

These are unlocked by the GPU renderer architecture but not part of
the initial implementation:

1. **Shaped-run cache (Level 2):** Content-hash-keyed cache matching
   Ghostty's `CacheTable` model. Avoids re-shaping identical text
   across rows and frames.

2. **Subpixel text rendering:** Direct control over DirectWrite's
   rendering mode and gamma correction, independent of GPUI's text
   pipeline.

3. **Background blur / transparency:** Direct control over the
   render target's alpha channel and composition mode.

4. **Sixel / image protocol support:** Render inline images directly
   into the terminal texture without going through GPUI's sprite
   atlas.

5. **Upgrade to separate device:** If GPU submission contention
   becomes measurable, switch the renderer thread to its own
   `ID3D11Device` on the same adapter. The renderer thread's
   internals are unchanged; only resource creation and the
   surface handoff mechanism change (add `SHARED_KEYEDMUTEX`).
