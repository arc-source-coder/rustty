const TerminalHandle = @import("handle.zig").TerminalHandle;
const Scrollbar = @import("../ghostty/src/terminal/PageList.zig").Scrollbar;

/// Scroll the viewport by delta rows (negative = up/towards history, positive = down)
pub export fn ghostty_terminal_scroll_viewport(ptr: *anyopaque, delta: i32) callconv(.c) void {
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));
    handle.lock();
    defer handle.unlock();
    handle.terminal_inst.scrollViewport(.{ .delta = @intCast(delta) });
}

/// Scroll the viewport to the top of scrollback
pub export fn ghostty_terminal_scroll_viewport_top(ptr: *anyopaque) callconv(.c) void {
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));
    handle.lock();
    defer handle.unlock();
    handle.terminal_inst.scrollViewport(.top);
}

/// Scroll the viewport to the bottom (active area)
pub export fn ghostty_terminal_scroll_viewport_bottom(ptr: *anyopaque) callconv(.c) void {
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));
    handle.lock();
    defer handle.unlock();
    handle.terminal_inst.scrollViewport(.bottom);
}

/// Query scrollbar positioning info.
/// The caller must already hold the terminal mutex.
pub export fn ghostty_terminal_scrollbar_info(ptr: *anyopaque) callconv(.c) Scrollbar.C {
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));
    return handle.terminal_inst.screens.active.pages.scrollbar().cval();
}

/// Whether the viewport is at the bottom (active area).
pub export fn ghostty_terminal_viewport_is_bottom(ptr: *anyopaque) callconv(.c) bool {
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));
    handle.lock();
    defer handle.unlock();
    return handle.terminal_inst.screens.active.viewportIsBottom();
}

/// Scroll the viewport to an absolute row offset from the top.
/// Clamped internally: row >= total_rows - viewport_rows scrolls to bottom.
pub export fn ghostty_terminal_scroll_to_row(ptr: *anyopaque, row: u64) callconv(.c) void {
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));
    handle.lock();
    defer handle.unlock();
    // Use Screen.scroll(.{ .row = N }) for absolute positioning.
    handle.terminal_inst.screens.active.scroll(.{ .row = @intCast(row) });
}
