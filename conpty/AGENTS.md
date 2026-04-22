# AGENTS.md

## Zig Development

Use `zigdoc` to discover current APIs for the Zig standard library and any third-party dependencies before coding.

Examples:
```bash
zigdoc std.fs
zigdoc std.posix.getuid
zigdoc vaxis.Window
```

After zig work, run `ziglint` and `zig fmt` on the changed files.

## Current Zig Patterns

**ArrayList:**
```zig
var list: std.ArrayList(u32) = .empty;
defer list.deinit(allocator);
try list.append(allocator, 42);
```

**HashMap/StringHashMap (default to unmanaged):**
```zig
var map: std.StringHashMapUnmanaged(u32) = .empty;
defer map.deinit(allocator);
try map.put(allocator, "key", 42);
```

**stdout/stderr writer:**
```zig
var buf: [4096]u8 = undefined;
var writer = std.fs.File.stdout().writer(&buf);
defer writer.interface.flush() catch {};
try writer.interface.print("hello {s}\n", .{"world"});
```

**build.zig executable:**
```zig
b.addExecutable(.{
    .name = "foo",
    .root_module = b.createModule(.{
        .root_source_file = b.path("src/main.zig"),
        .target = target,
        .optimize = optimize,
    }),
});
```

**Calling Convention**

Zig 0.15.2 uses `callconv(.c)`, not `callconv(.C)` (Note the lowercase .c)
- The calling convention for Windows APIs is `callconv(.winapi)`.

**JSON writing:**
```zig
var buf: [4096]u8 = undefined;
var writer = std.fs.File.stdout().writer(&buf);
defer writer.interface.flush() catch {};

var jw: std.json.Stringify = .{
    .writer = &writer.interface,
    .options = .{ .whitespace = .indent_2 },
};
try jw.write(my_struct);
```

**Allocating writer:**
```zig
var writer: std.Io.Writer.Allocating = .init(allocator);
defer writer.deinit();
try writer.writer.print("hello {s}", .{"world"});
const output = try writer.toOwnedSlice();
```

## Zig Style

- `camelCase` for functions and methods
- `snake_case` for variables, parameters, and constants
- `PascalCase` for types, structs, and enums
- prefer `const foo: Type = .{ .field = value };` over `const foo = Type{ .field = value };`
- preferred file order: `//!` module doc comment, `const Self = @This();` (for self-referential types), imports, `const log = std.log.scoped(...)`
- preferred methods order: `init` → `deinit` → public API → private helpers
- pass allocators explicitly; use `errdefer` for cleanup on error
- keep tests inline with the code they cover; register them in `src/main.zig`

## Safety

- Add assertions at API boundaries and state transitions; avoid trivial assertions.
- Keep functions small and push pure computation into helpers.
- Comments should explain why, not what.
