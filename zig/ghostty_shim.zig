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
const ClipboardWriteCallback = handle_mod.ClipboardWriteCallback;
const OutputCallback = handle_mod.OutputCallback;
const TitleCallback = handle_mod.TitleCallback;
const TerminalHandle = handle_mod.TerminalHandle;
const TerminalDimensions = handle_mod.TerminalDimensions;
const WriteInputCallback = handle_mod.WriteInputCallback;

const terminal = @import("ghostty/src/terminal/main.zig");

pub export fn ghostty_terminal_new(dimensions: TerminalDimensions, fg: u32, bg: u32) callconv(.c) ?*anyopaque {
    const alloc = std.heap.smp_allocator;
    const foreground: terminal.color.RGB = @bitCast(@as(u24, @truncate(fg)));
    const background: terminal.color.RGB = @bitCast(@as(u24, @truncate(bg)));
    const handle = TerminalHandle.init(alloc, dimensions, foreground, background) catch return null;

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
    clipboard_write: ?ClipboardWriteCallback,
    output: ?OutputCallback,
) callconv(.c) void {
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));
    handle.lock();
    defer handle.unlock();
    handle.event_callbacks = .{
        .userdata = userdata,
        .bell = bell,
        .title = title,
        .clipboard_write = clipboard_write,
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

pub export fn ghostty_terminal_resize(ptr: *anyopaque, dimensions: TerminalDimensions) callconv(.c) void {
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));
    handle.lock();
    defer handle.unlock();
    handle.stream.handler.resize(.{
        .cols = dimensions.grid.columns,
        .rows = dimensions.grid.rows,
        .cell_size_px = .{
            .width = dimensions.cell.width,
            .height = dimensions.cell.height,
        },
    }) catch |err| @panic(@errorName(err));
    handle.size = .{ .screen = dimensions.screen, .cell = dimensions.cell, .padding = .{} };
}
