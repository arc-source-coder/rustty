const std = @import("std");
const handle_mod = @import("handle.zig");
const terminal = @import("ghostty/src/terminal/main.zig");

const TerminalHandle = handle_mod.TerminalHandle;
const Screen = terminal.Screen;

/// Mirrors Ghostty's default `selection-word-chars` codepoints from
/// config/Config.zig (SelectionWordChars.default_codepoints).
const default_selection_word_chars = [_]u21{
    0, // null
    ' ', // space
    '\t', // tab
    '\'', // single quote
    '"', // double quote
    0x2502, // U+2502 box drawing vertical bar
    '`', // backtick
    '|', // pipe
    ':', // colon
    ';', // semicolon
    ',', // comma
    '(', // left paren
    ')', // right paren
    '[', // left bracket
    ']', // right bracket
    '{', // left brace
    '}', // right brace
    '<', // less than
    '>', // greater than
    '$', // dollar
};

fn viewportPin(screen: *Screen, x: u16, y: u32) ?terminal.Pin {
    return screen.pages.pin(.{ .viewport = .{
        .x = x,
        .y = y,
    } });
}

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

/// Select the word at viewport coordinates (0-indexed) using Ghostty's
/// `Screen.selectWord` with default boundary semantics.
/// Returns 0 on success, non-zero if no selectable word or invalid point.
export fn ghostty_vt_terminal_select_word_at(
    ptr: ?*anyopaque,
    x: u16,
    y: u32,
) callconv(.c) c_int {
    if (ptr == null) return 1;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    const screen = handle.terminal_inst.screens.active;

    const pin = viewportPin(screen, x, y) orelse return 2;
    const sel = screen.selectWord(pin, &default_selection_word_chars) orelse return 3;
    screen.select(sel) catch return 4;
    return 0;
}

/// Select the (soft-wrapped) line at viewport coordinates (0-indexed) using
/// Ghostty's `Screen.selectLine` default semantics.
/// Returns 0 on success, non-zero if no selectable line or invalid point.
export fn ghostty_vt_terminal_select_line_at(
    ptr: ?*anyopaque,
    x: u16,
    y: u32,
) callconv(.c) c_int {
    if (ptr == null) return 1;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    const screen = handle.terminal_inst.screens.active;

    const pin = viewportPin(screen, x, y) orelse return 2;
    const sel = screen.selectLine(.{ .pin = pin }) orelse return 3;
    screen.select(sel) catch return 4;
    return 0;
}

/// Select shell output at viewport coordinates (0-indexed) using Ghostty's
/// semantic-prompt-aware `Screen.selectOutput` semantics.
/// Returns 0 on success, non-zero if no selectable output or invalid point.
export fn ghostty_vt_terminal_select_output_at(
    ptr: ?*anyopaque,
    x: u16,
    y: u32,
) callconv(.c) c_int {
    if (ptr == null) return 1;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    const screen = handle.terminal_inst.screens.active;

    const pin = viewportPin(screen, x, y) orelse return 2;
    const sel = screen.selectOutput(pin) orelse return 3;
    screen.select(sel) catch return 4;
    return 0;
}

/// Update selection during a double-click drag using Ghostty semantics:
/// expand by whole words nearest the click and drag endpoints.
/// Returns 0 on success, non-zero on invalid points or no selectable words.
export fn ghostty_vt_terminal_select_word_drag(
    ptr: ?*anyopaque,
    click_x: u16,
    click_y: u32,
    drag_x: u16,
    drag_y: u32,
) callconv(.c) c_int {
    if (ptr == null) return 1;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    const screen = handle.terminal_inst.screens.active;

    const click_pin = viewportPin(screen, click_x, click_y) orelse return 2;
    const drag_pin = viewportPin(screen, drag_x, drag_y) orelse return 3;

    const word_start = screen.selectWordBetween(
        click_pin,
        drag_pin,
        &default_selection_word_chars,
    ) orelse {
        screen.clearSelection();
        return 5;
    };

    const word_current = screen.selectWordBetween(
        drag_pin,
        click_pin,
        &default_selection_word_chars,
    ) orelse {
        screen.clearSelection();
        return 6;
    };

    if (drag_pin.before(click_pin)) {
        const sel = terminal.Selection.init(
            word_current.start(),
            word_start.end(),
            false,
        );
        screen.select(sel) catch return 4;
    } else {
        const sel = terminal.Selection.init(
            word_start.start(),
            word_current.end(),
            false,
        );
        screen.select(sel) catch return 4;
    }

    return 0;
}

/// Update selection during a triple-click drag using Ghostty semantics:
/// expand by whole wrapped lines nearest the click and drag endpoints.
/// Returns 0 on success, non-zero on invalid points or non-selectable lines.
export fn ghostty_vt_terminal_select_line_drag(
    ptr: ?*anyopaque,
    click_x: u16,
    click_y: u32,
    drag_x: u16,
    drag_y: u32,
) callconv(.c) c_int {
    if (ptr == null) return 1;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    const screen = handle.terminal_inst.screens.active;

    const click_pin = viewportPin(screen, click_x, click_y) orelse return 2;
    const drag_pin = viewportPin(screen, drag_x, drag_y) orelse return 3;

    const line = screen.selectLine(.{ .pin = drag_pin }) orelse return 5;
    const clicked = screen.selectLine(.{ .pin = click_pin }) orelse
        screen.selectLine(.{ .pin = click_pin, .whitespace = null }) orelse return 6;

    var sel = clicked;
    if (drag_pin.before(click_pin)) {
        sel.startPtr().* = line.start();
    } else {
        sel.endPtr().* = line.end();
    }

    screen.select(sel) catch return 4;
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
