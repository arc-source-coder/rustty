const std = @import("std");
const harness = @import("harness.zig");
const support = @import("support.zig");
pub const lib = @import("../ghostty_shim.zig");

const input = support.input;

fn test_encode_key_basic() !void {
    const handle = try support.initHandle();
    defer handle.deinit();

    var buf: [128]u8 = undefined;
    // Key.enter = 58 in Ghostty's key enum.
    const len = input.ghostty_terminal_encode_key(
        @ptrCast(handle),
        58,
        .press,
        @bitCast(@as(u16, 0)),
        @bitCast(@as(u16, 0)),
        null,
        0,
        0,
        &buf,
        buf.len,
    );
    try std.testing.expect(len > 0);
    try std.testing.expectEqual(@as(u8, '\r'), buf[0]);
}

fn test_encode_mouse_in_sgr_mode() !void {
    const handle = try support.initHandle();
    defer handle.deinit();

    // CSI ?1003h => any-event mouse tracking
    // CSI ?1006h => SGR mouse format
    support.feed(handle, "\x1b[?1003h\x1b[?1006h");

    // Set renderer dimensions for viewport detection when encoding mouse events.
    // 80 cells * 8px = 640px screen width.
    // 24 cells * 16px = 384px screen height.
    lib.ghostty_terminal_set_render_dimensions(handle, 640, 384, 8, 16);

    var buf: [64]u8 = undefined;
    const press_len = input.ghostty_terminal_encode_mouse(
        @ptrCast(handle),
        1,
        .press,
        @bitCast(@as(u16, 0)),
        40, // 5 * 8px
        160, // 10 * 16px
        &buf,
        buf.len,
    );
    try std.testing.expectEqualStrings("\x1b[<0;6;11M", buf[0..press_len]);

    const release_len = input.ghostty_terminal_encode_mouse(
        @ptrCast(handle),
        1,
        .release,
        @bitCast(@as(u16, 0)),
        40, // 5 * 8px
        160, // 10 * 16px
        &buf,
        buf.len,
    );
    try std.testing.expectEqualStrings("\x1b[<0;6;11m", buf[0..release_len]);
}

fn test_encode_mouse_in_x10_mode() !void {
    const handle = try support.initHandle();
    defer handle.deinit();

    // CSI ?9h => X10 mouse mode.
    support.feed(handle, "\x1b[?9h");

    // Set renderer dimensions for viewport detection when encoding mouse events.
    // 80 cells * 8px = 640px screen width.
    // 24 cells * 16px = 384px screen height.
    lib.ghostty_terminal_set_render_dimensions(handle, 640, 384, 8, 16);

    var buf: [64]u8 = undefined;
    const len = input.ghostty_terminal_encode_mouse(
        @ptrCast(handle),
        1,
        .press,
        @bitCast(@as(u16, 0)),
        0,
        0,
        &buf,
        buf.len,
    );

    try std.testing.expectEqual(@as(usize, 6), len);
    try std.testing.expectEqualSlices(u8, "\x1b[M", buf[0..3]);
    try std.testing.expectEqual(@as(u8, 32), buf[3]);
    try std.testing.expectEqual(@as(u8, 33), buf[4]);
    try std.testing.expectEqual(@as(u8, 33), buf[5]);
}

fn test_encode_mouse_wheel_in_sgr_mode() !void {
    const handle = try support.initHandle();
    defer handle.deinit();

    support.feed(handle, "\x1b[?1003h\x1b[?1006h");

    // Set renderer dimensions for viewport detection when encoding mouse events.
    // 80 cells * 8px = 640px screen width.
    // 24 cells * 16px = 384px screen height.
    lib.ghostty_terminal_set_render_dimensions(handle, 640, 384, 8, 16);

    var buf: [64]u8 = undefined;
    const len = input.ghostty_terminal_encode_mouse(
        @ptrCast(handle),
        4,
        .press,
        @bitCast(@as(u16, 0)),
        40, // 5 * 8px
        160, // 10 * 16px
        &buf,
        buf.len,
    );
    try std.testing.expectEqualStrings("\x1b[<64;6;11M", buf[0..len]);
}

fn test_encode_mouse_wheel_in_x10_mode_is_dropped() !void {
    const handle = try support.initHandle();
    defer handle.deinit();

    support.feed(handle, "\x1b[?9h");

    var buf: [64]u8 = undefined;
    const len = input.ghostty_terminal_encode_mouse(
        @ptrCast(handle),
        4,
        .press,
        @bitCast(@as(u16, 0)),
        5,
        10,
        &buf,
        buf.len,
    );
    try std.testing.expectEqual(@as(usize, 0), len);
}

fn test_encode_paste_bracketed() !void {
    const handle = try support.initHandle();
    defer handle.deinit();

    // CSI ?2004h enables bracketed paste wrapping.
    support.feed(handle, "\x1b[?2004h");

    var buf: [64]u8 = undefined;
    const bracketed_len = input.ghostty_terminal_encode_paste(
        @ptrCast(handle),
        "hello".ptr,
        5,
        &buf,
        buf.len,
    );
    try std.testing.expectEqualStrings("\x1b[200~hello\x1b[201~", buf[0..bracketed_len]);
}

fn test_encode_paste_empty() !void {
    const handle = try support.initHandle();
    defer handle.deinit();

    var buf: [64]u8 = undefined;
    const empty_len = input.ghostty_terminal_encode_paste(@ptrCast(handle), null, 0, &buf, buf.len);
    try std.testing.expectEqual(@as(usize, 0), empty_len);
}

pub fn run(suite: *harness.Suite) !void {
    try suite.run("input: test_encode_key_basic", test_encode_key_basic);
    try suite.run("input: test_encode_mouse_sgr", test_encode_mouse_in_sgr_mode);
    try suite.run("input: test_encode_mouse_x10", test_encode_mouse_in_x10_mode);
    try suite.run("input: test_encode_mouse_wheel_sgr", test_encode_mouse_wheel_in_sgr_mode);
    try suite.run("input: test_encode_mouse_wheel_x10", test_encode_mouse_wheel_in_x10_mode_is_dropped);
    try suite.run("input: test_encode_paste_bracketed", test_encode_paste_bracketed);
    try suite.run("input: test_encode_paste_empty", test_encode_paste_empty);
}
