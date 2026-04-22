const std = @import("std");
const harness = @import("harness.zig");
const support = @import("support.zig");

const lib = support.lib;

const CallbackState = struct {
    bell_count: usize = 0,
    output_count: usize = 0,
    title: std.ArrayList(u8) = .empty,
    event_log: std.ArrayList(u8) = .empty,

    fn deinit(self: *CallbackState) void {
        self.title.deinit(support.allocator());
        self.event_log.deinit(support.allocator());
        self.* = undefined;
    }
};

fn bellCallback(userdata: ?*anyopaque) callconv(.c) void {
    const state: *CallbackState = @ptrCast(@alignCast(userdata.?));
    state.bell_count += 1;
    state.event_log.append(support.allocator(), 'B') catch unreachable;
}

fn titleCallback(userdata: ?*anyopaque, ptr: [*]const u8, len: usize) callconv(.c) void {
    const state: *CallbackState = @ptrCast(@alignCast(userdata.?));
    state.title.clearRetainingCapacity();
    state.title.appendSlice(support.allocator(), ptr[0..len]) catch unreachable;
    state.event_log.append(support.allocator(), 'T') catch unreachable;
}

fn outputCallback(userdata: ?*anyopaque) callconv(.c) void {
    const state: *CallbackState = @ptrCast(@alignCast(userdata.?));
    state.output_count += 1;
    state.event_log.append(support.allocator(), 'O') catch unreachable;
}

fn test_new_free() !void {
    const ptr = lib.ghostty_terminal_new(80, 24, 0xDD, 0xDD, 0xDD, 0x1E, 0x1E, 0x2E);
    try std.testing.expect(ptr != null);
    lib.ghostty_terminal_free(ptr.?);
}

fn test_resize() !void {
    const handle = try support.initHandle();
    defer handle.deinit();

    try support.expectOk(lib.ghostty_terminal_resize(handle, 120, 40));
}

fn test_bell_callback() !void {
    const handle = try support.initHandle();
    defer handle.deinit();

    var callback_state: CallbackState = .{};
    defer callback_state.deinit();

    lib.ghostty_terminal_set_callbacks(
        handle,
        &callback_state,
        &bellCallback,
        &titleCallback,
        null,
    );

    // BEL (0x07) should invoke the bell callback once.
    support.feed(handle, "\x07");
    try std.testing.expectEqual(@as(usize, 1), callback_state.bell_count);
    try std.testing.expectEqualStrings("B", callback_state.event_log.items);
}

fn test_title_callback() !void {
    const handle = try support.initHandle();
    defer handle.deinit();

    var callback_state: CallbackState = .{};
    defer callback_state.deinit();

    lib.ghostty_terminal_set_callbacks(
        handle,
        &callback_state,
        &bellCallback,
        &titleCallback,
        null,
    );

    // OSC 0 ; <title> ST — set window title.
    support.feed(handle, "\x1b]0;shim-title\x1b\\");
    try std.testing.expectEqualStrings("shim-title", callback_state.title.items);
    try std.testing.expectEqualStrings("T", callback_state.event_log.items);
}

fn test_multiple_events_in_one_feed() !void {
    const handle = try support.initHandle();
    defer handle.deinit();

    var callback_state: CallbackState = .{};
    defer callback_state.deinit();

    lib.ghostty_terminal_set_callbacks(
        handle,
        &callback_state,
        &bellCallback,
        &titleCallback,
        null,
    );

    // Two BELs + OSC 0 title change in a single feed call.
    support.feed(handle, "\x07\x07\x1b]0;Title\x1b\\");
    try std.testing.expectEqual(@as(usize, 2), callback_state.bell_count);
    try std.testing.expectEqualStrings("Title", callback_state.title.items);
    try std.testing.expectEqualStrings("BBT", callback_state.event_log.items);
}

fn test_output_callback_fires_once_per_non_empty_feed() !void {
    const handle = try support.initHandle();
    defer handle.deinit();

    var callback_state: CallbackState = .{};
    defer callback_state.deinit();

    lib.ghostty_terminal_set_callbacks(
        handle,
        &callback_state,
        null,
        null,
        &outputCallback,
    );

    support.feed(handle, "hello");
    try std.testing.expectEqual(@as(usize, 1), callback_state.output_count);
    try std.testing.expectEqualStrings("O", callback_state.event_log.items);
}

pub fn run(suite: *harness.Suite) !void {
    try suite.run("core: test_new_free", test_new_free);
    try suite.run("core: test_resize", test_resize);
    try suite.run("core: test_bell_callback", test_bell_callback);
    try suite.run("core: test_title_callback", test_title_callback);
    try suite.run("core: test_multiple_events_in_one_feed", test_multiple_events_in_one_feed);
    try suite.run(
        "core: test_output_callback_fires_once_per_non_empty_feed",
        test_output_callback_fires_once_per_non_empty_feed,
    );
}
