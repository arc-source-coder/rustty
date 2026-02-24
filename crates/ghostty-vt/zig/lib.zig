const std = @import("std");
const Allocator = std.mem.Allocator;

const handle_mod = @import("handle.zig");

comptime {
    _ = @import("modes.zig");
    _ = @import("scroll.zig");
    _ = @import("render.zig");
    _ = @import("selection.zig");
    _ = @import("input.zig");
}

const BellCallback = handle_mod.BellCallback;
const TitleCallback = handle_mod.TitleCallback;
const TerminalHandle = handle_mod.TerminalHandle;

export fn ghostty_vt_terminal_new(cols: u16, rows: u16) callconv(.c) ?*anyopaque {
    const alloc = std.heap.smp_allocator;
    const handle = TerminalHandle.init(alloc, cols, rows) catch return null;
    return @ptrCast(handle);
}

export fn ghostty_vt_terminal_free(ptr: ?*anyopaque) callconv(.c) void {
    if (ptr == null) return;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    handle.deinit();
}

export fn ghostty_vt_terminal_set_callbacks(
    ptr: ?*anyopaque,
    userdata: ?*anyopaque,
    bell: ?BellCallback,
    title: ?TitleCallback,
) callconv(.c) void {
    if (ptr == null) return;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    handle.callbacks = .{
        .userdata = userdata,
        .bell = bell,
        .title = title,
    };
}

export fn ghostty_vt_terminal_feed(
    ptr: ?*anyopaque,
    bytes: [*]const u8,
    len: usize,
) callconv(.c) c_int {
    if (ptr == null) return 1;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    handle.stream.nextSlice(bytes[0..len]) catch return 2;
    return 0;
}

export fn ghostty_vt_terminal_resize(
    ptr: ?*anyopaque,
    cols: u16,
    rows: u16,
) callconv(.c) c_int {
    if (ptr == null) return 1;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    handle.terminal_inst.resize(handle.alloc, cols, rows) catch return 2;
    return 0;
}
