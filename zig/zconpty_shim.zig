//! Ghostty-backed zconpty shim.
//!
//! This file redeclares the Ghostty shim functions with `extern fn` instead of
//! importing export-bearing shim modules directly. Importing those modules here
//! would compile the exported `ghostty_terminal_*` symbols into this artifact
//! too, which causes duplicate-symbol linker failures.

const std = @import("std");
const zconpty = @import("zconpty");
const handle_mod = @import("src/handle.zig");

const windows = zconpty.Windows;
const server = zconpty.Server;
const input_types = zconpty.InputTypes;
const Terminal = zconpty.Terminal;
const TerminalHandle = handle_mod.TerminalHandle;
const surface = @import("src/surfaceRect.zig");

const WriteInputCallback = handle_mod.WriteInputCallback;

extern fn ghostty_terminal_set_write_input(
    ptr: *anyopaque,
    userdata: ?*anyopaque,
    callback: ?WriteInputCallback,
) callconv(.c) void;

extern fn ghostty_terminal_feed(ptr: *anyopaque, bytes: [*]const u8, len: usize) callconv(.c) void;

extern fn ghostty_terminal_encode_key(
    ptr: *anyopaque,
    key: c_int,
    action: input_types.KeyAction,
    mods: input_types.Mods,
    consumed_mods: input_types.Mods,
    text_ptr: ?[*]const u8,
    text_len: usize,
    unshifted_codepoint: u32,
    out: ?[*]u8,
    out_len: usize,
) callconv(.c) usize;

extern fn ghostty_terminal_encode_mouse(
    ptr: *anyopaque,
    button: i8,
    action: input_types.MouseAction,
    mods: input_types.Mods,
    x: f32,
    y: f32,
    out: ?[*]u8,
    out_len: usize,
) callconv(.c) usize;

extern fn ghostty_terminal_encode_paste(
    ptr: *anyopaque,
    text_ptr: ?[*]const u8,
    text_len: usize,
    out: ?[*]u8,
    out_len: usize,
) callconv(.c) usize;

extern fn ghostty_terminal_get_size(ptr: *anyopaque, cols: *u16, rows: *u16) callconv(.c) void;

extern fn ghostty_terminal_get_cursor_position(
    ptr: *anyopaque,
    col: *u16,
    row: *u16,
) callconv(.c) void;

extern fn ghostty_terminal_get_cursor_visible(ptr: *anyopaque, visible: *bool) callconv(.c) void;

extern fn ghostty_terminal_get_cell_size(
    ptr: *anyopaque,
    width_px: *u16,
    height_px: *u16,
) callconv(.c) void;

extern fn ghostty_terminal_get_base16_palette(
    ptr: *anyopaque,
    out: *[16]Terminal.RGB,
) callconv(.c) void;

const ghostty_vtable: Terminal.VTable = .{
    .feed = ghosttyFeed,
    .vt_encode_key = ghosttyEncodeKey,
    .vt_encode_mouse = ghosttyEncodeMouse,
    .vt_encode_focus = ghosttyEncodeFocus,
    .vt_encode_paste = ghosttyEncodePaste,
    .get_size = ghosttyGetSize,
    .get_cursor_position = ghosttyGetCursorPosition,
    .get_cursor_visible = ghosttyGetCursorVisible,
    .get_cell_size = ghosttyGetCellSize,
    .get_title = ghosttyGetTitle,
    .get_base16_palette = ghosttyGetBase16Palette,
    .read_rect = ghosttyReadRect,
    .write_rect = ghosttyWriteRect,
    .fill_span = ghosttyFillSpan,
};

export fn zconpty_start_console_server(
    terminal_ptr: *anyopaque,
    out_session: *isize,
) callconv(.c) windows.HRESULT {
    out_session.* = 0;

    const ptr = terminal_ptr;

    var session: ?*server.Session = null;
    const terminal = Terminal.init(ptr, &ghostty_vtable);
    const create_hr = server.create(std.heap.smp_allocator, terminal, &session);
    if (create_hr != windows.S_OK) return create_hr;

    const console_session = session.?;
    errdefer server.stop(console_session);

    ghostty_terminal_set_write_input(ptr, console_session, &server.writeInputCallback);
    errdefer ghostty_terminal_set_write_input(ptr, null, null);

    const start_hr = server.start(console_session);
    if (start_hr != windows.S_OK) return start_hr;

    out_session.* = @intCast(@intFromPtr(console_session));
    return windows.S_OK;
}

export fn zconpty_stop_console_server(session: isize) callconv(.c) void {
    if (session == 0) return;

    const console_session: *server.Session = @ptrFromInt(@as(usize, @intCast(session)));
    ghostty_terminal_set_write_input(console_session.state.terminal.ptr, null, null);
    server.stop(console_session);
}

export fn zconpty_send_key(session: isize, event: input_types.KeyEvent) callconv(.c) void {
    sessionFromHandle(session).input_subsystem.sendKey(event);
}

export fn zconpty_send_mouse(session: isize, event: input_types.MouseEvent) callconv(.c) void {
    sessionFromHandle(session).input_subsystem.sendMouse(event);
}

export fn zconpty_send_focus(session: isize, focused: bool) callconv(.c) void {
    sessionFromHandle(session).input_subsystem.sendFocus(focused);
}

export fn zconpty_send_paste(
    session: isize,
    text_ptr: [*]const u8,
    text_len: usize,
) callconv(.c) void {
    if (text_len == 0) return;
    sessionFromHandle(session).input_subsystem.sendPaste(text_ptr[0..text_len]);
}

export fn zconpty_send_resize(session: isize, cols: u16, rows: u16) callconv(.c) void {
    sessionFromHandle(session).input_subsystem.sendResize(cols, rows);
}

export fn zconpty_key_from_w3c(code_ptr: [*]const u8, code_len: usize) callconv(.c) c_int {
    const code = input_types.W3CCode.fromW3C(code_ptr[0..code_len]);
    if (code) |c| {
        return @intCast(@intFromEnum(c));
    } else {
        return -1;
    }
}

fn sessionFromHandle(session: isize) *server.Session {
    std.debug.assert(session != 0);
    return @ptrFromInt(@as(usize, @intCast(session)));
}

fn ghosttyFeed(ptr: *anyopaque, bytes: []const u8) void {
    if (bytes.len == 0) return;
    ghostty_terminal_feed(ptr, bytes.ptr, bytes.len);
}

fn ghosttyEncodeKey(
    ptr: *anyopaque,
    event: *const input_types.KeyEvent,
    out: [*]u8,
    out_len: usize,
) usize {
    return ghostty_terminal_encode_key(
        ptr,
        @intCast(@intFromEnum(event.code)),
        event.action,
        @bitCast(event.mods),
        @bitCast(event.consumed_mods),
        if (event.text_len == 0) null else @ptrCast(&event.text),
        @as(usize, event.text_len),
        event.unshifted_codepoint,
        out,
        out_len,
    );
}

fn ghosttyEncodeMouse(
    ptr: *anyopaque,
    event: *const input_types.MouseEvent,
    out: [*]u8,
    out_len: usize,
) usize {
    return ghostty_terminal_encode_mouse(
        ptr,
        @intFromEnum(event.button),
        event.action,
        @bitCast(event.mods),
        event.position.x_px,
        event.position.y_px,
        out,
        out_len,
    );
}

fn ghosttyEncodeFocus(ptr: *anyopaque, focused: bool, out: [*]u8, out_len: usize) usize {
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));
    const t = &handle.terminal_inst;
    handle.lock();
    const focus_event_mode = t.modes.get(.focus_event);
    handle.unlock();

    if (out_len < 3 or focus_event_mode == false) return 0;

    const bytes = if (focused) "\x1b[I" else "\x1b[O";
    @memcpy(out[0..bytes.len], bytes);
    return bytes.len;
}

fn ghosttyEncodePaste(
    ptr: *anyopaque,
    out: [*]u8,
    text: [*]const u8,
    text_len: usize,
    out_len: usize,
) usize {
    return ghostty_terminal_encode_paste(ptr, text, text_len, out, out_len);
}

fn ghosttyGetSize(ptr: *anyopaque, cols: *u16, rows: *u16) void {
    ghostty_terminal_get_size(ptr, cols, rows);
}

fn ghosttyGetCursorPosition(ptr: *anyopaque, col: *u16, row: *u16) void {
    ghostty_terminal_get_cursor_position(ptr, col, row);
}

fn ghosttyGetCursorVisible(ptr: *anyopaque, visible: *bool) void {
    ghostty_terminal_get_cursor_visible(ptr, visible);
}

fn ghosttyGetCellSize(ptr: *anyopaque, width_px: *u16, height_px: *u16) void {
    ghostty_terminal_get_cell_size(ptr, width_px, height_px);
}

fn ghosttyGetTitle(ptr: *anyopaque, out: [*]u8, out_len: usize) usize {
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));
    handle.lock();
    defer handle.unlock();

    const title = handle.terminal_inst.getTitle() orelse "";
    const written = @min(title.len, out_len);
    if (written > 0) {
        @memcpy(out[0..written], title[0..written]);
    }
    return title.len;
}

fn ghosttyGetBase16Palette(ptr: *anyopaque, out: *[16]Terminal.RGB) void {
    ghostty_terminal_get_base16_palette(ptr, out);
}

fn ghosttyReadRect(ptr: *anyopaque, rect: Terminal.Rect, out: [*]Terminal.Cell) Terminal.Rect {
    return surface.readRect(ptr, rect, out);
}

fn ghosttyWriteRect(ptr: *anyopaque, rect: Terminal.Rect, cells: [*]const Terminal.Cell) Terminal.Rect {
    return surface.writeRect(ptr, rect, cells);
}

fn ghosttyFillSpan(
    ptr: *anyopaque,
    start: Terminal.Point,
    len: u32,
    kind: Terminal.FillKind,
    cell: Terminal.Cell,
) u32 {
    return surface.fillSpan(ptr, start, len, kind, cell);
}
