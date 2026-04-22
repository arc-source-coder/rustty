# External Surface Host: SwapChainPanel-Style Composition In GPUI

> Supersedes the manual `CompositionSlot::set_bounds(...)` geometry
> publication described in [13-gpu-renderer.md](13-gpu-renderer.md).
>
> This doc is implementation-facing. It is intended to be detailed enough
> that a future thread can pick it up and execute the refactor directly.

## Goal

Replace the current low-level Windows-only `CompositionSlot` escape hatch
with a first-class GPUI **external surface host** that behaves like a
`SwapChainPanel`:

- GPUI layout owns the host rectangle.
- GPUI Windows owns the DirectComposition child visual lifecycle.
- External renderers own pixels and present independently.
- Terminal output must **not** flow through GPUI's scene as textured
  pixels.

This is a **control-plane** integration, not a `PaintSurface` revival.
GPUI should participate only in layout, clipping, and composition-tree
ownership. It must stay out of the way of terminal rendering.

## Non-Goals

- No resize-generation handshake in this slice.
- No temporary stretch / scale-to-fit resize workaround.
- No arbitrary interleaving between GPUI primitives and external
  surfaces. For now, external surfaces live in one fixed composition
  plane below GPUI's main visual.
- No `PaintSurface`-style per-frame scene upload of terminal pixels.

## Source Of Truth Today

### Rustty

- `crates/renderer/src/terminal_view.rs`
- `crates/renderer/src/terminal_element.rs`
- `crates/renderer/src/gpu/terminal_renderer.rs`
- `crates/renderer/src/gpu/thread.rs`
- `crates/renderer/src/gpu/backend_d3d11.rs`

### GPUI

- `vendor/zed/crates/gpui/src/window.rs`
- `vendor/zed/crates/gpui/src/element.rs`
- `vendor/zed/crates/gpui/src/scene.rs`
- `vendor/zed/crates/gpui/src/taffy.rs`
- `vendor/zed/crates/gpui/src/platform.rs`
- `vendor/zed/crates/gpui_windows/src/events.rs`
- `vendor/zed/crates/gpui_windows/src/directx_renderer.rs`

### Windows Terminal / Microsoft Docs

- `opensrc/repos/microsoft/terminal/src/cascadia/TerminalControl/TermControl.cpp`
- `opensrc/repos/microsoft/terminal/src/renderer/atlas/AtlasEngine.r.cpp`
- `https://learn.microsoft.com/en-us/windows/uwp/gaming/directx-and-xaml-interop`
- `https://learn.microsoft.com/en-us/windows/win32/api/windows.ui.xaml.media.dxinteropnn-windows-ui-xaml-media-dxinterop-iswapchainpanelnative2`
- `https://learn.microsoft.com/en-us/windows/win32/api/windows.ui.xaml.media.dxinteropnf-windows-ui-xaml-media-dxinterop-iswapchainpanelnative2-setswapchainhandle`
- `https://learn.microsoft.com/en-us/uwp/apiwindows.ui.xaml.controls.swapchainpanel.compositionscalechanged`
- `https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_2nf-dxgi1_2-idxgifactory2-createswapchainforcomposition`

## Why The Current Design Is Wrong

Today the terminal renderer uses a raw `CompositionSlot` and relies on
app code to manually push geometry:

1. GPUI Windows resizes its main swapchain immediately on `WM_SIZE`.
2. GPUI later notifies the app that window bounds changed.
3. `TerminalElement::prepaint()` computes terminal bounds.
4. `TerminalElement::prepaint()` calls `CompositionSlot::set_bounds(...)`.
5. The terminal renderer thread receives that event and resizes its own
   swapchain later.

That creates the visible resize gap:
- GPUI's window swapchain already matches the new window size.
- The terminal child visual still has the old bounds.
- The terminal swapchain still has the old buffers.

The root cause is architectural:
> `CompositionSlot` is a low-level child-visual API. It is not a
> layout-bound host element.

GPUI already has the right layout tree. We are just not using it as the
source of truth for the external surface host.

## Current vs WT vs Proposed

### Current Rustty / GPUI `CompositionSlot`

```text
WM_SIZE
  -> GPUI main swapchain resizes immediately
  -> app later gets bounds-changed callback
  -> TerminalElement::prepaint computes bounds
  -> app sends set_bounds(...) through a side channel
  -> terminal renderer thread applies slot visual bounds/clip
  -> terminal renderer thread resizes and presents its own swapchain
```

The layout tree exists, but the external host does not participate in the
final frame contract. Geometry is published manually after layout.

### Windows Terminal / `SwapChainPanel`

```text
XAML layout tree
  -> SwapChainPanel gets final size/scale from framework layout
  -> renderer swapchain handle is attached to the panel
  -> panel SizeChanged / CompositionScaleChanged fire
  -> terminal core + renderer resize to panel-owned geometry
  -> XAML composes the panel with the rest of the UI
```

The key property is not "XAML stretches for us". WT explicitly undoes the
framework transform. The key property is:
> the hosted surface is a real layout object with framework-owned
> geometry and framework-owned lifecycle.

### Proposed GPUI External Surface Host

```text
GPUI layout tree
  -> host element participates in Taffy layout
  -> host paint publishes one placement record
  -> gpui_windows reconciles DComp child visual from that frame record
  -> GPUI presents window swapchain
  -> external renderer reacts to latest host resize event and presents
```

This copies the ownership model of `SwapChainPanel` without copying XAML
itself.

## Research Findings: What SwapChainPanel Actually Gives Us

### 1. `SwapChainPanel` Is A Real Layout Element

Microsoft's docs describe `SwapChainPanel` as a `Grid` subclass that can
appear anywhere in the XAML tree. It is not a raw compositor hook; it is
an actual layout participant.

Implication:
- size, offset, clipping, ancestor transforms, and layout updates are
  owned by the UI framework,
- not by the swapchain producer.

### 2. The Producer Is Expected To React To Layout And Scale Changes

The docs explicitly call out:
- `SizeChanged`
- `CompositionScaleChanged`

as the signals that the swapchain producer should use to resize content.

`CompositionScaleChanged` is especially important because it can be
caused by ancestor transforms or other layout-driven changes that are not
obvious from app-level resize logic.

Implication:
- host geometry and scale belong to the framework,
- renderer-owned swapchains react to host events.

### 3. Windows Terminal Does Not Rely On XAML Stretching

Windows Terminal attaches its swapchain handle with
`ISwapChainPanelNative2::SetSwapChainHandle` in `TermControl.cpp:1358-1362`

It handles panel size and scale changes via:
- `SwapChainPanel.SizeChanged` → `_SwapChainSizeChanged`
- `SwapChainPanel.CompositionScaleChanged` → `_SwapChainScaleChanged`

See:
- `TermControl.xaml:1268-1270`
- `TermControl.cpp:2423-2477`

WT also explicitly undoes XAML's transform on the composition swapchain:
- `AtlasEngine.r.cpp:421-431`

This is important:
> We should **not** design around framework stretching. WT doesn't.

### 4. SwapChainPanel Still Uses A Composition Swapchain

The docs require:
- `CreateSwapChainForComposition`
- `DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL`
- `DXGI_SCALING_STRETCH`

But the content is not supposed to rely on user-visible stretch during
resize. The swapchain producer is expected to keep the backbuffer sized
to the current panel dimensions.

Implication:
- the host owns placement,
- the renderer still owns correct backbuffer sizing.

## Windows Terminal Comparison Pass

This section records the WT-specific setup and integration choices that
are useful to copy, along with the ones that are specific to XAML and
should **not** be copied literally.

### WT Setup Path

WT's relevant control-plane looks like this:

```text
AtlasEngine
  -> creates composition swapchain + shared handle
  -> exposes handle via swapChainChangedCallback

ControlCore
  -> duplicates handle into UI process / dispatcher context
  -> raises SwapChainChanged event

TermControl
  -> receives handle event early in ctor setup
  -> calls ISwapChainPanelNative2::SetSwapChainHandle(handle)
  -> listens to SwapChainPanel SizeChanged + CompositionScaleChanged
  -> forwards size/scale changes back into core/renderer
```

Relevant code:
- `AtlasEngine.r.cpp:322-390`
- `common.h:501-529`
- `TermControl.cpp:300-352`
- `TermControl.cpp:1289-1362`
- `TermControl.cpp:2423-2477`

### WT Swapchain Choices Worth Copying

WT's `AtlasEngine::_createSwapChain()` uses:
- `DCompositionCreateSurfaceHandle`
- `CreateSwapChainForCompositionSurfaceHandle`
- `BufferCount = 3`
- `SwapEffect = FLIP_SEQUENTIAL` (unless explicitly disabling Present1)
- `AlphaMode = IGNORE` for opaque paths, `PREMULTIPLIED` otherwise
- `FRAME_LATENCY_WAITABLE_OBJECT`
- `SetMaximumFrameLatency(1)`

Relevant code:
- `AtlasEngine.r.cpp:326-379`

Useful comments from WT:
- **3 buffers** because up to 2 can be locked during screen capture or
  window moves; 3 stabilizes refresh-rate rendering.
- **Flip sequential + Present1 dirty rects** for Panel Self Refresh
  (PSR); WT notes DWM folks explicitly asked them to use this.
- **Alpha ignore** when opaque to enable more independent composition and
  lower latency.

We already mirror most of this in Rustty's terminal swapchain setup:
- `crates/renderer/src/gpu/terminal_renderer.rs:144-175`

### WT Choice We Should Not Copy Literally

WT uses `SwapChainPanel`, and the public docs for `SwapChainPanel` talk
about `DXGI_SCALING_STRETCH`. WT does **not** rely on the resulting
stretch visually.

WT explicitly counteracts the XAML transform with SetMatrixTransform in `AtlasEngine.r.cpp:421-431`

We are not building on XAML, so we should not introduce an equivalent
framework stretch just to undo it later.

### WT Layout / Initialization Lesson To Copy

WT delays terminal initialization until `LayoutUpdated`, because it wants
the final panel size after layout has settled - `TermControl.cpp:340-352`

The GPUI equivalent is not a separate layout-updated hook. GPUI already
has a layout tree and a final-frame pipeline. The correct analogue is:
- host element participates in normal GPUI layout,
- GPUI emits final host geometry in the frame scene,
- GPUI Windows reconciles child visuals from that final frame output.

## What We Can Reuse From WT

### Reuse Directly

- `DCompositionCreateSurfaceHandle` +
  `CreateSwapChainForCompositionSurfaceHandle`
- `BufferCount = 3`
- `FLIP_SEQUENTIAL`
- `FRAME_LATENCY_WAITABLE_OBJECT`
- `SetMaximumFrameLatency(1)`
- `AlphaMode = IGNORE` when the surface is opaque

### Reuse Semantically

- hosted surface is a real layout participant,
- host geometry belongs to the UI framework,
- size / scale changes are delivered from the host to the renderer,
- swapchain handle attachment is a one-time host binding concern,
- the producer owns correct backbuffer sizing.

### Do Not Reuse Literally

- XAML `LayoutUpdated`
- XAML stretch behavior
- XAML's transform compensation via `SetMatrixTransform`

Those are XAML-specific artifacts, not desired properties of the design.

## Verified Current GPUI Behavior

### GPUI Main Swapchain Resizes Immediately On `WM_SIZE`

On Windows, GPUI handles `WM_SIZE` in `gpui_windows/src/events.rs`.

`handle_size_change(...)`:
- updates the logical size,
- resizes the GPUI window renderer swapchain immediately,
- only then invokes the resize callback.

Relevant files:
- `vendor/zed/crates/gpui_windows/src/events.rs:202-222`
- `vendor/zed/crates/gpui_windows/src/directx_renderer.rs:440-484`

This means the main GPUI surface is already correct before app code gets
its resize callback.

### GPUI Already Has A Clean Layout Tree

GPUI's layout pipeline is:
1. element tree construction (`Render::render()`)
2. `request_layout(...)`
3. Taffy layout solve
4. `prepaint(...)`
5. `paint(...)`
6. `platform_window.draw(&scene)`

Relevant files:
- `vendor/zed/crates/gpui/src/element.rs`
- `vendor/zed/crates/gpui/src/taffy.rs`
- `vendor/zed/crates/gpui/src/window.rs:2251-2353`

Important verified facts:
- `Drawable::prepaint()` receives the final `Bounds<Pixels>` from
  `window.layout_bounds(layout_id)`.
- Taffy computes absolute layout bounds via `TaffyLayoutEngine`.
- `Window::draw()` produces a `Scene` and then hands it to the platform
  window in one call: `platform_window.draw(&self.rendered_frame.scene)`.

This is enough to implement a `SwapChainPanel`-like abstraction cleanly.
We do **not** need a separate native layout tree.

### `PaintSurface` Is The Wrong Model For Terminal Pixels

GPUI's existing `PaintSurface` path is a macOS scene primitive for
framework-driven image surfaces:
- `vendor/zed/crates/gpui/src/window.rs:3715-3731`
- `vendor/zed/crates/gpui/src/scene.rs:716-727`

That path is wrong for our Windows terminal renderer because it implies:
- GPUI draw/present participation for every surface update,
- scene transport of pixel content,
- GPUI owning the presentation cadence.

That is the exact overhead we want to avoid.

## FLWO Verification

We are intentionally skipping a resize-generation handshake in this
design, so it is important to document what synchronization we already
have.

### GPUI Window Swapchain

GPUI Windows configures `SetMaximumFrameLatency(1)` and obtains the frame
latency waitable object:
- `vendor/zed/crates/gpui_windows/src/directx_renderer.rs:1537-1552`

Then it waits on that object before drawing/presenting:
- `vendor/zed/crates/gpui_windows/src/directx_renderer.rs:241-255`
- `vendor/zed/crates/gpui_windows/src/directx_renderer.rs:383-438`

Present uses `Present(1, ...)`:
- `vendor/zed/crates/gpui_windows/src/directx_renderer.rs:257-267`

### Terminal Swapchain

Rustty's renderer swapchain does the same:
- `crates/renderer/src/gpu/backend_d3d11.rs:854-869`
- `crates/renderer/src/gpu/backend_d3d11.rs:409-418`
- `crates/renderer/src/gpu/backend_d3d11.rs:439-475`

The renderer thread waits before the next draw after a present:
- `crates/renderer/src/gpu/thread.rs:174-239`

### What This Guarantees

It guarantees:
- both swapchains are pacing themselves to vblank,
- both use low-latency flip-model presents,
- resize skew is minimized.

It does **not** guarantee atomic same-generation presentation across both swapchains.

That is acceptable for this refactor. The architecture must leave room
for a later handshake, but must not require one.

## Ordering And Sequencing

### Correct Ordering For The New Design

The right baseline ordering is:

```text
WM_SIZE
  -> GPUI window swapchain resizes now
  -> GPUI marks window dirty
  -> next GPUI draw runs layout / prepaint / paint
  -> scene includes final external-host geometry
  -> GPUI Windows reconciles child visual bounds / clip from scene
  -> GPUI Windows publishes latest coalesced host resize event
  -> GPUI presents resized window swapchain
  -> external renderer reacts to latest host resize and presents
```

The resize event should come from the same reconciled geometry
pass, not as an independent later step.

### Why This Is Better Than Today

Today the geometry update path has an extra hop:
- GPUI prepaint computes bounds,
- app sends bounds through a side channel,
- terminal renderer thread later applies child visual geometry.

The redesign removes that hop entirely.

In other words:
- **today** the host rect itself can lag,
- **after the redesign** the host rect is correct on the first GPUI frame
  after layout, and only the hosted content can lag.

That is a materially better artifact.

### Can This Be Better Without A Handshake?

Yes, slightly:
1. **Latest-state coalescing** for host resize events. Drag-resize should
   not queue an unbounded stream of stale sizes.
2. **Publish resize from reconciled geometry**, not from ad hoc app code.
3. **Apply clip immediately on shrink** so stale content never bleeds
   outside the host rectangle.

But without a resize-generation handshake, two independent swapchains can
still briefly show different generations during growth or recovery.

That is acceptable here.

## Design Principles

### 1. GPUI Owns Geometry, Not Pixels

GPUI should own:
- host identity,
- host bounds,
- host clip,
- host visibility,
- child visual lifecycle.

GPUI should **not** own:
- terminal rasterization,
- terminal present cadence,
- swapchain backbuffer contents.

### 2. Separate Control Plane From Data Plane

The hosted surface refactor must split responsibilities cleanly:

#### Control plane (GPUI)

- layout
- clipping
- composition tree ownership
- host lifecycle
- size / scale notifications to renderer

#### Data plane (external renderer)

- swapchain creation
- backbuffer resize
- rendering
- `Present1` / `Present`

This is how we avoid `PaintSurface` overhead on Windows.

### 2.1 Performance Characteristics Of The New Host Element

The performance cost of the new host participating in GPUI is small and
bounded because it adds only control-plane work.

Per GPUI frame where the window is already dirty, the host costs roughly:

- **one Taffy node** in `request_layout`,
- **one simple prepaint/paint participant**,
- **one placement record in the frame scene**,
- **one registry reconciliation step** in `gpui_windows`.

It does **not** add:

- per-cell terminal scene primitives,
- per-frame pixel upload to GPUI,
- texture atlas churn for terminal frames,
- GPUI-driven present cadence for terminal content.

The expected cost compared to the current manual `CompositionSlot` path is
therefore:

- slightly more work in GPUI's normal frame pipeline,
- less work in app-side geometry plumbing,
- much less visual skew.

In practice this should be negligible compared to terminal rendering,
swapchain presents, and GPU work.

The redesign keeps the main property we care about:

- GPUI main swapchain remains one DComp visual,
- terminal swapchain remains a separate DComp child visual,
- the compositor still composites both visuals independently.

So we retain:

- no terminal pixels through GPUI,
- independent renderer cadence,
- compositor composition of two visuals,
- terminal renderer autonomy.

What changes is only **who owns geometry**.

Before: app / terminal renderer side channel owned geometry.
After: GPUI layout + gpui_windows own geometry.

### 3. Scene Participation Must Be Placement-Only

The scene is allowed to carry **one small primitive per external host**
for placement data.

It must never carry:
- terminal textures,
- frame content,
- per-cell geometry,
- per-output-frame updates.

If terminal output still wakes GPUI later, that is a separate issue.
This design must not require GPUI redraws in order for terminal pixels to
change.

### Data-Plane Independence Diagram

```text
                  CONTROL PLANE                         DATA PLANE

      GPUI layout / scene / DComp host           Terminal renderer thread
      --------------------------------           ------------------------

      compute host bounds / clip                 render terminal frame
      reconcile child visual                     present terminal swapchain
      emit latest resize event                   keep own FLWO pacing

                      \                                 /
                       \                               /
                        +----- DirectComposition ------+
                               composes two visuals
```

This is the critical performance boundary. GPUI owns placement; the
terminal renderer owns pixels.

## Proposed Model

### Public GPUI Concept

Introduce one first-class persistent GPUI host type, conceptually:

```rust
pub struct ExternalSurfaceHost {
    id: ExternalSurfaceId,
    ...
}
```

This host is:
- created from GPUI / `Window`,
- retained across frames,
- the renderer-facing control-plane object for the external surface.

Separately, GPUI needs a tiny layout participant that mounts the host
into the element tree for the current frame.

That mount point does **not** need to be a major public API. It can just
be:
- a small helper element under `gpui::elements`, or
- a `host.element()` helper.

Important distinction:
- `ExternalSurfaceHost` is the long-lived object the renderer holds,
- the element/helper is an ephemeral frame-time layout participant.

The exact naming can change, but the semantics should be:
- **host** rather than **slot**,
- **layout-bound** rather than manually positioned,
- **platform-managed** rather than renderer-managed.

### Public Rustty Model

Rustty should depend on the host like this:
1. `TerminalView` creates the host once.
2. `TerminalElement` (or a replacement host element) inserts the host
    into the GPUI layout tree.
3. During prepaint, the host publishes latest size / scale to the
   terminal renderer.
4. The terminal renderer publishes its swapchain handle to the host when
    created or recreated.

The terminal renderer should no longer know about:
- `IDCompositionVisual`,
- slot recovery visuals,
- manual slot clipping,
- manual slot bounds messages.

That all moves into GPUI Windows.

## Geometry Ownership

### Two Geometry Outputs, Two Consumers

The redesign should split geometry publication by consumer:
1. **Renderer-facing size / scale**
   - produced from layout in **prepaint**,
   - includes the current **window DPI / scale factor**,
   - used to resize terminal buffers as early as possible.
2. **Compositor-facing placement / clip**
   - published as part of the final frame output,
   - used by `gpui_windows` to place and clip the child visual.

This mirrors `SwapChainPanel` semantics:
- the framework owns hosted-surface placement,
- the producer reacts to host size / scale changes.

For this slice, "scale" specifically means GPUI's current window scale
factor, i.e. the DPI-driven value returned by `Window::scale_factor()`.
That cleanly covers window DPI changes.

It does **not** attempt to reproduce the full semantic surface area of
XAML `CompositionScaleChanged` under arbitrary ancestor transforms.

### Why Compositor Placement Must Be Final-Frame Data

The current bug exists because geometry is published through an
out-of-band app path (`set_bounds(...)`) instead of through GPUI's final
frame output contract.

The redesign should make authoritative child-visual placement part of the
final per-frame frame contract.

That means:
- layout is computed by Taffy,
- final bounds are resolved by GPUI,
- paint publishes one placement record for the host,
- GPUI Windows consumes that record when composing the frame.

### Why Not Use Prepaint Placement As The Source Of Truth?

Prepaint is still necessary for hitboxes and request-time side effects,
but it should not be the platform-facing source of truth for external
surface placement.

Reason:
- the platform renderer should consume the same final frame contract as
  every other drawn thing,
- not an extra side channel that can drift from paint-time clipping or
  ordering.

Prepaint should still publish the latest renderer-facing **size / scale**
(including the current window DPI scale factor). What moves out of
prepaint is only compositor placement.

## Scene Primitive Design

Do **not** reuse macOS `PaintSurface` for this.

Add a new scene primitive specifically for externally presented hosts.
This is not pixel payload. It is a small per-frame placement record.

Conceptually:

```rust
pub struct PositionExternalSurface {
    pub id: ExternalSurfaceId,
    pub bounds: Bounds<ScaledPixels>,
    pub content_mask: ContentMask<ScaledPixels>,
}
```

Key properties:
- placement only,
- no pixel payload,
- one primitive per host per GPUI frame,
- stable ID across frames.

### Why A Placement Record Is Needed At All

When `platform_window.draw(&scene)` runs, GPUI elements are already gone.
The platform backend receives the frame output, not live element objects.

So `gpui_windows` needs some retained per-frame description of:
- which external hosts exist this frame,
- their final bounds,
- their final clip.

XAML / WinUI do not need an equivalent handoff because `SwapChainPanel`
is already a retained native layout object. GPUI's element tree is not.

### Stable Host ID

Host reconciliation must use a stable opaque ID, not draw order.

Good model:
- `ExternalSurfaceId(u64)`

Rules:
- one ID == one host lifetime,
- the same ID appearing twice in one scene is a bug,
- IDs survive across frames,
- visuals are reconciled by ID.

## Windows Backend Design

### GPUI Windows Owns The Child Visual Registry

`gpui_windows::DirectXRenderer` should maintain a registry:

```text
ExternalSurfaceId -> ExternalSurfaceVisualState
```

Each entry owns:
- current child visual,
- attached surface handle (if any),
- last effective bounds,
- last effective clip,
- visibility state.

The registry persists across frames and is independent of terminal
content updates.

### Per-Frame Reconciliation

During `platform_window.draw(&scene)`:
1. collect all `PositionExternalSurface` primitives from the scene,
2. compute their effective rectangle,
3. update or create child visuals by host ID,
4. remove or hide stale hosts,
5. commit DirectComposition updates,
6. draw/present GPUI's main swapchain.

This is the key timing change.

Today: host bounds are applied later through a separate app→renderer channel.
After: host bounds are applied during the same GPUI draw that presents the resized main swapchain.

That is the `SwapChainPanel`-like behavior we want.

### Effective Rectangle And Clip

The external host must respect the final content mask, not just raw
bounds.

For v1, clipping remains rectangular.

Compute:
```text
effective_bounds = bounds ∩ content_mask.bounds
```

If `effective_bounds` is empty: the host is hidden for that frame.

Visual application should then use:
- offset = `effective_bounds.origin`
- clip = local rect covering `effective_bounds.size`

This keeps the external host aligned with GPUI's final clipped layout.

### Z-Order Semantics

Do not promise arbitrary interleaving with GPUI primitives.

For this refactor, define one fixed external-surface plane: external
hosts are composed **below** GPUI chrome / overlays.

That matches the current terminal use case and keeps the design simple.

Do **not** introduce a general "composition bands" abstraction yet. If we
later need overlay hosts, add explicit additional planes then. Do not
imply that scene order alone can provide perfect interleaving with the
monolithic GPUI swapchain.

## External Renderer API

### Surface Handle Publication

The external renderer publishes its swapchain handle to the host when:

- created,
- recreated after device loss.

Conceptually:

```rust
host.set_surface_handle(handle)
```

This is the only compositor-facing object the external renderer should
need to provide.

### Resize / Visibility Notifications

Prefer a shared latest-state object plus a payload-free wake event, not a
FIFO channel of geometry payloads.

For this slice, use a simple mutex-backed shared state:

```rust
type SharedExternalSurfaceState = Arc<Mutex<ExternalSurfaceState>>;
```

That choice is acceptable here because:
- writes happen on resize / DPI / visibility changes,
- reads happen only when the renderer is explicitly woken,
- the lock can be held only long enough to copy the small state into a
  renderer-local value,
- the lock does not need to stay held during any render, resize, or
  text-resource rebuild work.

Conceptually:

```rust
pub struct ExternalSurfaceState {
    pub logical_size: Size<Pixels>,
    pub device_size: Size<DevicePixels>,
    pub window_scale_factor: f32,
    pub visible: bool,
}

enum ExternalSurfaceEvent {
    StateChanged,
    Dropped,
}
```

The renderer reads `ExternalSurfaceState` from host-owned shared state
after `StateChanged` fires.

Important implementation rule:
- lock once,
- copy the small state into a renderer-local value,
- unlock immediately,
- do all resize / rebuild / draw work after releasing the lock.

The renderer needs:
- logical size,
- device size,
- window DPI scale factor,
- visibility.

It does **not** need origin or clip; those remain compositor concerns
owned by `gpui_windows`.

Notably absent:
- no `IDCompositionVisual`,
- no compositor device handle,
- no manual slot recovery event,
- no manual bounds / clip API.

If GPUI's compositor state is lost and rebuilt, GPUI rebinds the current
surface handle internally. That is not renderer business anymore.

This means the current `CompositionSlotEvent` channel should not survive
in its current form. In particular:
- manual `SetBounds(...)` goes away,
- renderer-visible recovery of raw DComp objects goes away.

Some signaling still remains, but only for control-plane state such as:
- latest size / scale,
- visibility,
- surface-handle publication,
- dropped / shutdown.

The preferred shape is:
- host stores the latest `ExternalSurfaceState` in shared state,
- host wakes the renderer with `StateChanged`,
- renderer locks once, copies the current state, and unlocks immediately,
- stale intermediate changes collapse automatically.

### Resize Publication Semantics

Resize is **latest state**, not a durable FIFO event log.

That means:
- GPUI stores the latest resolved host size / scale,
- GPUI wakes the external renderer,
- the external renderer locks once, copies the current host state, and unlocks,
- stale intermediate sizes do not need to be processed one by one.

In this slice, the scale component of that state comes from
`Window::scale_factor()` and therefore tracks window DPI changes cleanly.

The same principle applies to terminal / PTY resize publication:

- GPUI layout resolves the latest terminal layout,
- `TerminalSession` forwards the latest resize state to the IO thread,
- intermediate drag-resize states are allowed to collapse.

## Why This Bypasses PaintSurface Overhead

This design deliberately keeps GPUI out of the terminal hot path.

### What GPUI Does

GPUI participates when it is already doing window work:
- layout changes,
- chrome redraws,
- resize / scale changes,
- other app UI invalidation.

In those frames, GPUI carries **one placement record per host**.

### What GPUI Does Not Do

GPUI does **not**:
- receive terminal frame pixels,
- upload terminal textures,
- build per-cell terminal scene primitives,
- present terminal content on behalf of the renderer.

### Data-Plane Persistence

The child visual registry persists in `gpui_windows::DirectXRenderer`.

That means terminal frames can continue presenting directly into the
existing hosted swapchain even when GPUI is not drawing a new scene.

This is the core performance property we want:
> layout goes through GPUI,
> terminal pixels do not.

## How This Eliminates The Current Resize Gap

### Current Timeline

```text
WM_SIZE
  -> GPUI window swapchain resizes now
  -> later app prepaint publishes slot bounds
  -> later terminal renderer resizes slot swapchain
```

### New Timeline

```text
WM_SIZE
  -> GPUI window swapchain resizes now
  -> GPUI marks window dirty
  -> next GPUI frame runs layout / prepaint
  -> prepaint computes latest host size / scale for the renderer
  -> GPUI stores latest host size / scale and wakes external renderer
  -> GPUI layout coordinator sends latest terminal resize to session / IO thread
  -> paint publishes one external-host placement record
  -> GPUI Windows updates child visual bounds / clip during draw
  -> GPUI presents resized window swapchain
  -> terminal renderer reacts to latest host resize and presents
```

This still allows brief cross-swapchain skew because we are not doing a
generation handshake. But it removes the extra late app-side geometry
publication step that causes the most obvious visible gap today.

This ordering is intentional:
- **prepaint** is the earliest point where GPUI knows the final host
  size from layout,
- the external renderer should be woken immediately from that layout
  result,
- **paint + scene reconciliation** remains the authoritative path for
  child-visual placement and clipping.

## GPUI Layout Seam: Why It Is Clean Enough

The GPUI seam is clean because the necessary building blocks already
exist.

### Layout Participation

Any element can:
- request Taffy layout,
- receive final bounds in prepaint / paint,
- maintain persistent element-local state via `with_element_state(...)`.

Relevant files:

- `vendor/zed/crates/gpui/src/element.rs`
- `vendor/zed/crates/gpui/src/window.rs:3753-3827`
- `vendor/zed/crates/gpui/src/taffy.rs:57-260`

### Frame Output Contract

All painted output already converges into a `Scene`, and then into one
platform call:
- `Window::draw()` builds `rendered_frame.scene`
- `Window::present()` calls `platform_window.draw(&scene)`

That is exactly where external-surface host reconciliation belongs.
- first-class GPUI layout participants,
- first-class GPUI frame placement records,
- first-class GPUI Windows composition objects.

## File-Level Refactor Map

### GPUI public API

Likely touch:
- `vendor/zed/crates/gpui/src/platform.rs`
- `vendor/zed/crates/gpui/src/window.rs`
- `vendor/zed/crates/gpui/src/scene.rs`
- new element helper under `vendor/zed/crates/gpui/src/elements/`

Needed additions:
- `ExternalSurfaceId`
- `ExternalSurfaceHost`
- external-surface placement primitive
- `Window::create_external_surface_host(...)`
- a way for an element/helper to publish the host placement record

### GPUI Windows backend

Likely touch:
- `vendor/zed/crates/gpui_windows/src/directx_renderer.rs`
- possibly `vendor/zed/crates/gpui_windows/src/window.rs`

Needed changes:
- replace ad hoc slot state with host registry keyed by stable ID,
- reconcile visuals from per-frame placement records,
- bind host surface handles internally,
- remove app-managed `CompositionSlot` geometry updates.

### Rustty renderer

Likely touch:
- `crates/renderer/src/gpu/terminal_renderer.rs`
- `crates/renderer/src/gpu/thread.rs`
- `crates/renderer/src/gpu/backend_d3d11.rs`
- `crates/renderer/src/terminal_view.rs`
- `crates/renderer/src/terminal_element.rs`

Needed changes:
- stop acquiring / managing raw `CompositionSlot`,
- stop sending manual bounds to the renderer thread,
- register for host resize / visibility events,
- keep swapchain-handle publication,
- remove renderer-owned slot visual / clip / recovery state.

### Terminal crate

Likely touch later, but not required for the host redesign itself:
- `crates/terminal/src/session.rs`
- `crates/terminal/src/io_thread.rs`
- `crates/terminal/src/types.rs`

Reason:
- resize-specific renderer attachment plumbing becomes less necessary,
- hot-path GPUI wakeups remain a separate follow-up.

## Suggested Implementation Sequence

1. Add the new GPUI host ID + host handle types.
2. Add the new scene primitive for external hosts.
3. Add a minimal GPUI element helper that emits that primitive.
4. Implement the Windows host registry in `gpui_windows`.
5. Switch Rustty from raw `CompositionSlot` to the new host.
6. Remove now-obsolete slot-bounds plumbing.
7. Verify resize behavior with live window drag and DPI changes.

Do **not** mix this with the hot-path wakeup redesign.

## Acceptance Criteria

The refactor is complete when all of the following are true:
1. No Rustty app code manually calls `set_bounds(...)` on a raw Windows
   composition child visual.
2. The terminal host participates in GPUI layout as a real element.
3. GPUI Windows owns child visual placement and clipping for hosted
   external surfaces.
4. Terminal pixel updates continue without going through GPUI scene
   pixel transport.
5. Window resize no longer shows the current obvious late-slot gap.
6. Device loss on either side does not require the terminal renderer to
   manipulate DirectComposition visuals directly.

## Open Risks

### No Handshake Yet

Even with FLWO on both swapchains, two independent swapchains can still
briefly drift during resize or recovery.

This is acceptable for this slice.

### Dedicated Composition Band

External surfaces will not be able to interleave arbitrarily between
individual GPUI primitives. If that is ever needed, a more complex
multi-band or multi-visual GPUI composition model will be required.

### Rectangular Clip Only

The design assumes rectangular clipping based on `content_mask.bounds`.
Do not imply support for rounded or arbitrary clip shapes in this slice.

## Final Design Summary

The correct long-term model is:

> **SwapChainPanel semantics on top of GPUI's layout tree.**

Concretely:
- GPUI layout computes the external host rectangle.
- GPUI paint publishes one placement record for that host into the frame scene.
- GPUI Windows reconciles DirectComposition child visuals from that
  placement record.
- The external renderer owns its swapchain and presents directly.
- Terminal pixels never travel through GPUI's scene.

That gives us the `SwapChainPanel` ownership split we want without
relying on XAML, stretching, or a second layout system.
