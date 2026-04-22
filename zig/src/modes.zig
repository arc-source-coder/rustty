const TerminalHandle = @import("handle.zig").TerminalHandle;

/// Whether synchronized output mode (DEC 2026) is active.
pub export fn ghostty_terminal_is_synchronized_output(ptr: *anyopaque) callconv(.c) bool {
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));
    handle.lock();
    defer handle.unlock();
    return handle.terminal_inst.modes.get(.synchronized_output);
}

/// Reset synchronized output mode (DEC 2026). Called by the
/// sync-output safety timer to prevent frozen terminals.
pub export fn ghostty_terminal_reset_synchronized_output(ptr: *anyopaque) callconv(.c) void {
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));
    handle.lock();
    defer handle.unlock();
    handle.terminal_inst.modes.set(.synchronized_output, false);
}

/// Whether focus event mode (DEC 1004) is active.
pub export fn ghostty_terminal_is_focus_event_mode(ptr: *anyopaque) callconv(.c) bool {
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));
    handle.lock();
    defer handle.unlock();
    return handle.terminal_inst.modes.get(.focus_event);
}

/// Whether the alternate screen is active.
pub export fn ghostty_terminal_is_alternate_screen(ptr: *anyopaque) callconv(.c) bool {
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));
    handle.lock();
    defer handle.unlock();
    return handle.terminal_inst.screens.active_key == .alternate;
}

/// Whether mouse reporting is enabled.
pub export fn ghostty_terminal_is_mouse_reporting(ptr: *anyopaque) callconv(.c) bool {
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));
    handle.lock();
    defer handle.unlock();
    return handle.terminal_inst.flags.mouse_event != .none;
}

/// Whether alternate scroll mode (DEC 1007) is active.
pub export fn ghostty_terminal_is_mouse_alternate_scroll(ptr: *anyopaque) callconv(.c) bool {
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));
    handle.lock();
    defer handle.unlock();
    return handle.terminal_inst.modes.get(.mouse_alternate_scroll);
}
