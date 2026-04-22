const std = @import("std");
const harness = @import("harness.zig");
const support = @import("support.zig");

const root = support.scroll;
const ScrollbarInfoC = support.scroll.ScrollbarInfoC;

fn test_scroll_viewport() !void {
    const handle = try support.initHandle();
    defer handle.deinit();

    // Feed enough newlines to generate scrollback rows.
    const newlines = try support.allocator().alloc(u8, 50);
    defer support.allocator().free(newlines);
    @memset(newlines, '\n');
    support.feed(handle, newlines);

    root.ghostty_terminal_scroll_viewport(handle, -5);
    root.ghostty_terminal_scroll_viewport_top(handle);
    root.ghostty_terminal_scroll_viewport_bottom(handle);
}

fn test_scrollbar_info_no_scrollback() !void {
    const handle = try support.initHandle();
    defer handle.deinit();

    var info: ScrollbarInfoC = undefined;
    support.scrollbarInfo(handle, &info);
    try std.testing.expectEqual(@as(u16, 24), info.viewport_rows);
    try std.testing.expect(info.total_rows >= info.viewport_rows);
    try std.testing.expect(root.ghostty_terminal_viewport_is_bottom(handle));
}

fn test_scrollbar_info_with_scrollback() !void {
    const handle = try support.initHandle();
    defer handle.deinit();

    // Feed enough lines to push content into scrollback.
    const newlines = try support.allocator().alloc(u8, 100);
    defer support.allocator().free(newlines);
    @memset(newlines, '\n');
    support.feed(handle, newlines);

    var info_before: ScrollbarInfoC = undefined;
    support.scrollbarInfo(handle, &info_before);
    try std.testing.expect(info_before.total_rows > info_before.viewport_rows);
    try std.testing.expect(root.ghostty_terminal_viewport_is_bottom(handle));

    root.ghostty_terminal_scroll_viewport(handle, -10);
    try std.testing.expect(!root.ghostty_terminal_viewport_is_bottom(handle));

    var info_after: ScrollbarInfoC = undefined;
    support.scrollbarInfo(handle, &info_after);
    try std.testing.expect(info_after.top_row < info_before.top_row);

    root.ghostty_terminal_scroll_viewport_bottom(handle);
    try std.testing.expect(root.ghostty_terminal_viewport_is_bottom(handle));
}

fn test_scroll_to_row() !void {
    const handle = try support.initHandle();
    defer handle.deinit();

    // Feed enough lines so row 0 is in scrollback history.
    const newlines = try support.allocator().alloc(u8, 100);
    defer support.allocator().free(newlines);
    @memset(newlines, '\n');
    support.feed(handle, newlines);

    root.ghostty_terminal_scroll_to_row(handle, 0);
    try std.testing.expect(!root.ghostty_terminal_viewport_is_bottom(handle));

    var info: ScrollbarInfoC = undefined;
    support.scrollbarInfo(handle, &info);
    try std.testing.expectEqual(@as(u64, 0), info.top_row);
}

pub fn run(suite: *harness.Suite) !void {
    try suite.run("scroll: test_scroll_viewport", test_scroll_viewport);
    try suite.run("scroll: test_scrollbar_info_no_scrollback", test_scrollbar_info_no_scrollback);
    try suite.run("scroll: test_scrollbar_info_with_scrollback", test_scrollbar_info_with_scrollback);
    try suite.run("scroll: test_scroll_to_row", test_scroll_to_row);
}
