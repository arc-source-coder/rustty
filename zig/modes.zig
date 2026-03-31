const TerminalHandle = @import("handle.zig").TerminalHandle;

/// Returns mouse event mode: 0=none, 1=x10, 2=normal, 3=button, 4=any
export fn ghostty_terminal_get_mouse_mode(ptr: ?*anyopaque) callconv(.c) u8 {
    if (ptr == null) return 0;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    return @intFromEnum(handle.terminal_inst.flags.mouse_event);
}

/// Returns mouse format: 0=x10, 1=utf8, 2=sgr, 3=urxvt, 4=sgr_pixels
export fn ghostty_terminal_get_mouse_format(ptr: ?*anyopaque) callconv(.c) u8 {
    if (ptr == null) return 0;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    return @intFromEnum(handle.terminal_inst.flags.mouse_format);
}

/// Returns 1 if bracketed paste mode is active, 0 otherwise
export fn ghostty_terminal_is_bracketed_paste(ptr: ?*anyopaque) callconv(.c) u8 {
    if (ptr == null) return 0;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    return @intFromBool(handle.terminal_inst.modes.get(.bracketed_paste));
}

/// Returns kitty keyboard flags as a u8 bitfield (5 bits used)
export fn ghostty_terminal_get_kitty_keyboard_flags(ptr: ?*anyopaque) callconv(.c) u8 {
    if (ptr == null) return 0;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    const flags = handle.terminal_inst.screens.active.kitty_keyboard.current();

    const casted_flags: u5 = @bitCast(flags);
    return @as(u8, casted_flags);
}

/// Returns 1 if synchronized output mode (DEC 2026) is active, 0 otherwise
export fn ghostty_terminal_is_synchronized_output(ptr: ?*anyopaque) callconv(.c) u8 {
    if (ptr == null) return 0;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    return @intFromBool(handle.terminal_inst.modes.get(.synchronized_output));
}

/// Reset synchronized output mode (DEC 2026). Called by the sync-output
/// safety timer to prevent frozen terminals.
export fn ghostty_terminal_reset_synchronized_output(ptr: ?*anyopaque) callconv(.c) void {
    if (ptr == null) return;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    handle.terminal_inst.modes.set(.synchronized_output, false);
}

/// Returns 1 if focus event mode (DEC 1004) is active, 0 otherwise
export fn ghostty_terminal_is_focus_event_mode(ptr: ?*anyopaque) callconv(.c) u8 {
    if (ptr == null) return 0;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    return @intFromBool(handle.terminal_inst.modes.get(.focus_event));
}

/// Returns 1 if the alternate screen is active, 0 for primary.
export fn ghostty_terminal_is_alternate_screen(ptr: ?*anyopaque) callconv(.c) u8 {
    if (ptr == null) return 0;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    return @intFromBool(handle.terminal_inst.screens.active_key == .alternate);
}

/// Returns 1 if alternate scroll mode (DEC 1007) is active, 0 otherwise.
export fn ghostty_terminal_is_mouse_alternate_scroll(ptr: ?*anyopaque) callconv(.c) u8 {
    if (ptr == null) return 0;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    return @intFromBool(handle.terminal_inst.modes.get(.mouse_alternate_scroll));
}
