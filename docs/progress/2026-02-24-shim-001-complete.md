# shim-001: Zig Shim Build System + Core Shim — Complete

**Date:** 2026-02-24
**Status:** ✅ Done — `cargo test -p ghostty_vt` passes all tests on Windows (MSVC)

---

## What was built

A Zig shim that compiles Ghostty 1.3.x terminal internals into a static library (`ghostty_shim.lib`), linked into the `ghostty_vt` Rust crate via Cargo's `build.rs`.

### Exported C ABI

| Function | Signature |
| --- | --- |
| `ghostty_vt_terminal_new` | `(cols: u16, rows: u16) -> ?*anyopaque` |
| `ghostty_vt_terminal_free` | `(ptr: ?*anyopaque) -> void` |
| `ghostty_vt_terminal_set_callbacks` | `(ptr, userdata, bell_cb, title_cb) -> void` |
| `ghostty_vt_terminal_feed` | `(ptr, bytes, len) -> c_int` |
| `ghostty_vt_terminal_resize` | `(ptr, cols, rows) -> c_int` |

### Files

- `crates/ghostty-vt/zig/build.zig` — Zig build script with uucode + unicode table generation
- `crates/ghostty-vt/zig/build.zig.zon` — Package manifest with uucode dependency
- `crates/ghostty-vt/zig/lib.zig` — Shim: TerminalHandle, ShimHandler (wraps ReadonlyHandler), lifecycle/feed/resize/callback exports
- `crates/ghostty-vt/build.rs` — Cargo build script invoking `zig build`
- `crates/ghostty-vt/src/lib.rs` — Rust FFI declarations + integration tests

### Tests

- `test_new_free` — lifecycle smoke test
- `test_feed_ascii` — feed ASCII text through the VT stream
- `test_resize` — resize terminal dimensions
- `test_bell_callback` — feed BEL (0x07), verify callback fires

---

## Key decisions & resolutions

### Ghostty source location

Moved the Ghostty submodule to `crates/ghostty-vt/zig/ghostty/`. This sidesteps the Zig module boundary problem entirely — all Ghostty files are inside our module root, so `..` imports from `terminal/Terminal.zig` resolve naturally. (Questions 1 & 2 from the investigation doc.)

### Named module imports

Named imports (`terminal_options`, `uucode`, `unicode_tables`, `symbols_tables`) do NOT propagate automatically to transitively-imported files — they must be explicitly added to the module in `build.zig`. (Question 3.)

### uucode two-step dependency pattern

Replicated Ghostty's pattern: `b.dependency("uucode", ...)` for host build to get `tables.zig`, then `b.lazyDependency("uucode", ...)` with target + `tables_path` for the actual module. Works correctly with Zig 0.15.2. (Question 4.)

### Unicode table generation

Host executables (`props-unigen`, `symbols-unigen`) built with `use_llvm = true`, stdout captured via `addWriteFiles` → `addCopyFile(captureStdOut())`. Generated files added as anonymous imports (`unicode_tables`, `symbols_tables`). (Question 5.)

### Windows / MSVC linking

Zig's `___chkstk_ms` (stack probing) is not bundled into static libraries by default. Fixed by setting `lib.bundle_compiler_rt = true` in `build.zig`. (Question 6.)

### SIMD fallback

Not needed. With `simd = false`, the `nextSlice` path falls back to byte-by-byte `next()` at comptime, so the `ghostty_simd_decode_utf8_until_control_seq` extern symbol is never referenced.

### Stream initialization

Using `Stream.initAlloc(alloc, handler)` (not `Stream.init`) to support heap-backed OSC operations (e.g., OSC 52 clipboard).

### ShimHandler design

Wraps `ReadonlyHandler` for state mutations + intercepts side-effect actions (bell, window_title) that ReadonlyHandler no-ops on. Has a `deinit` method as required by Stream's unconditional `handler.deinit()` call.

---

## What's next

The shim provides the foundation for the terminal data model. Next steps:
- **Screen reading API** — export functions to read cell contents, cursor position, dirty state from `terminal.Terminal`
- **Rust safe wrapper** — ergonomic `Terminal` struct in Rust wrapping the raw FFI
- **PTY integration** — feed PTY output through the shim's `feed` function
