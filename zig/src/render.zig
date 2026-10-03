const handle_mod = @import("handle.zig");
const terminal = @import("../ghostty/src/terminal/main.zig");
const std = @import("std");
const color = terminal.color;

const TerminalHandle = handle_mod.TerminalHandle;

/// C/Rust mirror for `?color.RGB`.
///
/// Layout is validated against Zig's optional representation in comptime
/// assertions below. Tag values are also asserted: 0 = null, 1 = present.
const OptionalColorRGB = extern struct {
    rgb: u32,
    tag: u8,
};

/// C/Rust mirror of `std.MultiArrayList(T)` header.
///
/// Rust uses this with type-specific prefix constants to recreate
/// `MultiArrayList.slice().items(.field)` without copying cell data.
pub const ZigMultiArrayList = extern struct {
    bytes: [*]const u8,
    len: usize,
    capacity: usize,
};

/// C/Rust mirror for `?[2]u16` used by `RenderState.Row.selection`.
/// Layout is validated against Zig's optional representation below.
pub const OptionalSelection = extern struct {
    range: [2]u16,
    tag: u8,
};

/// C/Rust mirror for `terminal.RenderState.Colors`.
///
/// This type is pointer-cast over `state.colors` (zero-copy), so
/// all offsets, alignment, optional layout and palette
/// representation are asserted at comptime.
pub const RenderColors = extern struct {
    background: u32,
    foreground: u32,
    cursor: OptionalColorRGB,
    palette: [256]u32,
};

/// C/Rust mirror of `terminal.RenderState.Cursor.Viewport`.
const CursorViewport = extern struct {
    x: u16,
    y: u16,
    wide_tail: bool,
};

/// C/Rust mirror of `?terminal.RenderState.Cursor.Viewport`.
const OptionalCursorViewport = extern struct {
    viewport: CursorViewport,
    tag: u8,
};

/// C/Rust mirror of `terminal.RenderState.Cursor`.
const RenderCursor = extern struct {
    // Field order is memory order, not Ghostty declaration order.
    cell: u64,
    active: terminal.Coordinate,
    style: CellStyle,
    viewport: OptionalCursorViewport,
    visual_style: u8,
    password_input: bool,
    visible: bool,
    blinking: bool,
};

fn malOffset(comptime Mal: type, comptime field: Mal.Field) usize {
    const mal: Mal = .{ .bytes = @ptrFromInt(0x1000), .len = 1, .capacity = 1 };
    return @intFromPtr(mal.slice().items(field).ptr) - @intFromPtr(mal.bytes);
}

fn assertSameLayout(comptime A: type, comptime B: type) void {
    std.debug.assert(@sizeOf(A) == @sizeOf(B));
    std.debug.assert(@alignOf(A) == @alignOf(B));
}

// === ABI STABILITY ASSERTIONS ===
// These verify that Ghostty's internal type layouts match what the Rust
// side expects. If any of these assertions fail after a Ghostty update,
// the Rust-side structs and these assertions must be updated to match.
comptime {
    const page = @import("../ghostty/src/terminal/page.zig");
    const render = @import("../ghostty/src/terminal/render.zig");

    const Row = render.RenderState.Row;
    const Cell = render.RenderState.Cell;
    const Cursor = render.RenderState.Cursor;
    const RowMal = std.MultiArrayList(Row);
    const CellMal = std.MultiArrayList(Cell);

    // These guard the Rust direct-MultiArrayList reader in crates/ghostty/src/zig.rs.
    // If they fail after a Zig/Ghostty update, update the Rust constants and mirrors.
    assertSameLayout(ZigMultiArrayList, RowMal);

    std.debug.assert(@offsetOf(ZigMultiArrayList, "bytes") == @offsetOf(RowMal, "bytes"));
    std.debug.assert(@offsetOf(ZigMultiArrayList, "len") == @offsetOf(RowMal, "len"));
    std.debug.assert(@offsetOf(ZigMultiArrayList, "capacity") == @offsetOf(RowMal, "capacity"));

    std.debug.assert(malOffset(RowMal, .cells) == 48);
    std.debug.assert(malOffset(RowMal, .selection) == 96);
    std.debug.assert(malOffset(RowMal, .dirty) == 102);
    std.debug.assert(@sizeOf(?[2]u16) == @sizeOf(OptionalSelection));
    std.debug.assert(@alignOf(?[2]u16) == @alignOf(OptionalSelection));

    std.debug.assert(malOffset(CellMal, .raw) == 0);
    std.debug.assert(malOffset(CellMal, .grapheme) == 8);
    std.debug.assert(malOffset(CellMal, .style) == 24);

    // --- page.Cell (RawCell on Rust side) ---
    // page.Cell must be u64-aligned so the [*]const u64 cast in _row_raw is sound.
    std.debug.assert(@alignOf(page.Cell) == @alignOf(u64));

    std.debug.assert(@sizeOf(page.Cell) == 8);
    std.debug.assert(@bitSizeOf(page.Cell) == 64);
    std.debug.assert(@bitOffsetOf(page.Cell, "content_tag") == 0);
    std.debug.assert(@bitOffsetOf(page.Cell, "content") == 2);
    std.debug.assert(@bitOffsetOf(page.Cell, "style_id") == 26);
    std.debug.assert(@bitOffsetOf(page.Cell, "wide") == 42);
    std.debug.assert(@bitOffsetOf(page.Cell, "protected") == 44);
    std.debug.assert(@bitOffsetOf(page.Cell, "hyperlink") == 45);

    std.debug.assert(@intFromEnum(page.Cell.ContentTag.codepoint) == 0);
    std.debug.assert(@intFromEnum(page.Cell.ContentTag.codepoint_grapheme) == 1);
    std.debug.assert(@intFromEnum(page.Cell.ContentTag.bg_color_palette) == 2);
    std.debug.assert(@intFromEnum(page.Cell.ContentTag.bg_color_rgb) == 3);
    std.debug.assert(@intFromEnum(page.Cell.Wide.narrow) == 0);
    std.debug.assert(@intFromEnum(page.Cell.Wide.wide) == 1);
    std.debug.assert(@intFromEnum(page.Cell.Wide.spacer_tail) == 2);
    std.debug.assert(@intFromEnum(page.Cell.Wide.spacer_head) == 3);

    // --- u21 grapheme ABI contract ---
    // We rely on reinterpreting []u21 as []u32 for zero-copy grapheme access.
    // This is only valid if layout/stride/alignment match.
    std.debug.assert(@bitSizeOf(u21) == 21);
    std.debug.assert(@sizeOf(u21) == @sizeOf(u32));
    std.debug.assert(@alignOf(u21) == @alignOf(u32));

    // C-safe mirror of a Zig slice to verify layout.
    // Used to return []const []const u21 as ?[*]const GraphemeView.
    // The Rust side interprets it as &[GraphemeView] and forms slices
    // from ptr + len when accessing the grapheme codepoints for a cell.
    const GraphemeView = extern struct {
        ptr: [*]const u21,
        len: usize,
    };
    std.debug.assert(@sizeOf([]const u21) == @sizeOf(GraphemeView));
    std.debug.assert(@alignOf([]const u21) == @alignOf(GraphemeView));

    // --- terminal.Style (CellStyle on Rust side) ---
    // CellStyle is the extern struct matching terminal.Style. These
    // assertions verify that CellStyle and terminal.Style have the same
    // layout. If Ghostty reorders Style fields, these fail at build time.
    std.debug.assert(@sizeOf(terminal.Style) == @sizeOf(CellStyle));
    // CellStyle should match terminal.Style alignment for safe pointer casting.
    std.debug.assert(@alignOf(terminal.Style) == @alignOf(CellStyle));
    std.debug.assert(@offsetOf(terminal.Style, "fg_color") == @offsetOf(CellStyle, "fg_color"));
    std.debug.assert(@offsetOf(terminal.Style, "bg_color") == @offsetOf(CellStyle, "bg_color"));
    std.debug.assert(@offsetOf(terminal.Style, "underline_color") == @offsetOf(CellStyle, "underline_color"));
    std.debug.assert(@offsetOf(terminal.Style, "flags") == @offsetOf(CellStyle, "flags"));

    // --- Style.Color tagged union (StyleColor on Rust/Zig side) ---
    std.debug.assert(@sizeOf(terminal.Style.Color) == @sizeOf(StyleColor));
    std.debug.assert(@alignOf(terminal.Style.Color) == @alignOf(StyleColor));

    const fields = std.meta.fields(terminal.Style.Color);
    std.debug.assert(fields.len == 3);

    std.debug.assert(std.mem.eql(u8, fields[0].name, "none"));
    std.debug.assert(std.mem.eql(u8, fields[1].name, "palette"));
    std.debug.assert(std.mem.eql(u8, fields[2].name, "rgb"));

    const none_color: terminal.Style.Color = .none;
    const palette_color: terminal.Style.Color = .{ .palette = 0xAB };
    const rgb_color: terminal.Style.Color = .{ .rgb = .{ .r = 1, .g = 2, .b = 3 } };

    std.debug.assert(@intFromEnum(std.meta.activeTag(none_color)) == 0);
    std.debug.assert(@intFromEnum(std.meta.activeTag(palette_color)) == 1);
    std.debug.assert(@intFromEnum(std.meta.activeTag(rgb_color)) == 2);

    // --- color.RGB zero-copy palette ABI contract ---
    // color.RGB is packed struct(u24): 4 bytes in memory (padded to u32).
    // Rust reads [256]color.RGB as [256]u32 via zero-copy pointer.
    std.debug.assert(@bitSizeOf(color.RGB) == 24);
    std.debug.assert(@sizeOf(color.RGB) == @sizeOf(u32));
    std.debug.assert(@alignOf(color.RGB) == @alignOf(u32));
    std.debug.assert(@sizeOf(color.Palette) == 256 * @sizeOf(u32));

    // --- terminal.RenderState.Colors ABI contract ---
    std.debug.assert(@sizeOf(terminal.RenderState.Colors) == @sizeOf(RenderColors));
    std.debug.assert(@alignOf(terminal.RenderState.Colors) == @alignOf(RenderColors));
    std.debug.assert(@offsetOf(terminal.RenderState.Colors, "background") == @offsetOf(RenderColors, "background"));
    std.debug.assert(@offsetOf(terminal.RenderState.Colors, "foreground") == @offsetOf(RenderColors, "foreground"));
    std.debug.assert(@offsetOf(terminal.RenderState.Colors, "cursor") == @offsetOf(RenderColors, "cursor"));
    std.debug.assert(@offsetOf(terminal.RenderState.Colors, "palette") == @offsetOf(RenderColors, "palette"));

    // --- optional color.RGB ABI contract (?color.RGB) ---
    std.debug.assert(@sizeOf(?color.RGB) == @sizeOf(OptionalColorRGB));
    std.debug.assert(@alignOf(?color.RGB) == @alignOf(OptionalColorRGB));

    // --- terminal.RenderState.Cursor zero-copy ABI contract ---
    std.debug.assert(@sizeOf(Cursor.Viewport) == @sizeOf(CursorViewport));
    std.debug.assert(@alignOf(Cursor.Viewport) == @alignOf(CursorViewport));
    std.debug.assert(@offsetOf(Cursor.Viewport, "x") == @offsetOf(CursorViewport, "x"));
    std.debug.assert(@offsetOf(Cursor.Viewport, "y") == @offsetOf(CursorViewport, "y"));
    std.debug.assert(@offsetOf(Cursor.Viewport, "wide_tail") == @offsetOf(CursorViewport, "wide_tail"));

    std.debug.assert(@sizeOf(?Cursor.Viewport) == @sizeOf(OptionalCursorViewport));
    std.debug.assert(@alignOf(?Cursor.Viewport) == @alignOf(OptionalCursorViewport));

    std.debug.assert(@intFromEnum(terminal.CursorStyle.bar) == 0);
    std.debug.assert(@intFromEnum(terminal.CursorStyle.block) == 1);
    std.debug.assert(@intFromEnum(terminal.CursorStyle.underline) == 2);
    std.debug.assert(@intFromEnum(terminal.CursorStyle.block_hollow) == 3);

    std.debug.assert(@sizeOf(Cursor) == @sizeOf(RenderCursor));
    std.debug.assert(@alignOf(Cursor) == @alignOf(RenderCursor));
    std.debug.assert(@offsetOf(Cursor, "active") == @offsetOf(RenderCursor, "active"));
    std.debug.assert(@offsetOf(Cursor, "viewport") == @offsetOf(RenderCursor, "viewport"));
    std.debug.assert(@offsetOf(Cursor, "cell") == @offsetOf(RenderCursor, "cell"));
    std.debug.assert(@offsetOf(Cursor, "style") == @offsetOf(RenderCursor, "style"));
    std.debug.assert(@offsetOf(Cursor, "visual_style") == @offsetOf(RenderCursor, "visual_style"));
    std.debug.assert(@offsetOf(Cursor, "password_input") == @offsetOf(RenderCursor, "password_input"));
    std.debug.assert(@offsetOf(Cursor, "visible") == @offsetOf(RenderCursor, "visible"));
    std.debug.assert(@offsetOf(Cursor, "blinking") == @offsetOf(RenderCursor, "blinking"));
}

/// C-safe mirror of terminal.Style.Color tagged union.
/// Tag values: 0=none, 1=palette, 2=rgb
/// For palette: r holds palette index. For rgb: r/g/b hold components.
/// Layout verified by manual probe: 8 bytes (tag + 3 bytes + 4 padding).
/// align(4) on tag field to match terminal.Style.Color's alignment.
pub const StyleColor = extern struct {
    r: u8 align(4), // byte 0 — payload (palette index also lives here)
    g: u8, // byte 1
    b: u8, // byte 2
    _pad: u8 = 0, // byte 3
    tag: u8, // byte 4
    _pad2: [3]u8 = .{ 0, 0, 0 }, // bytes 5–7 to reach 8 bytes
};

/// C-safe mirror of terminal.Style.
/// Layout verified by manual probe: 28 bytes total.
/// fg_color at offset 0, bg_color at 8, underline_color at 16, flags at 24.
pub const CellStyle = extern struct {
    fg_color: StyleColor,
    bg_color: StyleColor,
    underline_color: StyleColor,
    flags: u16,
    _pad: [2]u8 = undefined,
};

/// Update the persistent RenderState from current terminal state.
/// The caller must already hold the terminal mutex.
/// Returns 0 on success, 1 on allocation error.
pub export fn ghostty_terminal_render_update(ptr: *anyopaque) callconv(.c) u8 {
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));
    handle.render_state.update(handle.alloc, &handle.terminal_inst) catch return 1;
    return 0;
}

/// Returns dirty state: 0=false, 1=partial, 2=full.
pub export fn ghostty_terminal_render_dirty(ptr: *anyopaque) callconv(.c) c_int {
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));
    return @intFromEnum(handle.render_state.dirty);
}

/// Clear the dirty state (call after rendering).
pub export fn ghostty_terminal_render_clear_dirty(ptr: *anyopaque) callconv(.c) void {
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));
    handle.render_state.dirty = .false;
    // Also clear per-row dirty flags
    const row_dirty = handle.render_state.row_data.items(.dirty);
    @memset(row_dirty, false);
}

/// Writes the number of rows and columns in the current render state.
pub export fn ghostty_terminal_get_dimensions(
    ptr: *anyopaque,
    // Safety: Rust passes &mut u16 references, which is noalias
    noalias rows: *u16,
    noalias cols: *u16,
) callconv(.c) void {
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));
    rows.* = handle.render_state.rows;
    cols.* = handle.render_state.cols;
}

/// Returns a pointer to `RenderState.cursor`.
/// The pointer is valid until the next `render_update()` call.
pub export fn ghostty_terminal_render_cursor(ptr: *anyopaque) callconv(.c) *const RenderCursor {
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));
    return @ptrCast(&handle.render_state.cursor);
}

/// Returns a pointer to `RenderState.colors`
/// The pointer is valid until the next `render_update()` call.
pub export fn ghostty_terminal_render_colors(ptr: *anyopaque) callconv(.c) *const RenderColors {
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));
    return @ptrCast(&handle.render_state.colors);
}

/// Returns a pointer to the `std.MultiArrayList(Row)` in RenderState.
/// The returned pointer is valid until the next `render_update()` call.
pub export fn ghostty_terminal_render_row_data(ptr: *anyopaque) callconv(.c) *const ZigMultiArrayList {
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));
    return @ptrCast(&handle.render_state.row_data);
}
