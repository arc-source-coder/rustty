const std = @import("std");
const harness = @import("harness.zig");
const support = @import("support.zig");
const render = @import("../src/render.zig");

const root = support.render;

fn rawCellCodepoint(raw: u64) u21 {
    // page.Cell stores content at bit offset 2.
    return @truncate(raw >> 2);
}

fn rawCellContentTag(raw: u64) u2 {
    // content_tag occupies the lowest two bits.
    return @truncate(raw);
}

fn rawCellStyleId(raw: u64) u16 {
    // style_id starts at bit 26 in page.Cell.
    return @truncate(raw >> 26);
}

fn rawCellWide(raw: u64) u2 {
    // wide flag starts at bit 42 in page.Cell.
    return @truncate(raw >> 42);
}

fn test_render_update_empty() !void {
    const handle = try support.initHandle();
    defer handle.deinit();

    try support.renderUpdate(handle);
    try std.testing.expectEqual(@as(u8, 2), root.ghostty_terminal_render_dirty(handle));
    try std.testing.expectEqual(@as(u16, 24), root.ghostty_terminal_render_rows(handle));
    try std.testing.expectEqual(@as(u16, 80), root.ghostty_terminal_render_cols(handle));

    root.ghostty_terminal_render_clear_dirty(handle);
    try std.testing.expectEqual(@as(u8, 0), root.ghostty_terminal_render_dirty(handle));
}

fn test_render_partial_dirty() !void {
    const handle = try support.initHandle();
    defer handle.deinit();

    // Initial frame starts full-dirty; clear to observe incremental dirtiness.
    try support.renderUpdate(handle);
    root.ghostty_terminal_render_clear_dirty(handle);

    support.feed(handle, "Hello");
    try support.renderUpdate(handle);

    try std.testing.expect(root.ghostty_terminal_render_dirty(handle) > 0);
    try std.testing.expect(root.ghostty_terminal_render_row_dirty(handle, 0));
}

fn test_render_cursor() !void {
    const handle = try support.initHandle();
    defer handle.deinit();

    try support.renderUpdate(handle);

    var cursor: render.CursorState = undefined;
    root.ghostty_terminal_render_cursor(handle, &cursor);
    try std.testing.expectEqual(@as(u16, 0), cursor.x);
    try std.testing.expectEqual(@as(u16, 0), cursor.y);
    try std.testing.expectEqual(@as(u8, 1), cursor.in_viewport);
    try std.testing.expectEqual(@as(u8, 1), cursor.visible);
    try std.testing.expectEqual(@as(u8, 1), cursor.style);
}

fn test_render_row_raw() !void {
    const handle = try support.initHandle();
    defer handle.deinit();

    support.feed(handle, "ABC");
    try support.renderUpdate(handle);

    var cell_len: u16 = 0;
    const cells = root.ghostty_terminal_render_row_raw(handle, 0, &cell_len) orelse unreachable;
    try std.testing.expectEqual(@as(u16, 80), cell_len);
    try std.testing.expectEqual(@as(u21, 'A'), rawCellCodepoint(cells[0]));
    try std.testing.expectEqual(@as(u21, 'B'), rawCellCodepoint(cells[1]));
    try std.testing.expectEqual(@as(u21, 'C'), rawCellCodepoint(cells[2]));
    try std.testing.expectEqual(@as(u2, 0), rawCellContentTag(cells[0]));
    try std.testing.expectEqual(@as(u2, 0), rawCellWide(cells[0]));
}

fn test_render_row_styles() !void {
    const handle = try support.initHandle();
    defer handle.deinit();

    // SGR 1 (bold) + SGR 31 (red fg) + "X"
    support.feed(handle, "\x1b[1;31mX");
    try support.renderUpdate(handle);

    var cell_len: u16 = 0;
    const cells = root.ghostty_terminal_render_row_raw(handle, 0, &cell_len) orelse unreachable;
    try std.testing.expectEqual(@as(u16, 80), cell_len);
    try std.testing.expectEqual(@as(u21, 'X'), rawCellCodepoint(cells[0]));
    try std.testing.expect(rawCellStyleId(cells[0]) != 0);

    var style_len: u16 = 0;
    const styles = root.ghostty_terminal_render_row_styles(handle, 0, &style_len) orelse unreachable;
    try std.testing.expect(style_len > 0);
    try std.testing.expect((styles[0].flags & 1) != 0);
    try std.testing.expectEqual(@as(u8, 1), styles[0].fg_color.tag);
    try std.testing.expectEqual(@as(u8, 1), styles[0].fg_color.r);
}

fn test_render_row_background_styled() !void {
    const handle = try support.initHandle();
    defer handle.deinit();

    // SGR 48;5;196 => palette background color index 196.
    support.feed(handle, "\x1b[48;5;196mX");
    try support.renderUpdate(handle);

    var cell_len: u16 = 0;
    const cells = root.ghostty_terminal_render_row_raw(handle, 0, &cell_len) orelse unreachable;
    try std.testing.expectEqual(@as(u21, 'X'), rawCellCodepoint(cells[0]));
    try std.testing.expect(rawCellStyleId(cells[0]) != 0);

    var style_len: u16 = 0;
    const styles = root.ghostty_terminal_render_row_styles(handle, 0, &style_len) orelse unreachable;
    try std.testing.expect(style_len > 0);
    try std.testing.expectEqual(@as(u8, 1), styles[0].bg_color.tag);
    try std.testing.expectEqual(@as(u8, 196), styles[0].bg_color.r);
}

fn test_render_palette_batch() !void {
    const handle = try support.initHandle();
    defer handle.deinit();

    try support.renderUpdate(handle);
    const colors = root.ghostty_terminal_render_colors(handle);
    const red = colors.palette[1];
    try std.testing.expectEqual(@as(u8, 204), @as(u8, @truncate(red)));
    try std.testing.expectEqual(@as(u8, 102), @as(u8, @truncate(red >> 8)));
    try std.testing.expectEqual(@as(u8, 102), @as(u8, @truncate(red >> 16)));
}

fn test_render_row_selection_none() !void {
    const handle = try support.initHandle();
    defer handle.deinit();

    try support.renderUpdate(handle);

    var start_x: u16 = 99;
    var end_x: u16 = 99;
    try std.testing.expect(
        !root.ghostty_terminal_render_row_selection(handle, 0, &start_x, &end_x),
    );
}

fn test_multiple_row_raw_calls_are_independent() !void {
    const handle = try support.initHandle();
    defer handle.deinit();

    support.feed(handle, "Row0\r\nRow1");
    try support.renderUpdate(handle);

    var row0_len: u16 = 0;
    var row1_len: u16 = 0;
    const row0 = root.ghostty_terminal_render_row_raw(handle, 0, &row0_len) orelse unreachable;
    const row1 = root.ghostty_terminal_render_row_raw(handle, 1, &row1_len) orelse unreachable;

    try std.testing.expectEqual(@as(u21, 'R'), rawCellCodepoint(row0[0]));
    try std.testing.expectEqual(@as(u21, '0'), rawCellCodepoint(row0[3]));
    try std.testing.expectEqual(@as(u21, 'R'), rawCellCodepoint(row1[0]));
    try std.testing.expectEqual(@as(u21, '1'), rawCellCodepoint(row1[3]));
}

fn test_render_row_graphemes_stay_available_for_combining_characters() !void {
    const handle = try support.initHandle();
    defer handle.deinit();

    // "x" + COMBINING ACUTE ACCENT should produce grapheme backing data.
    support.feed(handle, "x\u{0301}");
    try support.renderUpdate(handle);

    var cell_len: u16 = 0;
    const cells = root.ghostty_terminal_render_row_raw(handle, 0, &cell_len) orelse unreachable;
    try std.testing.expectEqual(@as(u2, 1), rawCellContentTag(cells[0]));

    var grapheme_len: u16 = 0;
    const grapheme_ptr = root.ghostty_terminal_render_row_graphemes(handle, 0, &grapheme_len) orelse unreachable;
    try std.testing.expectEqual(cell_len, grapheme_len);
    try std.testing.expect(grapheme_ptr[0].ptr != null);
    try std.testing.expect(grapheme_ptr[0].len > 0);
}

fn test_render_row_access_returns_null_when_out_of_bounds() !void {
    const handle = try support.initHandle();
    defer handle.deinit();

    try support.renderUpdate(handle);

    var row_len: u16 = 0;
    try std.testing.expect(root.ghostty_terminal_render_row_raw(handle, 100, &row_len) == null);
    try std.testing.expect(root.ghostty_terminal_render_row_graphemes(handle, 100, &row_len) == null);
}

pub fn run(suite: *harness.Suite) !void {
    try suite.run("render: test_render_update_empty", test_render_update_empty);
    try suite.run("render: test_render_partial_dirty", test_render_partial_dirty);
    try suite.run("render: test_render_cursor", test_render_cursor);
    try suite.run("render: test_render_row_raw", test_render_row_raw);
    try suite.run("render: test_render_row_styles", test_render_row_styles);
    try suite.run("render: test_render_row_background_styled", test_render_row_background_styled);
    try suite.run("render: test_render_palette_batch", test_render_palette_batch);
    try suite.run("render: test_render_row_selection_none", test_render_row_selection_none);
    try suite.run(
        "render: test_multiple_row_raw_calls_are_independent",
        test_multiple_row_raw_calls_are_independent,
    );
    try suite.run(
        "render: test_render_row_graphemes_stay_available_for_combining_characters",
        test_render_row_graphemes_stay_available_for_combining_characters,
    );
    try suite.run(
        "render: test_render_row_access_returns_null_when_out_of_bounds",
        test_render_row_access_returns_null_when_out_of_bounds,
    );
}
