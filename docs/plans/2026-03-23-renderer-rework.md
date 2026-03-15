# Renderer Rework (2026-03-23): Text System First

## Goal
Remove the renderer's dependency on GPUI shaping/layout internals and replace it with a Windows-only DirectWrite text pipeline that follows:

- Ghostty shape and cache architecture
- Windows Terminal (WT) DirectWrite backend logic and Windows-specific optimizations

## Explicit Engineering Policy + Design Principles for this Workstream

We are willing to **completely reshape ** renderer and terminal-facing APIs / data model
from first principles for measurable performance and/or readability/maintainability wins.

- Prefer simple, explicit ownership boundaries over preserving legacy call shapes.
- Prefer simple fast paths over layered abstraction churn.
- Avoid enterprise-style heavy defensive scaffolding.
- Optimize wherever possible. Refer Ghostty and Windows Terminal frequently. If you see
  any available, note it in a message, then proceed with the rework.

This plan is intentionally split into:

1. Text-system-first cutover (do now)
2. Renderer inefficiency follow-up (after cutover)

We will still pull a few renderer fixes forward if they reduce churn during cutover:

- remove splice-based row replacement
- remove atlas hard-fail behavior
- switch touched upload paths to `Map(WRITE_DISCARD)`

## Why Current Renderer Looks GPUI-Shaped

Current renderer behavior is significantly constrained by GPUI text APIs:

1. Per-run shaping goes through GPUI `layout_line`, causing expensive shape/layout calls in the render path.
2. Glyph cache keying currently uses `RenderGlyphParams`, which includes subpixel variant, tying cache identity to absolute position.
3. Row updates depend on `Vec::splice` and row span fixups because the frontend is maintaining one mutable global instance list.
4. Background and glyph rendering are split into separate pipelines that mirror legacy structure rather than a unified stream.

Net: the largest architectural pressure is from GPUI shape/raster coupling, especially line layout and render param identity.

## Locked Decisions (Q1-Q36)

### Structure and ownership

- Q1: Create `crates/font` now. Text subsystem lives there.
- Q2: Windows-only first. No cross-platform abstraction in v1.
- Q3: Full Ghostty frontend shape/caching model.
- Q21: Font subsystem is owned by renderer thread.

### Shaping and fallback

- Q4: Use `IDWriteTextAnalyzer1` (`GetGlyphs`, `GetGlyphPlacements`) plus `IDWriteFontFallback1`.
- Q5: Shaping segmentation is independent from fg/bg color.
- Q6/Q7/Q8: Ghostty-style shaped-run cache policy and invalidation behavior.
- Q26: WT-style axis and OpenType feature plumbing on day one.
- Q33: Use WT feature/axis plumbing but keep Ghostty-style run/cache ownership model for future Ghostty config alignment.
- Q34: Use a hybrid fallback mapping strategy (Ghostty-style fallback cache + WT-style run mapping API calls).

### Glyph cache and atlases

- Q9: No absolute subpixel position in glyph cache key for grayscale path.
- Q10: Grayscale first with Ghostty-quality fractional placement; include WT gamma/contrast corrections.
- Q11: Two separate atlases from day one (grayscale + color).
- Q12: ~~Use WT `stb_rect_pack`.~~ -> Use Ghostty skyline packer (direct port of `Atlas.zig`). Simpler, no external dependency, identical packing quality. Decision updated during Task 5 implementation.
- Q13/Q27: Ghostty grow-first atlas policy. Since Ghostty has no fixed hard limit in its atlas layer, do not add routine WT-style reset logic.
- Q31: Ghostty grow-first policy, with WT-style reset only when hardware texture limit/allocation-limit is reached.

### Renderer model and draw path

- Q14: Ghostty row-owned persistent contents + per-frame flatten upload.
- Q15/Q28: Ghostty cursor lane model (cursor-first/cursor-last lanes) integrated with cutover.
- Q16/Q24: WT-style decoration rendering path on Windows.
- Q17/Q19: Unified cell stream + single `DrawIndexedInstanced`.
- Q20: Keep dirty-rect Present1 while adopting Ghostty dirty-row rebuild.
- Q18: WT background upload path (`D3D11_USAGE_DYNAMIC`, `Map(WRITE_DISCARD)`, generation-gated).
- Q25: Keep fractional glyph placement exactly like Ghostty.
- Q29: Replicate WT gamma-ratio computation/query on day one.
- Q32: Ligature overlap coloring is Ghostty-first in v1 (no WT overlap split surgery in first cut).
- Q35: Premultiplied color glyph path from day one.
- Q36: Dirty-rect full-frame fallback threshold is optional/profile-driven, not mandatory in day-one cutover.

### Reliability and rollout

- Q22: Fail-soft render loop, no panic-on-render-path failures.
- Q23: No instrumentation in v1 unless profiling demands it.

## Ghostty vs WT: Selected Best-of-Both

### Atlas packer

- Pick: Ghostty skyline packer (direct port of `Atlas.zig`)
- Reason: simpler implementation (single-file, no external dependency), well-tested by Ghostty's own test suite, identical packing quality to `stb_rect_pack` for terminal glyph workloads, and keeps our atlas code aligned with Ghostty for future maintenance
- Updated from original plan (was WT `stb_rect_pack`); decision changed during Task 5 implementation

### Atlas overflow

- Pick: Ghostty grow-first
- Behavior: grow atlas on demand and preserve existing mappings
- No routine reset during normal operation.
- If we hit hardware texture limit (or equivalent allocation-limit error), do WT-style atlas reset + lazy rerasterization recovery.
- Recovery should be handled inside renderer/font pipeline directly (epoch reset + full dirty rebuild), not deferred to callback-level error plumbing.

### Persistent rows + flatten

- Pick: Ghostty ownership model + flat upload buffer
- Note: flatten is O(n) and not D3D11-specific; Ghostty uses equivalent flatten/sync flows in other backends too

### Cursor model

- Pick: Ghostty cursor lanes for v1
- Defers WT overlap split/inversion complexity to later if needed

### Decorations

- Pick: WT procedural decoration quads and shader constants on Windows
- Reason: reduces atlas pressure and aligns with target platform optimization

## Crate and Module Boundaries

### `crates/font` (new)

Owns text shaping/raster policy and caches:

- DirectWrite shaping backend (`analyzer`, script segmentation, fallback mapping)
- run segmentation and run hash builder
- shaped-run cache (Ghostty-style fixed bucket table)
- glyph cache keys and glyph raster requests
- font feature/axis and fallback policy
- font metrics extraction for decorations/cursor metrics

Does not own D3D11 resources.

### `crates/renderer`

Owns GPU resources and drawing:

- atlas textures and upload
- instance stream build/flatten
- shader constants, pipelines, present path, dirty rect present

`renderer` consumes outputs from `font` and never calls GPUI text shaping in-frame.

## File Organization (Detailed)

```
crates/font/
  src/
    lib.rs
    types.rs
    atlas.rs               - Atlas, AtlasSet, Format, Region (Ghostty skyline packer port)
    backend/dwrite/
      mod.rs
      analyzer.rs
      fallback.rs
      metrics.rs
    shaper/
      run_iter.rs
      hash.rs
    cache/
      cache_table.rs
      shaped_run_cache.rs
      glyph_cache.rs

crates/renderer/src/gpu/
  frontend.rs          - persistent row-owned contents, dirty-row rebuild, cursor lanes
  backend_d3d11.rs     - unified draw path, GPU atlas mirrors, upload/present
  shader.hlsl          - unified shader (replaces split background/text shader path)
  thread.rs            - renderer thread orchestration (font-owned-by-render-thread)
  types.rs             - quad types/shading enums/constants
```

Delete during cutover:

- `scene.rs` splice-era path
- split background-only pipeline and shader path (`BackgroundPipeline`, old background shader flow)

## Detailed Backend Blueprint

### Unified shader (`shader.hlsl`)

- Keep one shader family for all primitives.
- Encode primitive behavior via `ShadingType` branch in pixel shader.
- Include WT-style constants:
  - `backgroundColor`
  - `backgroundCellSize`
  - `backgroundCellCount`
  - `gammaRatios[4]`
  - `enhancedContrast`
  - decoration metrics (`underlineWidth`, `doubleUnderlineWidth`, `curlyLineHalfHeight`)

### Single draw call

- Build one unified instance stream:
  - first instance: background quad (`ShadingType::Background`)
  - then all foreground row lists in draw order:
    - cursor-front lane
    - row foreground lanes
    - cursor-back lane
- Execute exactly one `DrawIndexedInstanced` in normal frame rendering.

### Remove `BackgroundPipeline` entirely

- No separate background pipeline after cutover.
- Background becomes one `QuadInstance` in the unified stream.
- This removes duplicate IA/VS/PS state setup and keeps draw ordering explicit.

### Upload path details

- Background texture uses `D3D11_USAGE_DYNAMIC`.
- Upload via `Map(D3D11_MAP_WRITE_DISCARD)` with row-pitch-safe copy.
- Skip upload when generation unchanged (WT-style gate).
- Instance buffer upload uses discard mapping in unified pipeline.

### Dirty present details

- Keep Present1 dirty rects for partial updates.
- Use row-coalescing before present.
- Keep full-frame fallback threshold as an optional guard for fragmented partial invalidations (enable only if profiling shows benefit).

## Phase 1: Text-System-First Cutover

### 1) Create `crates/font`

Add modules (initial target):

- `backend/dwrite/mod.rs`
- `backend/dwrite/analyzer.rs`
- `backend/dwrite/fallback.rs`
- `backend/dwrite/metrics.rs`
- `shaper/run_iter.rs`
- `shaper/hash.rs`
- `cache/cache_table.rs`
- `cache/shaped_run_cache.rs`
- `cache/glyph_cache.rs`
- `types.rs`

### 2) Implement DWrite analyzer shaping pipeline

Requirements:

- segment scripts/runs explicitly
- call `GetGlyphs` + `GetGlyphPlacements`
- use `IDWriteFontFallback1` mapping
- preserve cluster mapping and per-glyph offsets
- support style variants (normal/bold/italic/bold-italic)

Current codebase status note (T1 scaffold + T2 main):

- `crates/font` scaffold exists with planned modules under:
  - `src/backend/dwrite/*`
  - `src/shaper/*`
  - `src/cache/*`
  - `src/types.rs`
- `backend/dwrite/analyzer.rs` now exposes the primary shaping API:
  - `DWriteAnalyzer::new(...)`
  - `DWriteAnalyzer::shape(...) -> ShapedCells`
  - `DWriteAnalyzer::reserve_for(...)` (pre-sizing hook)
- Additional analyzer/backend APIs introduced in this slice:
  - `DWriteAnalyzer::reset_arenas(...)` (arena lifecycle hook)
  - `backend/dwrite/fallback::FontFallbackContext::map_characters(...)`
  - `backend/dwrite/metrics::extract_metrics(...)`
- Data model additions used by shaping:
  - `types::ShapedCells`, `TextRun`, `Cell`, `GlyphOffset`, `RunSpan`
  - `types::ScriptRun`, `BidiRun`
  - `types::RunOptions`, `FontAxisSpec`, `FontFeatureSpec`

Arena model note (current):

- `backend/dwrite/arena.rs` uses a Windows-first VirtualAlloc-backed VM arena.
- Memory is reserved/committed with OS page granularity (`GetSystemInfo`), and
  output/scratch use separate contiguous SoA regions.
- Growth is fallible end-to-end (no panic-only growth path), and reset modes are
  available (`retain committed`, `decommit`, `release`) for renderer lifecycle control.

Integration reminder for renderer cutover:

- When implementing Phase 1 step 7 (renderer switches to `crates/font`), wire:
  - analyzer ownership on renderer thread,
  - `reserve_for(...)` on init/resize/config changes,
  - `reset_arenas(...)` in memory-pressure/recovery and lifecycle reset paths.

### 3) Implement Ghostty-style shaped-run cache

- fixed bucket LRU table (`256 x 8`)
- key includes:
  - font variant identity
  - codepoints
  - relative clusters
  - run length
  - feature/axis fingerprint
- excludes fg/bg color
- cache invalidation strategy should follow Ghostty semantics first (font/grid/config generation changes clear affected caches; avoid ad-hoc invalidation rules)

### 4) Implement position-independent glyph cache keys

- key by font face/size/rendition + glyph id + render options
- do not include absolute position/subpixel variant for grayscale path
- keep fractional placement at draw-time

### 5) Add dual atlas support ✅

- grayscale atlas (R8) + color atlas (BGRA) in `crates/font/src/atlas.rs`
- allocator: Ghostty skyline packer (direct port of `Atlas.zig` — simpler and better performing than WT `stb_rect_pack`; no external dependency; produces identical packing behavior)
- policy: grow-first (power-of-two doubling via `AtlasSet::try_grow`)
- generation tracking: atomic `modified` + `resized` counters on each `Atlas` (renderer polls without lock)
- `AtlasSet` wraps both atlases + max_size limit from device caps
- `SharedGrid` owns `AtlasSet` directly (not behind RwLock) matching Ghostty's struct layout
- `Presentation::atlas_kind()` routes text→grayscale, emoji→color
- WT-style hard-limit recovery: `AtlasSet::reset_all()` clears both + caller clears glyph cache

### 6) Integrate WT axis/feature plumbing

- parse/apply OpenType feature tags
- parse/apply variable axes
- include defaults similar to WT handling for weight/italic/slant axes
- include values in shaping cache key fingerprint

### 7) Replace renderer text entrypoints to use `crates/font`

Remove GPUI text path from renderer-frame shaping/raster flow:

- no `TextSystem::layout_line` in row rebuild
- no GPUI-driven `RenderGlyphParams` identity in renderer glyph cache

**Renderer wiring note (required for Ghostty-style variation lifecycle):**

- renderer must build a font-config key and acquire grids through `font::shared_grid_set` (`ref_dwrite`/`deref`) instead of creating ad-hoc grids
- renderer must pass style-family + variation config into `DWriteGridConfig` so primary faces are resolved/configured once at grid creation time (no runtime axis mutation path)
- on font config change, renderer must ref new key + deref old key and reset shaper cache/row state against the new grid

**Renderer atlas cleanup notes (from Task 5):**

The following renderer-local structures are now redundant with `crates/font::atlas` and must be removed during this task:

- `backend_d3d11.rs::GlyphAtlas` — the GPUI-era atlas struct (`texture`, `srv`, `entries: HashMap<RenderGlyphParams, GlyphAtlasEntry>`, `packer: ShelfPacker`, `solid_white`)
- `backend_d3d11.rs::ShelfPacker` — replaced by Ghostty skyline packer in `font::atlas::Atlas`
- `backend_d3d11.rs::GlyphAtlasEntry` — replaced by `font::cache::glyph_cache::CachedGlyph`
- `backend_d3d11.rs::GlyphAtlas::ensure_glyph()` — the rasterize-on-miss path that calls `TextSystem`; replaced by `SharedGrid::get_or_insert_glyph` using `font::atlas` directly
- `backend_d3d11.rs::D3D11Backend::resolve_glyph_texels()` — entry point that calls `ensure_glyph`; renderer should instead read `CachedGlyph.atlas_x/atlas_y` from font cache

Replace with GPU-mirror pair:
- One `ID3D11Texture2D` + SRV for grayscale (R8), one for color (BGRA)
- Sync via `Atlas.modified`/`Atlas.resized` atomics (read without lock, take `SharedGrid` read lock for pixel data)
- Add `SHADING_TEXT_COLOR` (premultiplied BGRA passthrough) alongside existing `SHADING_TEXT_GRAYSCALE`
- Bind both atlas SRVs to pixel shader (slot 0 = grayscale, slot 1 = color)
- Remove `scene.rs` dependency on `gpui::RenderGlyphParams` and `gpui::TextSystem` for glyph identity

### 8) Migrate frontend model to row-owned persistent contents

- per-row owned foreground lists
- dedicated cursor-first and cursor-last lanes
- dirty-row rebuild only
- flatten to a contiguous instance upload buffer each frame

This also removes splice-row surgery from current scene model. End goal is to remove the
extremely inefficient ephemeral scene model entirely.

### 9) Unify draw path and keep dirty present

- one unified `QuadInstance` stream (bg/text/decorations/cursor/selection)
- one `DrawIndexedInstanced` in normal frame rendering
- explicit single unified shader path (`shader.hlsl`) replacing split text/background shader flow
- remove `BackgroundPipeline` entirely; background becomes the first `QuadInstance` (`ShadingType::Background`) in the unified stream
- keep Present1 dirty rects with row-coalescing; full-frame threshold fallback is optional and profile-driven
- D3D11 present-path micro-optimization: avoid per-frame `Vec<RECT>` allocation in `present()`. Keep a persistent backend-owned scratch vector (clear + refill each frame).

### 10) Add WT gamma/contrast

- query DWrite rendering params
- compute gamma ratios compatible with WT shader math
- wire into pixel shader constants

### 11) Bring forward required renderer fixes

In this phase (not deferred):

- remove atlas hard-fail path
- convert touched upload paths to `Map(WRITE_DISCARD)`
- remove splice-based row replacement
- remove legacy split-pipeline baggage (`BackgroundPipeline` and old background shader path)

### 12) Fail-soft policy in render loop

- shaping/raster failures skip glyph and continue
- atlas growth path is grow-first; on hardware-limit/allocation-limit failure, execute WT-style reset path in-process and continue
- avoid callback-level recovery plumbing by resolving this in renderer/font code path where the error occurs
- no panic in steady-state rendering

## Phase 2: Post-Text Follow-Up (Deferred)

After Phase 1 is stable, execute remaining renderer inefficiency work from the broader rework plan, including:

- any remaining upload-path cleanups not touched during Phase 1
- additional batching/memory layout tuning (arena/slab experiments if profile indicates)
- cursor overlap split/inversion enhancements (if visual profile requires WT-level behavior)
- optional instrumentation counters if regression diagnosis needs them

## Validation Checklist

- text visible with new `crates/font` pipeline; no GPUI shaping in render path
- shaped-run cache and glyph cache both hit in steady-state typing/idle
- grayscale and color glyph paths both render correctly
- cursor lanes render correctly for block/bar/underline
- WT-style decorations render correctly (solid/dotted/dashed/curly)
- atlas grows without crash; no hard-fail on full atlas
- dirty-row rebuild only touches changed rows
- Present1 dirty rect behavior remains correct
- resize and scroll remain smooth

## Implementation Order

1. `crates/font` skeleton + types
2. analyzer shaping + fallback mapping
3. run hash + shaped-run cache
4. glyph cache keying + dual atlas policy
5. axis/feature plumbing
6. renderer cutover to font outputs
7. row-owned contents + cursor lanes + flatten upload
8. unified stream draw path + WT decorations
9. add WT gamma/contrast constants
10. remove remaining GPUI text path and cleanup
11. verify and then execute deferred Phase 2 items

## References

Ghostty:

- `crates/ghostty-vt/zig/ghostty/src/font/SharedGrid.zig`
- `crates/ghostty-vt/zig/ghostty/src/font/Atlas.zig`
- `crates/ghostty-vt/zig/ghostty/src/font/shaper/Cache.zig`
- `crates/ghostty-vt/zig/ghostty/src/datastruct/cache_table.zig`
- `crates/ghostty-vt/zig/ghostty/src/renderer/generic.zig`
- `crates/ghostty-vt/zig/ghostty/src/renderer/cell.zig`
- `crates/ghostty-vt/zig/ghostty/src/font/face/coretext.zig`

Windows Terminal:

- `opensrc/repos/microsoft/terminal/src/renderer/atlas/AtlasEngine.cpp`
- `opensrc/repos/microsoft/terminal/src/renderer/atlas/AtlasEngine.api.cpp`
- `opensrc/repos/microsoft/terminal/src/renderer/atlas/BackendD3D.cpp`
- `opensrc/repos/microsoft/terminal/src/renderer/atlas/BackendD3D.h`
- `opensrc/repos/microsoft/terminal/src/renderer/atlas/shader_ps.hlsl`
- `opensrc/repos/microsoft/terminal/src/renderer/atlas/dwrite_helpers.hlsl`
