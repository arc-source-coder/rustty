const std = @import("std");
const handle_mod = @import("handle.zig");
const terminal = @import("ghostty/src/terminal/main.zig");

const TerminalHandle = handle_mod.TerminalHandle;

/// Set a selection on the terminal. Coordinates are in viewport space (0-indexed).
/// rectangular: 1 for block/rectangle selection, 0 for normal.
/// Returns 0 on success, 1 if null, 2 if coordinates can't be pinned.
export fn ghostty_vt_terminal_set_selection(
    ptr: ?*anyopaque,
    start_x: u16,
    start_y: u32,
    end_x: u16,
    end_y: u32,
    rectangular: u8,
) callconv(.c) c_int {
    if (ptr == null) return 1;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    const screen = handle.terminal_inst.screens.active;

    const start_pin = screen.pages.pin(.{ .viewport = .{
        .x = start_x,
        .y = start_y,
    } }) orelse return 2;

    const end_pin = screen.pages.pin(.{ .viewport = .{
        .x = end_x,
        .y = end_y,
    } }) orelse return 2;

    const sel = terminal.Selection.init(start_pin, end_pin, rectangular != 0);
    screen.select(sel) catch return 2;
    return 0;
}

/// Clear any active selection.
export fn ghostty_vt_terminal_clear_selection(ptr: ?*anyopaque) callconv(.c) void {
    if (ptr == null) return;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    handle.terminal_inst.screens.active.clearSelection();
}

/// Get the selected text as a UTF-8 null-terminated string.
/// Returns a pointer to the string, or null if no selection or error.
/// The caller must free the returned pointer with ghostty_vt_bytes_free().
export fn ghostty_vt_terminal_get_selection_text(
    ptr: ?*anyopaque,
    out_len: ?*usize,
) callconv(.c) ?[*]const u8 {
    if (ptr == null) return null;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    const screen = handle.terminal_inst.screens.active;

    const sel = screen.selection orelse return null;

    const text = screen.selectionString(handle.alloc, .{ .sel = sel }) catch return null;
    if (out_len) |len| len.* = text.len;
    return text.ptr;
}

/// Free a byte buffer returned by get_selection_text.
export fn ghostty_vt_bytes_free(bytes: ?[*]const u8, len: usize) callconv(.c) void {
    if (bytes == null) return;
    const alloc = std.heap.smp_allocator;
    // selectionString returns a [:0]const u8, so actual allocation is len+1
    const slice = @as([*]u8, @constCast(bytes.?))[0 .. len + 1];
    alloc.free(slice);
}
