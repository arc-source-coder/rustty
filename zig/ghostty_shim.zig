const std = @import("std");
const handle_mod = @import("src/handle.zig");

pub const std_options: std.Options = .{ .log_level = .warn };

comptime {
    _ = @import("src/font.zig");
    _ = @import("src/sprite.zig");
    _ = @import("src/modes.zig");
    _ = @import("src/scroll.zig");
    _ = @import("src/render.zig");
    _ = @import("src/selection.zig");
    _ = @import("src/input.zig");
    _ = @import("src/queries.zig");
}

pub const input = @import("src/input.zig");
pub const queries = @import("src/queries.zig");

const BellCallback = handle_mod.BellCallback;
const OutputCallback = handle_mod.OutputCallback;
const TitleCallback = handle_mod.TitleCallback;
const TerminalHandle = handle_mod.TerminalHandle;
const WriteInputCallback = handle_mod.WriteInputCallback;

const terminal = @import("ghostty/src/terminal/main.zig");
const size = @import("ghostty/src/renderer/size.zig");

pub export fn ghostty_terminal_new(cols: u16, rows: u16, fg: u32, bg: u32) callconv(.c) ?*anyopaque {
    const alloc = std.heap.smp_allocator;
    const foreground: terminal.color.RGB = @bitCast(@as(u24, @truncate(fg)));
    const background: terminal.color.RGB = @bitCast(@as(u24, @truncate(bg)));
    const handle = TerminalHandle.init(alloc, cols, rows, foreground, background) catch return null;

    return @ptrCast(handle);
}

pub export fn ghostty_terminal_free(ptr: *anyopaque) callconv(.c) void {
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));
    handle.deinit();
}

/// Acquire the Zig-owned terminal mutex.
/// Must be paired with `ghostty_terminal_unlock` by the caller.
pub export fn ghostty_terminal_lock(ptr: *anyopaque) callconv(.c) void {
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));
    handle.lock();
}

/// Release the Zig-owned terminal mutex.
/// The caller must currently hold the mutex.
pub export fn ghostty_terminal_unlock(ptr: *anyopaque) callconv(.c) void {
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));
    handle.unlock();
}

pub export fn ghostty_terminal_set_callbacks(
    ptr: *anyopaque,
    userdata: ?*anyopaque,
    bell: ?BellCallback,
    title: ?TitleCallback,
    output: ?OutputCallback,
) callconv(.c) void {
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));
    handle.lock();
    defer handle.unlock();
    handle.event_callbacks = .{
        .userdata = userdata,
        .bell = bell,
        .title = title,
    };
    handle.output_callback = .{
        .userdata = userdata,
        .output = output,
    };
}

pub export fn ghostty_terminal_set_write_input(
    ptr: *anyopaque,
    userdata: ?*anyopaque,
    callback: ?WriteInputCallback,
) callconv(.c) void {
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));
    handle.lock();
    defer handle.unlock();
    handle.write_input = .{
        .userdata = userdata,
        .callback = callback,
    };
}

pub export fn ghostty_terminal_feed(
    ptr: *anyopaque,
    bytes: [*]const u8,
    len: usize,
) callconv(.c) void {
    if (len == 0) return;

    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));
    handle.lock();
    handle.stream.nextSlice(bytes[0..len]);
    handle.unlock();

    // Match Ghostty's processOutputLocked behavior: wake the host on every
    // non-empty feed and let the host coalesce renders at its own boundary.
    handle.outputTrampoline();
}

pub export fn ghostty_terminal_resize(
    ptr: *anyopaque,
    cols: u16,
    rows: u16,
) callconv(.c) c_int {
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));
    handle.lock();
    defer handle.unlock();
    handle.terminal_inst.resize(handle.alloc, cols, rows) catch return 2;

    const cell_w = handle.size.cell.width;
    const cell_h = handle.size.cell.height;
    // Update pixel dimensions for Kitty graphics protocol.
    // We read cell dimensions from handle since they're stored separately.
    if (cell_w > 0 and cell_h > 0) {
        handle.terminal_inst.width_px = @as(u32, cols) * cell_w;
        handle.terminal_inst.height_px = @as(u32, rows) * cell_h;
    }

    return 0;
}

// TODO: Add doc comments for the below

pub export fn ghostty_terminal_set_screen_dimensions(
    ptr: *anyopaque,
    dimensions: size.ScreenSize,
) callconv(.c) void {
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));

    handle.size.screen = dimensions;

    // Update pixel dimensions for Kitty graphics protocol.
    handle.terminal_inst.width_px = handle.size.screen.width;
    handle.terminal_inst.height_px = handle.size.screen.height;
}

pub export fn ghostty_terminal_set_cell_dimensions(
    ptr: *anyopaque,
    dimensions: size.CellSize,
) callconv(.c) void {
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));

    // Store cell dimensions for resize calculations and CSI reports
    handle.size.cell = dimensions;
}

pub export fn ghostty_terminal_get_render_dimensions(
    ptr: *anyopaque,
) callconv(.c) handle_mod.Size {
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));
    return .{
        .cell = handle.size.cell,
        .screen = handle.size.screen,
        .padding = handle.size.padding,
    };
}
