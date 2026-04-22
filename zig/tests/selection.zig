const std = @import("std");
const harness = @import("harness.zig");
const support = @import("support.zig");

const root = support.selection;
const render = support.render;

fn expectSelectionText(handle: *support.TerminalHandle, expected: []const u8) !void {
    var len: usize = 0;
    const ptr = root.ghostty_terminal_get_selection_text(handle, &len) orelse unreachable;
    defer root.ghostty_terminal_bytes_free(handle, ptr, len);
    try std.testing.expectEqualStrings(expected, ptr[0..len]);
}

fn expectNoSelectionText(handle: *support.TerminalHandle) !void {
    var len: usize = 0;
    try std.testing.expect(root.ghostty_terminal_get_selection_text(handle, &len) == null);
}

fn test_selection_set_clear() !void {
    const handle = try support.initHandle();
    defer handle.deinit();

    support.feed(handle, "Hello, World!");
    try support.expectOk(root.ghostty_terminal_set_selection(handle, 0, 0, 4, 0, 0));
    try support.renderUpdate(handle);

    var start_x: u16 = 99;
    var end_x: u16 = 99;
    try std.testing.expect(render.ghostty_terminal_render_row_selection(handle, 0, &start_x, &end_x));
    try std.testing.expectEqual(@as(u16, 0), start_x);
    try std.testing.expectEqual(@as(u16, 4), end_x);
    try expectSelectionText(handle, "Hello");

    root.ghostty_terminal_clear_selection(handle);
    try support.renderUpdate(handle);
    try std.testing.expect(!render.ghostty_terminal_render_row_selection(handle, 0, &start_x, &end_x));
}

fn test_selection_set_reverse_coordinates() !void {
    const handle = try support.initHandle();
    defer handle.deinit();

    support.feed(handle, "Hello, World!");
    // Select "Hello" with reversed endpoints.
    try support.expectOk(root.ghostty_terminal_set_selection(handle, 4, 0, 0, 0, 0));
    try support.renderUpdate(handle);

    var start_x: u16 = 99;
    var end_x: u16 = 99;
    try std.testing.expect(render.ghostty_terminal_render_row_selection(handle, 0, &start_x, &end_x));
    try expectSelectionText(handle, "Hello");
}

fn test_select_word_at_uses_ghostty_boundaries() !void {
    const handle = try support.initHandle();
    defer handle.deinit();

    // Comma and spaces should split this into: "foo", "bar", "baz".
    support.feed(handle, "foo,bar baz");

    try support.expectOk(root.ghostty_terminal_select_word_at(handle, 1, 0));
    try expectSelectionText(handle, "foo");

    try support.expectOk(root.ghostty_terminal_select_word_at(handle, 4, 0));
    try expectSelectionText(handle, "bar");

    try support.expectOk(root.ghostty_terminal_select_word_at(handle, 8, 0));
    try expectSelectionText(handle, "baz");
}

fn test_select_line_at_trims_whitespace_like_ghostty() !void {
    const handle = try support.initHandle();
    defer handle.deinit();

    // Ghostty line selection trims leading/trailing ASCII whitespace.
    support.feed(handle, "   hello world   \r\n\tsecond line\t");

    try support.expectOk(root.ghostty_terminal_select_line_at(handle, 0, 0));
    try expectSelectionText(handle, "hello world");

    try support.expectOk(root.ghostty_terminal_select_line_at(handle, 0, 1));
    try expectSelectionText(handle, "second line");
}

fn test_select_word_at_returns_false_for_unwritten_cells() !void {
    const handle = try support.initHandle();
    defer handle.deinit();

    support.feed(handle, "abc");
    try std.testing.expect(root.ghostty_terminal_select_word_at(handle, 10, 0) != 0);
}

fn test_select_word_drag_expands_by_word_boundaries() !void {
    const handle = try support.initHandle();
    defer handle.deinit();

    support.feed(handle, "foo bar baz");

    // Drag right from inside "bar" should include "bar baz".
    try support.expectOk(root.ghostty_terminal_select_word_drag(handle, 5, 0, 9, 0));
    try expectSelectionText(handle, "bar baz");

    // Drag left from inside "bar" should include "foo bar".
    try support.expectOk(root.ghostty_terminal_select_word_drag(handle, 5, 0, 1, 0));
    try expectSelectionText(handle, "foo bar");
}

fn test_select_line_drag_expands_by_lines() !void {
    const handle = try support.initHandle();
    defer handle.deinit();

    support.feed(handle, "line0\r\nline1\r\nline2");

    // Drag downward from line1 to line2.
    try support.expectOk(root.ghostty_terminal_select_line_drag(handle, 1, 1, 1, 2));
    try expectSelectionText(handle, "line1\nline2");

    // Drag upward from line1 to line0.
    try support.expectOk(root.ghostty_terminal_select_line_drag(handle, 1, 1, 1, 0));
    try expectSelectionText(handle, "line0\nline1");
}

fn test_select_output_at_without_semantic_markers_returns_false() !void {
    const handle = try support.initHandle();
    defer handle.deinit();

    support.feed(handle, "plain text");
    try std.testing.expect(root.ghostty_terminal_select_output_at(handle, 2, 0) != 0);
}

fn test_no_selection_returns_none() !void {
    const handle = try support.initHandle();
    defer handle.deinit();

    try expectNoSelectionText(handle);
}

pub fn run(suite: *harness.Suite) !void {
    try suite.run("selection: test_selection_set_clear", test_selection_set_clear);
    try suite.run("selection: test_selection_set_reverse_coordinates", test_selection_set_reverse_coordinates);
    try suite.run(
        "selection: test_select_word_at_uses_ghostty_boundaries",
        test_select_word_at_uses_ghostty_boundaries,
    );
    try suite.run(
        "selection: test_select_line_at_trims_whitespace_like_ghostty",
        test_select_line_at_trims_whitespace_like_ghostty,
    );
    try suite.run(
        "selection: test_select_word_at_returns_false_for_unwritten_cells",
        test_select_word_at_returns_false_for_unwritten_cells,
    );
    try suite.run(
        "selection: test_select_word_drag_expands_by_word_boundaries",
        test_select_word_drag_expands_by_word_boundaries,
    );
    try suite.run(
        "selection: test_select_line_drag_expands_by_lines",
        test_select_line_drag_expands_by_lines,
    );
    try suite.run(
        "selection: test_select_output_at_without_semantic_markers_returns_false",
        test_select_output_at_without_semantic_markers_returns_false,
    );
    try suite.run("selection: test_no_selection_returns_none", test_no_selection_returns_none);
}
