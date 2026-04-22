const handle_mod = @import("handle.zig");
const TerminalHandle = handle_mod.TerminalHandle;

pub const RGB = extern struct {
    r: u8,
    g: u8,
    b: u8,
};

/// Query the current screen buffer dimensions.
pub export fn ghostty_terminal_get_size(
    ptr: *anyopaque,
    cols: *u16,
    rows: *u16,
) callconv(.c) void {
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));
    handle.lock();
    defer handle.unlock();
    const t = &handle.terminal_inst;

    cols.* = t.cols;
    rows.* = t.rows;
}

/// Query the current cursor position on the active screen.
pub export fn ghostty_terminal_get_cursor_position(
    ptr: *anyopaque,
    col: *u16,
    row: *u16,
) callconv(.c) void {
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));
    handle.lock();
    defer handle.unlock();
    const screen = handle.terminal_inst.screens.active;

    col.* = screen.cursor.x;
    row.* = screen.cursor.y;
}

pub export fn ghostty_terminal_get_cursor_visible(
    ptr: *anyopaque,
    visible: *bool,
) callconv(.c) void {
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));
    handle.lock();
    defer handle.unlock();

    visible.* = handle.terminal_inst.modes.get(.cursor_visible);
}

pub export fn ghostty_terminal_get_cell_size(
    ptr: *anyopaque,
    width_px: *u16,
    height_px: *u16,
) callconv(.c) void {
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));
    handle.lock();
    defer handle.unlock();

    width_px.* = @intCast(handle.size.cell.width);
    height_px.* = @intCast(handle.size.cell.height);
}

pub export fn ghostty_terminal_get_base16_palette(
    ptr: *anyopaque,
    out: *[16]RGB,
) callconv(.c) void {
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));
    const palette: *const [256]RGB = @ptrCast(&handle.render_state.colors.palette);
    out.* = palette[0..16].*;
}
