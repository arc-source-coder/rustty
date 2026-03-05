//! Input encoding: key events, mouse events, and mode flag snapshots.
//!
//! API: ghostty_vt_terminal_get_input_opts to snapshot all mode flags under
//! the lock, then ghostty_vt_encode_key / ghostty_vt_encode_mouse to encode
//! without a terminal reference. This confines the lock to a single cheap
//! flag-read FFI call.

const std = @import("std");

const handle_mod = @import("handle.zig");
const TerminalHandle = handle_mod.TerminalHandle;

const key_encode = @import("ghostty/src/input/key_encode.zig");
const input_key = @import("ghostty/src/input/key.zig");
const KittyFlags = @import("ghostty/src/terminal/kitty/key.zig").Flags;
const Terminal = @import("ghostty/src/terminal/Terminal.zig");

/// C-compatible snapshot of all terminal input mode flags.
///
/// Captured once under the terminal mutex via ghostty_vt_terminal_get_input_opts.
/// All encode functions below take this struct instead of a terminal handle,
/// so encoding runs entirely outside the mutex.
///
/// Field layout is stable: u8 per field, no padding between them.
pub const InputOptsC = extern struct {
    // Key encoding (matches key_encode.Options fields from terminal state)
    cursor_key_application: u8,
    keypad_key_application: u8,
    ignore_keypad_with_numlock: u8,
    alt_esc_prefix: u8,
    modify_other_keys_state_2: u8,
    /// KittyFlags packed u5, widened to u8 for C ABI.
    kitty_flags: u8,
    // Mouse encoding (raw enum discriminants, match MouseEvents/MouseFormat)
    mouse_event: u8,
    mouse_format: u8,
    // Other input flags
    bracketed_paste: u8,
    focus_event_mode: u8,
};

/// Snapshot all input-relevant mode flags from the terminal into an InputOptsC.
///
/// Must be called with the terminal mutex held. The returned struct is a
/// plain-data copy — no terminal reference is retained, so encoding can
/// proceed lock-free using ghostty_vt_encode_key_opts / ghostty_vt_encode_mouse_opts.
export fn ghostty_vt_terminal_get_input_opts(ptr: ?*anyopaque) callconv(.c) InputOptsC {
    // Return safe defaults (no special modes) when handle is null.
    if (ptr == null) return .{
        .cursor_key_application = 0,
        .keypad_key_application = 0,
        .ignore_keypad_with_numlock = 0,
        .alt_esc_prefix = 0,
        .modify_other_keys_state_2 = 0,
        .kitty_flags = 0,
        .mouse_event = 0,
        .mouse_format = 0,
        .bracketed_paste = 0,
        .focus_event_mode = 0,
    };
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    const t = &handle.terminal_inst;

    const raw_kitty_flags: u5 = @bitCast(t.screens.active.kitty_keyboard.current());

    return .{
        .cursor_key_application = @intFromBool(t.modes.get(.cursor_keys)),
        .keypad_key_application = @intFromBool(t.modes.get(.keypad_keys)),
        .ignore_keypad_with_numlock = @intFromBool(t.modes.get(.ignore_keypad_with_numlock)),
        .alt_esc_prefix = @intFromBool(t.modes.get(.alt_esc_prefix)),
        .modify_other_keys_state_2 = @intFromBool(t.flags.modify_other_keys_2),
        .kitty_flags = @as(u8, raw_kitty_flags),
        .mouse_event = @intFromEnum(t.flags.mouse_event),
        .mouse_format = @intFromEnum(t.flags.mouse_format),
        .bracketed_paste = @intFromBool(t.modes.get(.bracketed_paste)),
        .focus_event_mode = @intFromBool(t.modes.get(.focus_event)),
    };
}

/// Encode a key event using pre-captured input opts. No terminal handle needed.
export fn ghostty_vt_encode_key(
    opts: InputOptsC,
    key_val: c_int,
    mods: u16,
    action: u8,
    text_ptr: ?[*]const u8,
    text_len: usize,
    unshifted_codepoint: u32,
    buf: ?[*]u8,
    buf_len: usize,
) callconv(.c) usize {
    if (buf == null) return 0;

    const kitty_flags: KittyFlags = @bitCast(@as(u5, @truncate(opts.kitty_flags)));
    const encode_opts: key_encode.Options = .{
        .cursor_key_application = opts.cursor_key_application != 0,
        .keypad_key_application = opts.keypad_key_application != 0,
        .ignore_keypad_with_numlock = opts.ignore_keypad_with_numlock != 0,
        .alt_esc_prefix = opts.alt_esc_prefix != 0,
        .modify_other_keys_state_2 = opts.modify_other_keys_state_2 != 0,
        .kitty_flags = kitty_flags,
    };

    const event: input_key.KeyEvent = .{
        .key = @enumFromInt(key_val),
        .mods = @bitCast(mods),
        .action = @enumFromInt(action),
        .utf8 = if (text_ptr) |p| p[0..text_len] else "",
        .unshifted_codepoint = @truncate(unshifted_codepoint),
    };

    var writer: std.Io.Writer = .fixed(buf.?[0..buf_len]);
    key_encode.encode(&writer, event, encode_opts) catch return 0;
    return writer.end;
}

/// Encode a mouse event using pre-captured input opts. No terminal handle needed.
export fn ghostty_vt_encode_mouse(
    opts: InputOptsC,
    button: u8,
    action: u8,
    mods: u8,
    x: u16,
    y: u16,
    buf: ?[*]u8,
    buf_len: usize,
) callconv(.c) usize {
    if (buf == null) return 0;

    // If mouse reporting is disabled, don't encode anything.
    // This prevents flooding the shell with mouse escape sequences
    // when mouse reporting mode is off (the default).
    const mouse_event: Terminal.MouseEvents = @enumFromInt(opts.mouse_event);
    if (mouse_event == .none) return 0;

    const mouse_format: Terminal.MouseFormat = @enumFromInt(opts.mouse_format);

    var button_code: u8 = button;
    if (action == 1 and mouse_format != .sgr and mouse_format != .sgr_pixels) {
        button_code = 3;
    }

    if (mouse_event != .x10) {
        if (mods & 1 != 0) button_code += 4; // shift
        if (mods & 2 != 0) button_code += 8; // alt
        if (mods & 4 != 0) button_code += 16; // ctrl
    }

    if (action == 2) button_code += 32;

    var fbs = std.io.fixedBufferStream(buf.?[0..buf_len]);
    const writer = fbs.writer();

    switch (mouse_format) {
        .x10 => {
            if (x > 222 or y > 222) return 0;
            writer.writeAll("\x1b[M") catch return 0;
            writer.writeByte(32 + button_code) catch return 0;
            writer.writeByte(32 + @as(u8, @intCast(x)) + 1) catch return 0;
            writer.writeByte(32 + @as(u8, @intCast(y)) + 1) catch return 0;
        },
        .utf8 => {
            writer.writeAll("\x1b[M") catch return 0;
            writer.writeByte(32 + button_code) catch return 0;
            var tmp: [4]u8 = undefined;
            var n = std.unicode.utf8Encode(@intCast(32 + x + 1), &tmp) catch return 0;
            writer.writeAll(tmp[0..n]) catch return 0;
            n = std.unicode.utf8Encode(@intCast(32 + y + 1), &tmp) catch return 0;
            writer.writeAll(tmp[0..n]) catch return 0;
        },
        .sgr => {
            const final: u8 = if (action == 1) 'm' else 'M';
            std.fmt.format(writer.any(), "\x1b[<{d};{d};{d}{c}", .{
                button_code,
                @as(u32, x) + 1,
                @as(u32, y) + 1,
                final,
            }) catch return 0;
        },
        .urxvt => {
            std.fmt.format(writer.any(), "\x1b[{d};{d};{d}M", .{
                @as(u16, 32) + button_code,
                @as(u32, x) + 1,
                @as(u32, y) + 1,
            }) catch return 0;
        },
        .sgr_pixels => {
            const final: u8 = if (action == 1) 'm' else 'M';
            std.fmt.format(writer.any(), "\x1b[<{d};{d};{d}{c}", .{
                button_code,
                @as(u32, x) + 1,
                @as(u32, y) + 1,
                final,
            }) catch return 0;
        },
    }

    return fbs.pos;
}

/// Resolve a W3C key code string to a Ghostty Key enum integer.
/// Returns -1 if the code is not recognized (maps to "unidentified").
/// This avoids hardcoding Key enum values in Rust.
///
/// Example W3C codes: "KeyA", "Enter", "ArrowLeft", "F1", "Digit0",
///                    "Space", "Tab", "Escape", "Backspace", "Delete"
/// See: https://www.w3.org/TR/uievents-code
export fn ghostty_vt_key_from_w3c(
    code_ptr: ?[*]const u8,
    code_len: usize,
) callconv(.c) c_int {
    if (code_ptr == null or code_len == 0) return -1;
    const code = code_ptr.?[0..code_len];
    const key = input_key.Key.fromW3C(code) orelse return -1;
    return @intFromEnum(key);
}
