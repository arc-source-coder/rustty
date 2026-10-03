## Font System

---

Kairo: Per-codepoint fallback resolution in CodepointResolver::get_index - resolver.rs
WT: Span-based fallback mapping - _mapRegularText + _mapCharacters per contiguous span. Shapes those spans under the returned face in its row mapping logic. 

---

Kairo: `configure_dwrite` mutates existing grid: clears`inner.codepoints`, updates `metrics`, and swaps `self.dwrite`; Does not rebuild/update `resolver.collection`, `glyphs`, or atlases.
Ghostty: SharedGrid is immutable after init. Config changes -> build a new grid + swap pointers.

---

Kairo: `SharedGridSet::ref_dwrite` builds a light DWrite grid on demand. `try_ref_or_insert_with` drops the mutex before running the expensive initializer, then re-locks and discards the just-built grid if another thread won the race.
Ghostty: `SharedGridSet.ref` front-loads font discovery and collection setup and builds a fully owned collection/resolver for the grid. It has a richer collection lifecycle: deferred faces, explicit fallback metadata, config-derived descriptors, style completion, embedded fallback stack.

---

Kairo: `DWriteGridKey` includes variation axes + fallback COM pointer. But `resolve_primary_faces(...)` ignores `request.axes` entirely + resolves by family + style only (`GetFirstMatchingFont`).
Ghostty:  Variation axes are part of discovery descriptors and grid identity (`discovery.Descriptor.variations`, `Descriptor.hash`, `SharedGridSet.Key`). Primary faces are added as deferred faces with descriptor variations preserved, then `DeferredFace.load` applies `face.setVariations(...)` when materializing the face. For styled variable-font requests, `SharedGridSet.ref` also retries discovery with bold/italic traits disabled when variations are present, so variable axes can supply the style even if the font's style bits are not advertised.

---

Kairo: `SharedGrid::index_for_cell` calls has_codepoint, which goes through the DWrite-heavy collection path that repeatedly calls `GetGlyphIndices`. Presentation-sensitive checks can also trigger color-glyph probing. 
Ghostty: `indexForCell` sits on top of a collection that can answer cheaply from deferred/loaded metadata. In the hot grapheme-selection path, Ghostty is querying a collection built for this purpose.

---

Kairo: Collection is basically “per-style Vec<FaceEntry> + DWrite face dedupe map”. Eager and more DirectWrite-object-centric.
Ghostty: Collection stores loaded or deferred faces, scale adjustment, fallback metadata, aliases, and metrics. Designed to keep identity stable and defer expensive work.

---

Kairo: Our DWrite collection stores already-materialized COM faces and never has an equivalent deferred-face lifecycle. Raises memory pressure and moves more expensive face creation into fallback resolution.
Ghostty: Collection.addDeferred and getFace let Ghostty search many faces cheaply and load only when needed. Uses cheap search, with expensive load only on demand.

---

Kairo: Fallback context is just base family + base collection + system fallback. Resolver does fallback discovery one codepoint at a time. Collection dedupes faces by raw COM address, which is weaker than WT’s flow and weaker than Ghostty’s lifecycle-owned collection model.

WT: Builds a custom fallback chain from the configured font-family list using `AddMapping` and `CreateFontFallbackBuilder`. Calls MapCharacters on text spans, not per isolated codepoint (`_mapCharacters`). Uses axis-aware fallback by passing fontAxisValues into `IDWriteFontFallback1::MapCharacters` - in WT's axishandling and per-style axis defaults. IDWriteFontFallback1 supports axis-aware MapCharacters, and WT uses it. WT’s custom IDWriteFontFallbackBuilder setup in its font initialization means first configured family is primary, remaining configured families become explicit fallback priorities, system fallback comes after that. That is much closer to Ghostty’s collection/discovery model than our current “single family plus system fallback” setup in renderer text-state creation.

---

Kairo: `build_renderer_text_state` creates fresh DWrite factories/analyzers.  - `Collection::new()` recreates a shared DWrite factory every time. `face_for_index()` clones `IDWriteFontFace2` per shape call/run. COM-handle oriented compared to Ghostty’s stable entry-oriented model.
Ghostty: SharedGridSet centralizes lifetime-owned font infrastructure. Uses stable collection-owned face identity. 

---

Kairo: `FaceEntry` has no `fallback: bool`, so once a fallback face is inserted, it is treated like any other face. Combined with preferred-presentation probe + `None` / “any presentation” probe, a fallback-discovered monochrome font can satisfy an emoji codepoint earlier than Ghostty would allow. Results in lower-quality fallback choices, grayscale/emoji instability, and more color probing than necessary Add fallback-ness to `FaceEntry` and preserve Ghostty’s rule - only explicit faces get the relaxed “any presentation” fallback behavior.

Ghostty: Distinguishes explicit/user-configured faces from fallback-discovered faces. Fallback faces are stricter, so emoji/text fallback quality stays sane. Also distinguishes sprite fonts. Resolver also maintains codepoint maps and handles discovery.

---

Kairo: Fallback/grid reuse and face dedupe based on raw COM pointer identity - `DWriteFallbackKey`, `shared_grid_set.rs`, `Collection::get_or_insert_dwrite_face`, `resolver.rs`. Fallback path uses `MapCharacters(...)` + `CreateFontFace()` + cast to `IDWriteFontFace2` - this means stable pointer identity for the same logical face is not guaranteed across calls. Logically identical fallback contexts may miss `SharedGridSet` reuse and may be inserted multiple times. Each duplicate face makes `Collection::get_index` slower because it linearly scans `faces[style]`.

Ghostty: Uses stable descriptor/collection based identity + deferred-entry model.

---

Kairo: Atlas overflow handling is much harsher than Ghostty. Grows until `max_atlas_size`, then clears **both** atlases. Then clears the entire glyph cache. If only the color atlas hits the cap, all grayscale glyphs, all emoji glyphs, and every cached atlas coordinate is flushed. At the very least: Clear and invalidate only the saturated atlas. Invalidate only glyphs in that atlas kind

Ghostty: Grows the active atlas on `AtlasFull`. Text cache stability does not depend on emoji pressure.

----------------------------------------------------------------------------------------------------

## Renderer / Shader System

---

Kairo: Full-screen quad, then floor(position / cellSize) plus 4-way bounds checks in shader.hlsl
WT: Full-screen quad, simpler upper-bound check in shader_ps.hlsl

---

Kairo: Separate R8 atlas, one-channel Load, simple premul in shader.hlsl
WT: Grayscale path uses glyph alpha in shader_ps.hlsl 

---

Kairo: Binds 3 SRVs and also binds a sampler every draw in backend_d3d11.rs, even though the shader only uses Load and the sampler in shader.hlsl is unused.
WT: WT atlas path binds 2 SRVs and no main-path sampler in BackendD3D.cpp

---

Kairo: Single SV_Target + normal premultiplied alpha blend in backend_d3d11.rs
WT: Dual-source output (color + weights) and specialized blend state in shader_ps.hlsl and BackendD3D.cpp

---

Kairo: Always clears the render target, then draws a full-screen background quad in backend_d3d11.rs and backend_d3d11.rs. Pays an extra full-surface write every frame before immediately overwriting it.
WT: WT normally just draws the background quad; full clear is debug-only in BackendD3D.cpp and BackendD3D.cpp

---

Kairo: Background is a texture SRV sampled in PS from screen position.
WT: Uses background[cell] in shader_ps.hlsl. Explore efficiency.

---

Kairo: Text shader sampling uses .Load from separate grayscale/color textures in shader.hlsl
WT: Direct Texture2D texel fetch, no sampler in shader_ps.hlsl.

---

Kairo: 20-byte QuadInstance struct.
WT: WT explicitly aligns QuadInstance in BackendD3D.cpp: they keep the element compact at 20 bytes, but allocate the CPU-side instance array with 32-byte alignment because that makes memcpy much faster on some hardware.

---

Kairo: Separate build_batch. Should be folded into renderer state rebuild logic.
Ghostty: Doesn't have an equivalent “batch object” layer. It rebuilds persistent row contents and syncs the row lists to GPU in generic.zig.
