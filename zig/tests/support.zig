const std = @import("std");
const color = @import("../ghostty/src/terminal/color.zig");

pub const lib = @import("../ghostty_shim.zig");
pub const handle_mod = @import("../src/handle.zig");
pub const input = @import("../src/input.zig");
pub const modes = @import("../src/modes.zig");
pub const render = @import("../src/render.zig");
pub const selection = @import("../src/selection.zig");
pub const scroll = @import("../src/scroll.zig");

pub const TerminalHandle = handle_mod.TerminalHandle;

pub const default_fg: color.RGB = .{ .r = 0xDD, .g = 0xDD, .b = 0xDD };
pub const default_bg: color.RGB = .{ .r = 0x1E, .g = 0x1E, .b = 0x2E };

var test_allocator: ?std.mem.Allocator = null;

pub fn setAllocator(gpa_allocator: std.mem.Allocator) void {
    test_allocator = gpa_allocator;
}

pub fn allocator() std.mem.Allocator {
    return test_allocator orelse @panic("test allocator not initialized");
}

pub fn initHandle() !*TerminalHandle {
    return TerminalHandle.init(allocator(), 80, 24, default_fg, default_bg);
}

pub fn feed(handle: *TerminalHandle, bytes: []const u8) void {
    return lib.ghostty_terminal_feed(handle, bytes.ptr, bytes.len);
}

pub fn renderUpdate(handle: *TerminalHandle) !void {
    lib.ghostty_terminal_lock(handle);
    defer lib.ghostty_terminal_unlock(handle);
    try expectOk(render.ghostty_terminal_render_update(handle));
}

pub fn scrollbarInfo(handle: *TerminalHandle, out: *scroll.ScrollbarInfoC) void {
    lib.ghostty_terminal_lock(handle);
    defer lib.ghostty_terminal_unlock(handle);
    return scroll.ghostty_terminal_scrollbar_info(handle, out);
}

pub fn expectOk(rc: c_int) !void {
    try std.testing.expectEqual(@as(c_int, 0), rc);
}
