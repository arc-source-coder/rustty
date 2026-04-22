const std = @import("std");
const harness = @import("harness.zig");
const support = @import("support.zig");

const lib = support.lib;

const ResponseState = struct {
    bytes: std.ArrayList(u8) = .empty,

    fn deinit(self: *ResponseState) void {
        self.bytes.deinit(support.allocator());
        self.* = undefined;
    }

    fn clear(self: *ResponseState) void {
        self.bytes.clearRetainingCapacity();
    }
};

fn writeInputCallback(userdata: ?*anyopaque, ptr: [*]const u8, len: usize) callconv(.c) void {
    const state: *ResponseState = @ptrCast(@alignCast(userdata.?));
    // This mirrors the future VT input-slot handoff: Ghostty emits device
    // response bytes through write_pty and the host copies them into its own
    // input-side storage immediately.
    state.bytes.appendSlice(support.allocator(), ptr[0..len]) catch unreachable;
}

fn expectResponse(handle: *support.TerminalHandle, state: *ResponseState, query: []const u8, expected: []const u8) !void {
    state.clear();
    support.feed(handle, query);
    try std.testing.expectEqualStrings(expected, state.bytes.items);
}

fn test_device_responses_flow_to_write_input_callback() !void {
    const handle = try support.initHandle();
    defer handle.deinit();

    var state: ResponseState = .{};
    defer state.deinit();

    lib.ghostty_terminal_set_write_input(handle, &state, &writeInputCallback);

    support.feed(handle, "\x1b[c");
    try std.testing.expectEqualStrings("\x1b[?62;22c", state.bytes.items);
}

fn test_da1_primary_response() !void {
    const handle = try support.initHandle();
    defer handle.deinit();

    var state: ResponseState = .{};
    defer state.deinit();

    // DA1 query: ESC [ c
    lib.ghostty_terminal_set_write_input(handle, &state, &writeInputCallback);
    try expectResponse(handle, &state, "\x1b[c", "\x1b[?62;22c");
}

fn test_da2_secondary_response() !void {
    const handle = try support.initHandle();
    defer handle.deinit();

    var state: ResponseState = .{};
    defer state.deinit();

    // DA2 query: ESC [ > c
    lib.ghostty_terminal_set_write_input(handle, &state, &writeInputCallback);
    try expectResponse(handle, &state, "\x1b[>c", "\x1b[>1;0;0c");
}

fn test_dsr_operating_status() !void {
    const handle = try support.initHandle();
    defer handle.deinit();

    var state: ResponseState = .{};
    defer state.deinit();

    // DSR operating status query: ESC [ 5 n
    lib.ghostty_terminal_set_write_input(handle, &state, &writeInputCallback);
    try expectResponse(handle, &state, "\x1b[5n", "\x1b[0n");
}

fn test_kitty_keyboard_query() !void {
    const handle = try support.initHandle();
    defer handle.deinit();

    var state: ResponseState = .{};
    defer state.deinit();

    // Kitty keyboard protocol query: ESC [ ? u
    lib.ghostty_terminal_set_write_input(handle, &state, &writeInputCallback);
    try expectResponse(handle, &state, "\x1b[?u", "\x1b[?0u");
}

fn test_dsr_cursor_position() !void {
    const handle = try support.initHandle();
    defer handle.deinit();

    var state: ResponseState = .{};
    defer state.deinit();

    // Move cursor to row 5, col 10 (1-indexed): CSI 5;10 H
    lib.ghostty_terminal_set_write_input(handle, &state, &writeInputCallback);
    support.feed(handle, "\x1b[5;10H");
    // Query cursor position: CSI 6 n
    try expectResponse(handle, &state, "\x1b[6n", "\x1b[5;10R");
}

fn test_size_report_csi_18t() !void {
    const handle = try support.initHandle();
    defer handle.deinit();

    var state: ResponseState = .{};
    defer state.deinit();

    // Provide cell metrics so XTWINOPS size reports can be computed.
    lib.ghostty_terminal_set_write_input(handle, &state, &writeInputCallback);
    // 80 cells * 8px = 640px screen width.
    // 24 cells * 16px = 384px screen height.
    lib.ghostty_terminal_set_render_dimensions(handle, 640, 384, 8, 16);
    // Query grid size: CSI 18 t
    try expectResponse(handle, &state, "\x1b[18t", "\x1b[8;24;80t");
}

fn test_size_report_csi_14t_with_cell_size() !void {
    const handle = try support.initHandle();
    defer handle.deinit();

    var state: ResponseState = .{};
    defer state.deinit();

    lib.ghostty_terminal_set_write_input(handle, &state, &writeInputCallback);
    // 80 cells * 8px = 640px screen width.
    // 24 cells * 16px = 384px screen height.
    lib.ghostty_terminal_set_render_dimensions(handle, 640, 384, 8, 16);
    // Query text area pixel size: CSI 14 t
    try expectResponse(handle, &state, "\x1b[14t", "\x1b[4;384;640t");
}

fn test_size_report_csi_14t_without_cell_size_stays_quiet() !void {
    const handle = try support.initHandle();
    defer handle.deinit();

    var state: ResponseState = .{};
    defer state.deinit();

    lib.ghostty_terminal_set_write_input(handle, &state, &writeInputCallback);
    // Without cell metrics, CSI 14 t should not emit a response.
    try expectResponse(handle, &state, "\x1b[14t", "");
}

fn test_multiple_responses_preserve_order_within_one_feed() !void {
    const handle = try support.initHandle();
    defer handle.deinit();

    var state: ResponseState = .{};
    defer state.deinit();

    // DA1 + DSR in one feed should preserve write order.
    lib.ghostty_terminal_set_write_input(handle, &state, &writeInputCallback);
    try expectResponse(handle, &state, "\x1b[c\x1b[5n", "\x1b[?62;22c\x1b[0n");
}

fn test_split_escape_sequence_across_feeds_produces_single_response() !void {
    const handle = try support.initHandle();
    defer handle.deinit();

    var state: ResponseState = .{};
    defer state.deinit();

    lib.ghostty_terminal_set_write_input(handle, &state, &writeInputCallback);

    // Feed DA1 query in two chunks: "ESC [" then "c".
    support.feed(handle, "\x1b[");
    try std.testing.expectEqual(@as(usize, 0), state.bytes.items.len);

    support.feed(handle, "c");
    try std.testing.expectEqualStrings("\x1b[?62;22c", state.bytes.items);
}

pub fn run(suite: *harness.Suite) !void {
    try suite.run(
        "device_response: test_device_responses_flow_to_write_input_callback",
        test_device_responses_flow_to_write_input_callback,
    );
    try suite.run("device_response: test_da1_primary_response", test_da1_primary_response);
    try suite.run("device_response: test_da2_secondary_response", test_da2_secondary_response);
    try suite.run("device_response: test_dsr_cursor_position", test_dsr_cursor_position);
    try suite.run("device_response: test_dsr_operating_status", test_dsr_operating_status);
    try suite.run("device_response: test_kitty_keyboard_query", test_kitty_keyboard_query);
    try suite.run("device_response: test_size_report_csi_18t", test_size_report_csi_18t);
    try suite.run(
        "device_response: test_size_report_csi_14t_with_cell_size",
        test_size_report_csi_14t_with_cell_size,
    );
    try suite.run(
        "device_response: test_size_report_csi_14t_without_cell_size_stays_quiet",
        test_size_report_csi_14t_without_cell_size_stays_quiet,
    );
    try suite.run(
        "device_response: test_multiple_responses_preserve_order_within_one_feed",
        test_multiple_responses_preserve_order_within_one_feed,
    );
    try suite.run(
        "device_response: test_split_escape_sequence_across_feeds_produces_single_response",
        test_split_escape_sequence_across_feeds_produces_single_response,
    );
}
