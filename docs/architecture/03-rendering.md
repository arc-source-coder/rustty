# Rendering Design

## Goal

Reach smooth, predictable rendering under sustained terminal output using a simple incremental pipeline.

## Chosen Strategy

Row-run rendering with dirty-row updates.

- Pull visible rows only
- Batch text by style runs
- Cache shaped runs
- Repaint only dirty visible rows and overlays

This is the best balance of readability, simplicity, and performance for v0.

## Pipeline

1. `terminal` drains PTY bytes into `ghostty_vt::Terminal`
2. `terminal` calls `render_update()` on persistent shim `RenderState`
3. `terminal` checks `render_dirty()`:
   - `.false`: skip repaint
   - `.partial`: repaint only rows where `render_row_dirty(y)` is true
   - `.full`: repaint all visible rows
4. `terminal` calls `cx.notify()` when repaint is needed
5. `renderer` pulls row text/style runs/selection/highlights for rows
   selected by dirty state
6. `renderer` reuses or rebuilds shaped runs
7. Paint order:
   - default background fill
   - non-default background spans
   - selection overlay
   - highlight overlay(s)
   - text runs
   - cursor overlay

## Dirty and Cache Model

Dirty source:

- Global dirty state from `RenderState.Dirty` (`false`/`partial`/`full`)
- Per-row dirty booleans (`RenderState.Row.dirty`, cleared by `render_clear_dirty()`)
- Viewport pin changes force `.full`

### Two-Level Caching (Mirrors Ghostty)

Ghostty uses two independent caching levels; we mirror both:

**Level 1 — Row skipping:** if `RenderState.Row.dirty == false`, the row's
cell buffer is unchanged since the last render. Skip rebuilding it entirely.
This is the dominant fast path during idle/partial-output frames.

**Level 2 — Shaped-run cache:** keyed on a **content hash of the text run**,
not on row identity. The hash covers codepoints, relative cluster offsets
within the run (position-independent), and the font index. Identical text at
different columns shares the same cache entry. This is a fixed-capacity map
(Ghostty uses a `CacheTable<u64, []ShapedCell>`).

The original placeholder `(viewport_row, row_pin_identity, font_key, theme_key)`
key is **not how Ghostty works** and is dropped. The correct model:

```
row dirty? ──no──> reuse existing cell buffer (no reshape)
           └─yes─> rebuild cell buffer from RenderState.Row.cells
                    for each style run in the row:
                      run_hash = rapidhash(utf8_text, font_family, font_weight, font_style)
                      shaped_cells = shape_cache.get(run_hash) or shape_and_insert
```

`row_pin` (the `PageList.Pin` in `RenderState.Row`) is **not safe to hold
after the next `render_update()` call** — it is used only for viewport-change
detection inside `RenderState.update()` and must never be stored in the Rust
renderer.

Full redraw triggers:

- font or DPI change
- theme/default color change
- terminal/screen global dirty flags
- viewport geometry change affecting row/col layout
- viewport pin change

## Why Not Per-Cell Draw as Primary Path

- Per-cell text shaping and drawing is easy to start but costly under burst output.
- Style-run batching lowers draw and shaping overhead without large complexity.
- Ghostty and Zed references both validate run/damage strategies.

## Scroll Handling

- Viewport position is terminal-owned (Ghostty page list viewport pin)
- Scroll actions mutate terminal viewport via shim APIs
- `render_update()` computes resulting dirty state; renderer does not keep
  a second scroll offset model
- Rotate view-local row caches on scroll where pin identity indicates stable
  row movement; otherwise rebuild dirty rows

## Synchronized Output (DEC 2026)

When a program enables synchronized output mode, the renderer skips
painting entirely — matching Ghostty's approach where `updateFrame()`
returns early when the mode is active.

The drain loop still feeds bytes to the terminal (state stays current),
but `cx.notify()` is deferred. When the mode is cleared, the next drain
cycle calls `render_update()` + `cx.notify()` and the renderer sees a
full dirty state.

Safety timer: if synchronized output stays on for >1 second, force-clear
and repaint (see `04-pty-threading.md`).

## Cursor and Selection

- Cursor derived from terminal cursor position + focus state
- Selection semantics are terminal-owned; renderer receives per-row ranges
- Keep selection rendering independent from text layout cache

## Performance Guardrails

- No full viewport text fetch on every frame
- No unbounded shaped-line cache
- No blocking calls in paint path
- Cap work per UI tick and reschedule if more dirty data remains
- Dirty tracking may have false positives, never false negatives

## Alternatives

1. Full snapshot render every notify
   - simplest mentally
   - unacceptable under high output volumes

2. GPU glyph atlas custom renderer from scratch
   - maximum possible headroom
   - too much complexity for v0, high bug surface

Chosen now: GPUI text system with disciplined batching and caching.
