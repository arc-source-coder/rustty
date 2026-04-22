//! Input encoding: key events, mouse events, and mode flag snapshots.

const std = @import("std");

const handle_mod = @import("handle.zig");
const TerminalHandle = handle_mod.TerminalHandle;

const key_encode = @import("../ghostty/src/input/key_encode.zig");
const input_key = @import("../ghostty/src/input/key.zig");

const mouse_encode = @import("../ghostty/src/input/mouse_encode.zig");
const input_mouse = @import("../ghostty//src/input/mouse.zig");

const input_paste = @import("../ghostty/src/input/paste.zig");

/// Encode a key event based on current terminal state.
pub export fn ghostty_terminal_encode_key(
    ptr: *anyopaque,
    key_val: c_int,
    action: input_key.Action,
    mods: input_key.Mods,
    consumed_mods: input_key.Mods,
    text_ptr: ?[*]const u8,
    text_len: usize,
    unshifted_codepoint: u32,
    buf: ?[*]u8,
    buf_len: usize,
) callconv(.c) usize {
    if (buf == null) return 0;

    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));

    handle.lock();
    const encode_opts = key_encode.Options.fromTerminal(&handle.terminal_inst);
    handle.unlock();

    const event: input_key.KeyEvent = .{
        .key = @enumFromInt(key_val),
        .mods = mods,
        .consumed_mods = consumed_mods,
        .action = action,
        .utf8 = if (text_ptr) |p| p[0..text_len] else "",
        .unshifted_codepoint = @truncate(unshifted_codepoint),
    };

    var writer: std.Io.Writer = .fixed(buf.?[0..buf_len]);
    key_encode.encode(&writer, event, encode_opts) catch return 0;
    return writer.end;
}

/// Encode a mouse event based on current terminal state.
pub export fn ghostty_terminal_encode_mouse(
    ptr: *anyopaque,
    button: i8,
    action: input_mouse.Action,
    mods: input_key.Mods,
    x: f32,
    y: f32,
    buf: ?[*]u8,
    buf_len: usize,
) callconv(.c) usize {
    if (buf == null) return 0;

    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));
    handle.lock();
    const encode_opts = mouse_encode.Options.fromTerminal(&handle.terminal_inst, handle.size);
    handle.unlock();

    const event: mouse_encode.Event = .{
        .action = action,
        .button = if (button >= 0) @enumFromInt(@as(u8, @intCast(button))) else null,
        .mods = mods,
        .pos = .{
            .x = x,
            .y = y,
        },
    };

    var writer: std.Io.Writer = .fixed(buf.?[0..buf_len]);
    mouse_encode.encode(&writer, event, encode_opts) catch return 0;
    return writer.end;
}

/// Encode paste bytes using Ghostty's input/paste rules.
/// Applies bracketed paste fenceposts, strips unsafe control bytes,
/// and converts LF->CR in non-bracketed mode.
pub export fn ghostty_terminal_encode_paste(
    ptr: *anyopaque,
    text_ptr: ?[*]const u8,
    text_len: usize,
    buf: ?[*]u8,
    buf_len: usize,
) callconv(.c) usize {
    if (buf == null) return 0;
    const text: []const u8 = if (text_ptr == null or text_len == 0)
        ""
    else
        text_ptr.?[0..text_len];

    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));
    handle.lock();
    const encode_opts = input_paste.Options.fromTerminal(&handle.terminal_inst);
    handle.unlock();

    const parts_const = input_paste.encode(text, encode_opts) catch |err| switch (err) {
        // Only allocate when paste encoding needs mutable bytes.
        error.MutableRequired => {
            var stack = std.heap.stackFallback(4096, std.heap.smp_allocator);
            const alloc = stack.get();

            const mutable = alloc.dupe(u8, text) catch return 0;
            defer alloc.free(mutable);

            const parts = input_paste.encode(mutable, encode_opts);
            const total = parts[0].len + parts[1].len + parts[2].len;
            if (total > buf_len) return 0;

            const dst = buf.?[0..buf_len];
            @memcpy(dst[0..parts[0].len], parts[0]);
            @memcpy(dst[parts[0].len .. parts[0].len + parts[1].len], parts[1]);
            @memcpy(dst[parts[0].len + parts[1].len .. total], parts[2]);
            return total;
        },
    };

    const total = parts_const[0].len + parts_const[1].len + parts_const[2].len;
    if (total > buf_len) return 0;

    const dst = buf.?[0..buf_len];
    @memcpy(dst[0..parts_const[0].len], parts_const[0]);
    @memcpy(
        dst[parts_const[0].len .. parts_const[0].len + parts_const[1].len],
        parts_const[1],
    );
    @memcpy(dst[parts_const[0].len + parts_const[1].len .. total], parts_const[2]);
    return total;
}
