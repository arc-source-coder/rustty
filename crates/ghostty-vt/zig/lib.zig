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
const ResponseCallback = handle_mod.ResponseCallback;
const TerminalHandle = handle_mod.TerminalHandle;

const terminal = @import("ghostty/src/terminal/main.zig");
const color = terminal.color;

export fn ghostty_vt_terminal_new(
    cols: u16,
    rows: u16,
    fg_r: u8,
    fg_g: u8,
    fg_b: u8,
    bg_r: u8,
    bg_g: u8,
    bg_b: u8,
) callconv(.c) ?*anyopaque {
    const alloc = std.heap.smp_allocator;
    const fg: color.RGB = .{ .r = fg_r, .g = fg_g, .b = fg_b };
    const bg: color.RGB = .{ .r = bg_r, .g = bg_g, .b = bg_b };
    const handle = TerminalHandle.init(alloc, cols, rows, fg, bg) catch return null;
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
    response: ?ResponseCallback,
) callconv(.c) void {
    if (ptr == null) return;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    handle.callbacks = .{
        .userdata = userdata,
        .bell = bell,
        .title = title,
        .response = response,
        .handle = handle,
    };
}

export fn ghostty_vt_terminal_feed(
    ptr: ?*anyopaque,
    bytes: [*]const u8,
    len: usize,
) callconv(.c) c_int {
    if (ptr == null) return 1;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    // TODO: figure out a way to only mark as dirty when the palette actually cahnges
    handle.palette_dirty = true;
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

    // Update pixel dimensions for Kitty graphics protocol.
    // We read cell dimensions from handle since they're stored separately.
    if (handle.cell_width_px > 0 and handle.cell_height_px > 0) {
        handle.terminal_inst.width_px = @as(u32, cols) * @as(u32, handle.cell_width_px);
        handle.terminal_inst.height_px = @as(u32, rows) * @as(u32, handle.cell_height_px);
    }

    return 0;
}

/// Set cell pixel dimensions for size reports (CSI 14t, CSI 16t) and Kitty graphics.
/// Called by the renderer whenever font metrics change.
export fn ghostty_vt_terminal_set_cell_size(
    ptr: ?*anyopaque,
    width_px: u16,
    height_px: u16,
) callconv(.c) void {
    if (ptr == null) return;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));

    // Store cell dimensions for resize calculations and CSI reports
    handle.cell_width_px = width_px;
    handle.cell_height_px = height_px;

    // Update pixel dimensions for Kitty graphics protocol.
    if (width_px > 0 and height_px > 0) {
        handle.terminal_inst.width_px = @as(u32, handle.terminal_inst.cols) * @as(u32, width_px);
        handle.terminal_inst.height_px = @as(u32, handle.terminal_inst.rows) * @as(u32, height_px);
    }
}
