# shim-001: Zig Shim Build System + Core Shim

**Goal:** Compile Ghostty 1.3.x terminal internals into a static library via a Zig shim, link it into the `ghostty_vt` Rust crate, and export C ABI lifecycle/feed/resize functions.

**Architecture:** A Zig `build.zig` (invoked by Cargo's `build.rs`) compiles our `lib.zig` shim against vendored Ghostty 1.3.x internal modules. The shim wraps `terminal.Terminal` + `terminal.Stream` behind a `TerminalHandle` with a `ShimHandler` that delegates to `ReadonlyHandler` for state mutations and intercepts side-effect actions for C callbacks. The resulting `libghostty_vt.a` is linked into the Rust crate.

**Tech Stack:** Zig 0.15.2, Ghostty 1.3.x internals, Cargo build script, C ABI FFI

---

## Build System: Open Questions

The Zig build integration has multiple unknowns that must be resolved
empirically before the shim code can be finalized. These are documented
in **`docs/architecture/09-zig-build-investigation.md`** with a
step-by-step validation plan (V1→V4).

The key risk is **Zig 0.15.x module boundary behavior** — whether `..`
imports from `terminal/Terminal.zig` (e.g., `@import("../quirks.zig")`)
are allowed when the module root is at `terminal/main.zig`. Fallback
approaches are documented if they aren't.

---

## Part 1: Build Pipeline (V1 + V2 from investigation doc)

**Goal:** Get Zig → static lib → Rust linking working, then prove we can
import and use `terminal.Terminal` from vendored Ghostty 1.3.x.

**Files:**
- Create: `crates/ghostty-vt/zig/build.zig`
- Create: `crates/ghostty-vt/zig/build.zig.zon`
- Create: `crates/ghostty-vt/zig/lib.zig`
- Create: `crates/ghostty-vt/build.rs`
- Modify: `crates/ghostty-vt/Cargo.toml` (if needed)
- Modify: `crates/ghostty-vt/src/lib.rs`

This part follows the V1 → V2 → V3 validation steps in
`docs/architecture/09-zig-build-investigation.md`. The exact build.zig
structure depends on what we learn from each validation step.

### Acceptance criteria

```bash
cargo test -p ghostty_vt
```

passes with a test that calls a C ABI function which instantiates and
destroys a `terminal.Terminal` (proving the full dependency chain works:
Zig build → uucode → unicode tables → terminal module → static lib → Rust).

---

## Part 2: TerminalHandle + Lifecycle Exports

**Goal:** Define the core `TerminalHandle` struct with lifecycle C ABI: `new`, `free`. Add the scalar UTF-8 fallback export.

**Files:**
- Modify: `crates/ghostty-vt/zig/lib.zig`

**Depends on:** Part 1 (Ghostty module compiles and links)

### Step 1: Define TerminalHandle and ShimHandler types

```zig
// crates/ghostty-vt/zig/lib.zig
const std = @import("std");
const terminal = @import("ghostty");
const Allocator = std.mem.Allocator;

// --- Callback function pointer types ---
const BellCallback = *const fn (?*anyopaque) callconv(.C) void;
const TitleCallback = *const fn (?*anyopaque, [*]const u8, usize) callconv(.C) void;

const Callbacks = struct {
    userdata: ?*anyopaque = null,
    bell: ?BellCallback = null,
    title: ?TitleCallback = null,
};

// --- ShimHandler: wraps ReadonlyHandler + fires callbacks ---
const ShimHandler = struct {
    readonly: terminal.ReadonlyHandler,
    callbacks: *Callbacks,

    const Action = terminal.StreamAction;

    pub fn vt(
        self: *ShimHandler,
        comptime action: Action.Tag,
        value: Action.Value(action),
    ) !void {
        // Delegate ALL state mutation to ReadonlyHandler.
        // ReadonlyHandler no-ops on side-effect actions, so this is safe.
        try self.readonly.vt(action, value);

        // Intercept side-effect actions that ReadonlyHandler ignores.
        switch (action) {
            .bell => {
                if (self.callbacks.bell) |cb| cb(self.callbacks.userdata);
            },
            .window_title => {
                if (self.callbacks.title) |cb|
                    cb(self.callbacks.userdata, value.title.ptr, value.title.len);
            },
            else => {},
        }
    }
};

// --- TerminalHandle: owns all terminal state ---
const TerminalHandle = struct {
    alloc: Allocator,
    terminal_inst: terminal.Terminal,
    stream: terminal.Stream(*ShimHandler),
    handler: ShimHandler,
    callbacks: Callbacks,
    render_state: terminal.RenderState,

    fn init(alloc: Allocator, cols: u16, rows: u16) !*TerminalHandle {
        const handle = try alloc.create(TerminalHandle);
        errdefer alloc.destroy(handle);

        const t = try terminal.Terminal.init(alloc, .{
            .cols = cols,
            .rows = rows,
        });
        errdefer {
            var tmp = t;
            tmp.deinit(alloc);
        }

        handle.* = .{
            .alloc = alloc,
            .terminal_inst = t,
            .callbacks = .{},
            .handler = .{
                .readonly = .{ .terminal = undefined },
                .callbacks = undefined,
            },
            .stream = undefined,
            .render_state = .empty,
        };

        // Fix up self-referential pointers
        handle.handler.readonly.terminal = &handle.terminal_inst;
        handle.handler.callbacks = &handle.callbacks;
        handle.stream = terminal.Stream(*ShimHandler).init(&handle.handler);

        return handle;
    }

    fn deinit(self: *TerminalHandle) void {
        const alloc = self.alloc;
        self.render_state.deinit(alloc);
        self.stream.deinit();
        self.terminal_inst.deinit(alloc);
        alloc.destroy(self);
    }
};
```

### Step 2: Export lifecycle C ABI

```zig
// --- C ABI exports ---

export fn ghostty_vt_terminal_new(cols: u16, rows: u16) callconv(.C) ?*anyopaque {
    const alloc = std.heap.c_allocator;
    const handle = TerminalHandle.init(alloc, cols, rows) catch return null;
    return @ptrCast(handle);
}

export fn ghostty_vt_terminal_free(ptr: ?*anyopaque) callconv(.C) void {
    if (ptr == null) return;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    handle.deinit();
}
```

### Step 3: Export scalar UTF-8 fallback

Ghostty's `stream.zig` references `ghostty_simd_decode_utf8_until_control_seq` as an `extern "c"` symbol. When `simd=false`, the stream uses a scalar path, but the symbol must still be linkable. We provide it as a scalar implementation (same as gpui-ghostty).

```zig
export fn ghostty_simd_decode_utf8_until_control_seq(
    input: [*]const u8,
    count: usize,
    output: [*]u32,
    output_count: *usize,
) callconv(.C) usize {
    var i: usize = 0;
    var out_i: usize = 0;
    while (i < count) {
        if (input[i] == 0x1B) break;

        const b0 = input[i];
        var cp: u32 = 0xFFFD;
        var need: usize = 1;

        if (b0 < 0x80) {
            cp = b0;
            need = 1;
        } else if (b0 & 0xE0 == 0xC0) {
            need = 2;
            if (i + need > count) break;
            const b1 = input[i + 1];
            if (b1 & 0xC0 != 0x80) {
                cp = 0xFFFD;
                need = 1;
            } else {
                cp = ((@as(u32, b0 & 0x1F)) << 6) | (@as(u32, b1 & 0x3F));
            }
        } else if (b0 & 0xF0 == 0xE0) {
            need = 3;
            if (i + need > count) break;
            const b1 = input[i + 1];
            const b2 = input[i + 2];
            if (b1 & 0xC0 != 0x80 or b2 & 0xC0 != 0x80) {
                cp = 0xFFFD;
                need = 1;
            } else {
                cp = ((@as(u32, b0 & 0x0F)) << 12) |
                    ((@as(u32, b1 & 0x3F)) << 6) |
                    (@as(u32, b2 & 0x3F));
            }
        } else if (b0 & 0xF8 == 0xF0) {
            need = 4;
            if (i + need > count) break;
            const b1 = input[i + 1];
            const b2 = input[i + 2];
            const b3 = input[i + 3];
            if (b1 & 0xC0 != 0x80 or b2 & 0xC0 != 0x80 or b3 & 0xC0 != 0x80) {
                cp = 0xFFFD;
                need = 1;
            } else {
                cp = ((@as(u32, b0 & 0x07)) << 18) |
                    ((@as(u32, b1 & 0x3F)) << 12) |
                    ((@as(u32, b2 & 0x3F)) << 6) |
                    (@as(u32, b3 & 0x3F));
            }
        } else {
            cp = 0xFFFD;
            need = 1;
        }

        output[out_i] = cp;
        out_i += 1;
        i += need;
    }

    output_count.* = out_i;
    return i;
}
```

### Step 4: Validate

Update `src/lib.rs` with the new FFI declarations and a lifecycle smoke test:

```rust
unsafe extern "C" {
    pub fn ghostty_vt_terminal_new(cols: u16, rows: u16) -> *mut core::ffi::c_void;
    pub fn ghostty_vt_terminal_free(terminal: *mut core::ffi::c_void);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_free() {
        let ptr = unsafe { ghostty_vt_terminal_new(80, 24) };
        assert!(!ptr.is_null());
        unsafe { ghostty_vt_terminal_free(ptr) };
    }
}
```

Run: `cargo test -p ghostty_vt`

---

## Part 3: Feed, Resize, and Callbacks

**Goal:** Export `feed`, `resize`, `set_callbacks`. Complete the shim-001 scope.

**Files:**
- Modify: `crates/ghostty-vt/zig/lib.zig` (add exports)
- Modify: `crates/ghostty-vt/src/lib.rs` (add FFI declarations + tests)

**Depends on:** Part 2

### Step 1: Add `set_callbacks` export

```zig
export fn ghostty_vt_terminal_set_callbacks(
    ptr: ?*anyopaque,
    userdata: ?*anyopaque,
    bell: ?BellCallback,
    title: ?TitleCallback,
) callconv(.C) void {
    if (ptr == null) return;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    handle.callbacks = .{
        .userdata = userdata,
        .bell = bell,
        .title = title,
    };
}
```

### Step 2: Add `feed` export

```zig
export fn ghostty_vt_terminal_feed(
    ptr: ?*anyopaque,
    bytes: [*]const u8,
    len: usize,
) callconv(.C) c_int {
    if (ptr == null) return 1;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    handle.stream.nextSlice(bytes[0..len]) catch return 2;
    return 0;
}
```

Note: we use `nextSlice` (batch processing) rather than the byte-by-byte `next` loop that gpui-ghostty uses. `nextSlice` is the optimized path that handles ASCII runs efficiently even without SIMD.

### Step 3: Add `resize` export

```zig
export fn ghostty_vt_terminal_resize(
    ptr: ?*anyopaque,
    cols: u16,
    rows: u16,
) callconv(.C) c_int {
    if (ptr == null) return 1;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    handle.terminal_inst.resize(handle.alloc, cols, rows) catch return 2;
    return 0;
}
```

### Step 4: Update Rust FFI declarations

```rust
// crates/ghostty-vt/src/lib.rs

// Callback types
pub type BellCallback = unsafe extern "C" fn(userdata: *mut core::ffi::c_void);
pub type TitleCallback =
    unsafe extern "C" fn(userdata: *mut core::ffi::c_void, ptr: *const u8, len: usize);

unsafe extern "C" {
    pub fn ghostty_vt_terminal_new(cols: u16, rows: u16) -> *mut core::ffi::c_void;
    pub fn ghostty_vt_terminal_free(terminal: *mut core::ffi::c_void);

    pub fn ghostty_vt_terminal_set_callbacks(
        terminal: *mut core::ffi::c_void,
        userdata: *mut core::ffi::c_void,
        bell: Option<BellCallback>,
        title: Option<TitleCallback>,
    );

    pub fn ghostty_vt_terminal_feed(
        terminal: *mut core::ffi::c_void,
        bytes: *const u8,
        len: usize,
    ) -> core::ffi::c_int;

    pub fn ghostty_vt_terminal_resize(
        terminal: *mut core::ffi::c_void,
        cols: u16,
        rows: u16,
    ) -> core::ffi::c_int;
}
```

### Step 5: Add integration tests

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[test]
    fn test_new_free() {
        let ptr = unsafe { ghostty_vt_terminal_new(80, 24) };
        assert!(!ptr.is_null());
        unsafe { ghostty_vt_terminal_free(ptr) };
    }

    #[test]
    fn test_feed_ascii() {
        let ptr = unsafe { ghostty_vt_terminal_new(80, 24) };
        let text = b"Hello, world!";
        let rc = unsafe { ghostty_vt_terminal_feed(ptr, text.as_ptr(), text.len()) };
        assert_eq!(rc, 0);
        unsafe { ghostty_vt_terminal_free(ptr) };
    }

    #[test]
    fn test_resize() {
        let ptr = unsafe { ghostty_vt_terminal_new(80, 24) };
        let rc = unsafe { ghostty_vt_terminal_resize(ptr, 120, 40) };
        assert_eq!(rc, 0);
        unsafe { ghostty_vt_terminal_free(ptr) };
    }

    static BELL_COUNT: AtomicU32 = AtomicU32::new(0);

    unsafe extern "C" fn bell_handler(_: *mut core::ffi::c_void) {
        BELL_COUNT.fetch_add(1, Ordering::SeqCst);
    }

    #[test]
    fn test_bell_callback() {
        BELL_COUNT.store(0, Ordering::SeqCst);
        let ptr = unsafe { ghostty_vt_terminal_new(80, 24) };
        unsafe {
            ghostty_vt_terminal_set_callbacks(
                ptr,
                std::ptr::null_mut(),
                Some(bell_handler),
                None,
            );
        }
        // BEL character (0x07)
        let bel = [0x07u8];
        unsafe { ghostty_vt_terminal_feed(ptr, bel.as_ptr(), bel.len()) };
        assert_eq!(BELL_COUNT.load(Ordering::SeqCst), 1);
        unsafe { ghostty_vt_terminal_free(ptr) };
    }
}
```

### Step 6: Validate

```bash
cargo test -p ghostty_vt
```

All tests pass → shim-001 is complete.

---

## Summary of Exported C ABI (shim-001 scope)

| Function | Signature |
| --- | --- |
| `ghostty_vt_terminal_new` | `(cols: u16, rows: u16) -> ?*anyopaque` |
| `ghostty_vt_terminal_free` | `(ptr: ?*anyopaque) -> void` |
| `ghostty_vt_terminal_set_callbacks` | `(ptr, userdata, bell, title) -> void` |
| `ghostty_vt_terminal_feed` | `(ptr, bytes, len) -> c_int` |
| `ghostty_vt_terminal_resize` | `(ptr, cols, rows) -> c_int` |
| `ghostty_simd_decode_utf8_until_control_seq` | `(input, count, output, output_count) -> usize` |

---

## Unresolved Questions

1. **Module boundary for `..` imports**: Will Zig 0.15.x allow `@import("../quirks.zig")` from within a module rooted at `terminal/main.zig`? Part 2 validates this early. Fallback approaches are documented.

2. **`uucode` two-step dependency pattern**: Ghostty's build uses `b.dependency()` then `b.lazyDependency()` with different options for uucode. The plan replicates this pattern, but it may need adjustment based on Zig 0.15.x's dependency caching behavior.

3. **`nextSlice` vs `next`**: We use `nextSlice` for batch processing. If there are issues with the SIMD fallback path in `nextSlice` when `simd=false`, we can fall back to byte-by-byte `next` like gpui-ghostty does. This would be a performance regression but functionally correct.

4. **Stream `deinit`**: The gpui-ghostty calls `self.stream.deinit()` in TerminalHandle.deinit. Need to verify that `Stream.deinit` exists in 1.3.x and is required (it may free the parser's OSC allocator).

5. **ReadonlyHandler `deinit`**: ReadonlyHandler has a `deinit` method. Confirm whether it needs to be called and add to TerminalHandle.deinit if so.

6. **`window_title` value shape**: The plan accesses `value.title` (a `[]const u8` slice per the `WindowTitle` struct). Confirm `.ptr` and `.len` are accessible on Zig slices for C interop (they are — Zig slices are `{ptr, len}` pairs).
