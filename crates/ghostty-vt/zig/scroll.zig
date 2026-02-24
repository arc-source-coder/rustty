const TerminalHandle = @import("handle.zig").TerminalHandle;

/// Scroll the viewport by delta rows (negative = up/towards history, positive = down)
export fn ghostty_vt_terminal_scroll_viewport(ptr: ?*anyopaque, delta: i32) callconv(.c) void {
    if (ptr == null) return;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    handle.terminal_inst.scrollViewport(.{ .delta = @intCast(delta) });
}

/// Scroll the viewport to the top of scrollback
export fn ghostty_vt_terminal_scroll_viewport_top(ptr: ?*anyopaque) callconv(.c) void {
    if (ptr == null) return;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    handle.terminal_inst.scrollViewport(.top);
}

/// Scroll the viewport to the bottom (active area)
export fn ghostty_vt_terminal_scroll_viewport_bottom(ptr: ?*anyopaque) callconv(.c) void {
    if (ptr == null) return;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    handle.terminal_inst.scrollViewport(.bottom);
}
