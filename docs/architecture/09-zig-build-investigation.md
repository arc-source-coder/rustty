# Zig Build System Investigation

> **Status: All questions resolved.** The shim-001 build pipeline is working end-to-end. See `docs/progress/2026-02-24-shim-001-complete.md` for details.

## Purpose

Document open questions about compiling Ghostty 1.3.x internals into a
static library from our `crates/ghostty-vt/zig/` build root. These must
be answered empirically before we can finalize the shim-001 plan.

## The Core Problem

Our `lib.zig` lives at `crates/ghostty-vt/zig/lib.zig`.
Ghostty's source lives at `vendor/ghostty/src/`.

Ghostty's terminal code has **cross-directory relative imports**:

```
terminal/Terminal.zig  →  @import("../quirks.zig")      (src/quirks.zig)
terminal/Terminal.zig  →  @import("../unicode/main.zig") (src/unicode/)
terminal/stream.zig    →  @import("../simd/main.zig")    (src/simd/)
terminal/stream.zig    →  @import("../lib/main.zig")     (src/lib/)
terminal/render.zig    →  @import("../fastmem.zig")      (src/fastmem.zig)
```

These `..` imports go from `src/terminal/` UP to `src/`. Whether Zig
0.15.x allows this depends on where the **module root boundary** is.

## Question 1: Module Root Boundary for `..` Imports

When we do:

```zig
const ghostty_mod = b.createModule(.{
    .root_source_file = b.path("../../../vendor/ghostty/src/terminal/main.zig"),
});
```

Can files within this module do `@import("../quirks.zig")`?

- If the module root is **the directory of root_source_file** (`src/terminal/`),
  then `..` escapes it → **compile error**.
- If there's no restriction on `..` for non-dependency modules → **works**.

### How to test

```zig
// test_boundary/build.zig — minimal repro
const std = @import("std");
pub fn build(b: *std.Build) void {
    const mod = b.createModule(.{
        .root_source_file = b.path("../outside/inner/root.zig"),
    });
    const lib = b.addLibrary(.{
        .name = "test",
        .root_module = b.createModule(.{
            .root_source_file = b.path("main.zig"),
        }),
        .linkage = .static,
    });
    lib.root_module.addImport("tested", mod);
    b.installArtifact(lib);
}
```

Where `outside/inner/root.zig` does `@import("../sibling.zig")`.
If this compiles, `..` imports work. If not, we need approach B.

## Question 2: Module Root at `src/` Level (Approach B)

If Q1 fails, we root the module at `vendor/ghostty/src/lib_vt.zig`.
This file is at the `src/` level, so all `..` imports from `terminal/`
resolve within `src/`.

**Problem:** `lib_vt.zig` does NOT export `ReadonlyHandler`.

```zig
// lib_vt.zig
const terminal = @import("terminal/main.zig");  // private!
// terminal/main.zig has: pub const ReadonlyHandler = ...
// But lib_vt.zig doesn't re-export it.
```

Options:

- a) Can we reach it via `@import("ghostty").Terminal` and then somehow
  access the terminal module's public types? No — `terminal` is `const`
  not `pub const` in lib_vt.zig.
- b) Add a SECOND module import for `stream_readonly.zig`? But that file
  also does relative imports within `terminal/`.
- c) Use `addWriteFiles()` to generate a wrapper at build time that sits
  at the `src/` level and re-exports what we need:
  ```zig
  // generated ghostty_shim_root.zig (placed adjacent to lib_vt.zig conceptually)
  pub const terminal = @import("terminal/main.zig");
  ```
  But can `addWriteFiles` produce a file that's "in" the ghostty src tree
  for import resolution? Probably not — it goes to a cache dir.
- d) Root at `src/lib_vt.zig` AND add `terminal/main.zig` as a separate
  module. Both modules share the same named imports (`terminal_options`,
  `uucode`, etc.) but have independent roots.

### How to test

If Q1 fails, try approach (d): two modules, both with the same imports,
one rooted at `lib_vt.zig` and one at `terminal/main.zig`. Check if the
second module's `..` imports work when it's declared as a separate module
(it shouldn't, same problem as Q1).

If that also fails, try: root BOTH at `lib_vt.zig` level by using a
generated wrapper file via `addWriteFiles`.

## Question 3: Named Module Imports Propagation

When `terminal/main.zig` is imported (whether as a module root or
transitively), it does:

```zig
pub const options = @import("terminal_options");
```

And `Terminal.zig` does:

```zig
const build_options = @import("terminal_options");
const uucode = @import("uucode");
```

Do named module imports (`terminal_options`, `uucode`, etc.) propagate to
ALL files transitively imported via relative paths from the module root?

**Expected:** Yes — in Zig, all files within a module share the module's
named imports. But verify this works when files are imported via `..`.

### How to test

Extend the Q1 test: add a named import (`addOptions`) to the module,
and have a transitively-imported file access it.

## Question 4: `uucode` Dependency Two-Step Pattern

Ghostty's build uses a two-call pattern for uucode:

```zig
// Step 1: host build, get tables.zig path
const uucode = b.dependency("uucode", .{
    .build_config_path = b.path("src/build/uucode_config.zig"),
});
const tables = uucode.namedLazyPath("tables.zig");

// Step 2: target build with tables_path
if (b.lazyDependency("uucode", .{
    .target = target,
    .tables_path = tables,
    .build_config_path = b.path("src/build/uucode_config.zig"),
})) |dep| {
    module.addImport("uucode", dep.module("uucode"));
}
```

Questions:

- Does `b.dependency()` vs `b.lazyDependency()` with DIFFERENT option
  sets create separate dependency instances? Or do they collide on the
  name `"uucode"`?
- Can we simplify to a single `b.dependency()` call?
- Does `build_config_path` work when pointing OUTSIDE our package root
  (to `vendor/ghostty/src/build/uucode_config.zig`)?

### How to test

Try the simplest possible uucode integration: single `b.dependency()`
call, add the module, see if it compiles. If that fails, try the two-step
pattern.

## Question 5: Unicode Table Generation

We need to run `props_uucode.zig` and `symbols_uucode.zig` as host
executables to generate lookup tables. These files live at
`vendor/ghostty/src/unicode/` and need `uucode` as a module import.

```zig
const props_exe = b.addExecutable(.{
    .root_source_file = ghostty_src.path(b, "unicode/props_uucode.zig"),
    .target = b.graph.host,
});
props_exe.root_module.addImport("uucode", uucode_host.module("uucode"));
```

Questions:

- Do these generators need any OTHER module imports beyond `uucode`?
- Does the `root_source_file` path resolution work when pointing outside
  our package?

### How to test

Build and run the generators standalone (without the full shim). Capture
stdout and verify it produces valid Zig source.

## Question 6: Windows / MSVC Linking

On Windows with MSVC toolchain:

- Static libs are `.lib` not `.a`
- `cargo:rustc-link-lib=c` may not work

Zig's `installArtifact` or `getEmittedBin()` should produce the right
format for the target. But we need to verify the Cargo link line works.

### How to test

After the basic pipeline works, try `cargo.exe test -p ghostty_vt` to run
on Windows (native, not WSL). This can wait until after the basic build works.

## Validation Plan

Tackle these in order. Each builds on the previous:

### V1: Minimal Zig → Rust pipeline (no Ghostty)

```
zig/lib.zig:    export fn ghostty_vt_version() callconv(.C) u32 { return 1; }
zig/build.zig:  trivial static lib
build.rs:       invoke zig build, link
src/lib.rs:     extern "C" { fn ghostty_vt_version() -> u32; }
```

Run: `cargo.exe test -p ghostty_vt`
Proves: the toolchain pipeline works end-to-end.

### V2: Module boundary test (answers Q1)

Add Ghostty terminal as a module in build.zig. In lib.zig:

```zig
const terminal = @import("ghostty");
export fn ghostty_vt_version() callconv(.C) u32 {
    _ = terminal.Terminal;
    return 1;
}
```

No uucode/unicode_tables yet — just see if the module imports compile.
This will IMMEDIATELY fail if `..` imports are blocked, OR if
`terminal_options`/`build_options`/`uucode`/`unicode_tables` are missing.

To isolate: first try with ONLY `terminal_options` + `build_options` and
see what error we get. The error message will tell us whether it's a
module boundary issue or a missing import.

### V3: uucode + unicode tables (answers Q4, Q5)

Add the full dependency chain. If V2 failed on missing `uucode`, this
step adds it. Run table generators, wire up `unicode_tables` and
`symbols_tables`.

### V4: Full shim compilation

TerminalHandle, ShimHandler, lifecycle exports. If V2+V3 pass, this
should be straightforward Zig code.

## Reference Files

- `vendor/ghostty/src/build/GhosttyZig.zig` — how Ghostty sets up the vt module
- `vendor/ghostty/src/build/UnicodeTables.zig` — unicode table generation
- `vendor/ghostty/src/build/SharedDeps.zig:874` — uucode dependency setup
- `vendor/ghostty/src/terminal/build_options.zig` — terminal_options schema
- `vendor/ghostty/src/lib_vt.zig` — Ghostty's own vt module root
- `opensrc/repos/Xuanwo/gpui-ghostty/crates/ghostty_vt_sys/zig/` — 1.2.x reference
