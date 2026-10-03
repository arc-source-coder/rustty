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

pub const MouseMode = extern struct {
    is_alternate_screen: bool,
    is_mouse_reporting: bool,
    is_mouse_alternate_scroll: bool,
    is_mouse_shift_capture: bool,
};

pub export fn ghostty_terminal_get_mouse_mode(ptr: *anyopaque) callconv(.c) MouseMode {
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));
    handle.lock();
    defer handle.unlock();
    return .{
        // Whether the alternate screen is active.
        .is_alternate_screen = handle.terminal_inst.screens.active_key == .alternate,
        // Whether mouse reporting is enabled.
        .is_mouse_reporting = handle.terminal_inst.flags.mouse_event != .none,
        .is_mouse_alternate_scroll = handle.terminal_inst.modes.get(.mouse_alternate_scroll),
        .is_mouse_shift_capture = handle.terminal_inst.flags.mouse_shift_capture == .true,
    };
}
