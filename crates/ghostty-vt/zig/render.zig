const handle_mod = @import("handle.zig");
const terminal = @import("ghostty/src/terminal/main.zig");
const std = @import("std");
const color = terminal.color;

const TerminalHandle = handle_mod.TerminalHandle;

// === ABI STABILITY ASSERTIONS ===
// These verify that Ghostty's internal type layouts match what the Rust
// side expects. If any of these assertions fail after a Ghostty update,
// the Rust-side structs and these assertions must be updated to match.
comptime {
    const page = @import("ghostty/src/terminal/page.zig");

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
    std.debug.assert(@sizeOf([]const u21) == @sizeOf(GraphemeSlice));
    std.debug.assert(@alignOf([]const u21) == @alignOf(GraphemeSlice));

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
    // Verify tag values via @tagName (none=0, palette=1, rgb=2 based on declaration order)
    const none_color: terminal.Style.Color = .none;
    const pal_color: terminal.Style.Color = .{ .palette = 0xAB };
    const rgb_color: terminal.Style.Color = .{ .rgb = .{ .r = 1, .g = 2, .b = 3 } };
    std.debug.assert(std.mem.eql(u8, @tagName(none_color), "none"));
    std.debug.assert(std.mem.eql(u8, @tagName(pal_color), "palette"));
    std.debug.assert(std.mem.eql(u8, @tagName(rgb_color), "rgb"));

    // --- color.RGB (verifies why we need the sidecar) ---
    std.debug.assert(@sizeOf(color.RGB) == 4); // packed(u24) with 1 byte padding
    std.debug.assert(@sizeOf(color.RGB.C) == 3); // C-compatible version is 3 bytes
    std.debug.assert(@alignOf(color.RGB.C) == 1); // No alignment padding
}

/// C-safe cursor state
const CursorState = extern struct {
    /// Cursor position in viewport coordinates. If not visible in viewport,
    /// x and y are set to active-area coordinates and in_viewport is 0.
    x: u16,
    y: u16,
    in_viewport: u8,
    /// Visual style: 0=bar, 1=block, 2=underline, 3=block_hollow
    style: u8,
    visible: u8,
    blinking: u8,
    password_input: u8,
    /// 1 if cursor is on the tail half of a wide char
    wide_tail: u8,
};

/// C-safe color triplet
const ColorRGB = extern struct {
    r: u8,
    g: u8,
    b: u8,
};

/// C-safe terminal color state
const ColorState = extern struct {
    background: ColorRGB,
    foreground: ColorRGB,
    /// Cursor color; if has_cursor_color is 0, cursor_color is undefined
    cursor_color: ColorRGB,
    has_cursor_color: u8,
};

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

/// C-safe mirror of a Zig slice.
/// Used to return []const []const u21 as ?[*]const GraphemeSlice.
/// The Rust side interprets it as &[GraphemeSlice] and forms slices
/// from ptr + len when accessing the grapheme codepoints for a cell.
pub const GraphemeSlice = extern struct {
    ptr: ?[*]const u32,
    len: usize,
};

/// Update the persistent RenderState from current terminal state.
/// Returns 0 on success, 1 if null handle, 2 on allocation error.
export fn ghostty_vt_terminal_render_update(ptr: ?*anyopaque) callconv(.c) c_int {
    if (ptr == null) return 1;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    handle.render_state.update(handle.alloc, &handle.terminal_inst) catch return 2;

    // Repopulate palette sidecar only when dirty.
    // The check itself is free (one bool read). The loop (256 × 3-byte copy)
    // only runs when colors actually change — rare in normal use.
    // We cannot memcpy because color.RGB is packed struct(u24) with @sizeOf == 4.
    // Use .cval() to convert to C-compatible 3-byte RGB.
    if (handle.palette_dirty) {
        const palette = handle.render_state.colors.palette;
        for (palette, 0..) |rgb, i| {
            handle.palette_cache[i] = rgb.cval();
        }
        handle.palette_dirty = false;
    }

    return 0;
}

/// Returns dirty state: 0=false, 1=partial, 2=full
export fn ghostty_vt_terminal_render_dirty(ptr: ?*anyopaque) callconv(.c) u8 {
    if (ptr == null) return 0;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    return switch (handle.render_state.dirty) {
        .false => 0,
        .partial => 1,
        .full => 2,
    };
}

/// Clear the dirty state (call after rendering)
export fn ghostty_vt_terminal_render_clear_dirty(ptr: ?*anyopaque) callconv(.c) void {
    if (ptr == null) return;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    handle.render_state.dirty = .false;
    // Also clear per-row dirty flags
    for (handle.render_state.row_data.items(.dirty)) |*d| {
        d.* = false;
    }
}

/// Returns number of rows in the current render state
export fn ghostty_vt_terminal_render_rows(ptr: ?*anyopaque) callconv(.c) u16 {
    if (ptr == null) return 0;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    return handle.render_state.rows;
}

/// Returns number of columns in the current render state
export fn ghostty_vt_terminal_render_cols(ptr: ?*anyopaque) callconv(.c) u16 {
    if (ptr == null) return 0;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    return handle.render_state.cols;
}

/// Returns 1 if the given row is dirty, 0 otherwise
export fn ghostty_vt_terminal_render_row_dirty(ptr: ?*anyopaque, row: u16) callconv(.c) u8 {
    if (ptr == null) return 0;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    if (row >= handle.render_state.rows) return 0;
    return @intFromBool(handle.render_state.row_data.items(.dirty)[row]);
}

/// Get cursor state from the current render state
export fn ghostty_vt_terminal_render_cursor(ptr: ?*anyopaque, out: ?*CursorState) callconv(.c) c_int {
    if (ptr == null or out == null) return 1;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    const c = &handle.render_state.cursor;
    const result = out.?;

    if (c.viewport) |vp| {
        result.x = vp.x;
        result.y = vp.y;
        result.in_viewport = 1;
        result.wide_tail = @intFromBool(vp.wide_tail);
    } else {
        result.x = c.active.x;
        result.y = @intCast(c.active.y);
        result.in_viewport = 0;
        result.wide_tail = 0;
    }

    result.style = switch (c.visual_style) {
        .bar => 0,
        .block => 1,
        .underline => 2,
        .block_hollow => 3,
    };
    result.visible = @intFromBool(c.visible);
    result.blinking = @intFromBool(c.blinking);
    result.password_input = @intFromBool(c.password_input);
    return 0;
}

/// Get terminal colors from the current render state.
/// Palette access is separate (256 entries is too large for a return struct).
export fn ghostty_vt_terminal_render_colors(ptr: ?*anyopaque, out: ?*ColorState) callconv(.c) c_int {
    if (ptr == null or out == null) return 1;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    const colors = &handle.render_state.colors;
    const result = out.?;

    result.background = .{ .r = colors.background.r, .g = colors.background.g, .b = colors.background.b };
    result.foreground = .{ .r = colors.foreground.r, .g = colors.foreground.g, .b = colors.foreground.b };

    if (colors.cursor) |cc| {
        result.cursor_color = .{ .r = cc.r, .g = cc.g, .b = cc.b };
        result.has_cursor_color = 1;
    } else {
        result.has_cursor_color = 0;
    }

    return 0;
}

/// Get selection range for a row. Returns 1 if row has a selection, 0 otherwise.
/// When returning 1, start_x and end_x are set to the selection column range.
export fn ghostty_vt_terminal_render_row_selection(
    ptr: ?*anyopaque,
    row: u16,
    start_x: ?*u16,
    end_x: ?*u16,
) callconv(.c) u8 {
    if (ptr == null) return 0;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    if (row >= handle.render_state.rows) return 0;

    const sel = handle.render_state.row_data.items(.selection)[row];
    if (sel) |range| {
        if (start_x) |sx| sx.* = range[0];
        if (end_x) |ex| ex.* = range[1];
        return 1;
    }
    return 0;
}

/// Returns a direct pointer into RenderState's page.Cell array for a row.
/// Zero-copy: the pointer is into persistent Zig memory.
/// Valid until the next render_update() call.
export fn ghostty_vt_terminal_render_row_raw(
    ptr: ?*anyopaque,
    row: u16,
    out_len: ?*u16,
) callconv(.c) ?[*]const u64 {
    if (ptr == null) return null;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    if (row >= handle.render_state.rows) return null;

    const cells = handle.render_state.row_data.items(.cells)[row];
    const raws = cells.items(.raw);
    if (out_len) |len| len.* = @intCast(raws.len);
    // page.Cell is packed struct(u64), safe to cast to [*]const u64
    return @ptrCast(raws.ptr);
}

/// Returns a direct pointer into RenderState's Style array for a row.
/// Zero-copy: the pointer is into persistent Zig memory.
/// Valid until the next render_update() call.
/// The Style data at column `col` is only valid if the corresponding
/// page.Cell's style_id is non-zero OR content_tag is bg_color_*.
export fn ghostty_vt_terminal_render_row_styles(
    ptr: ?*anyopaque,
    row: u16,
    out_len: ?*u16,
) callconv(.c) ?[*]const CellStyle {
    if (ptr == null) return null;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    if (row >= handle.render_state.rows) return null;

    const cells = handle.render_state.row_data.items(.cells)[row];
    const styles = cells.items(.style);
    if (out_len) |len| len.* = @intCast(styles.len);
    // ptrCast is valid: CellStyle is an extern struct whose layout
    // is verified by comptime assertions to match terminal.Style exactly.
    return @ptrCast(styles.ptr);
}

/// Returns a direct pointer into the grapheme SoA column for a row.
/// Each element is a Zig slice []const u21 = { ptr: [*]const u21, len: usize }.
/// Since @sizeOf(u21) == @sizeOf(u32), ptr can be read as [*]const u32.
///
/// For cells without graphemes (content_tag != codepoint_grapheme), the
/// slice is undefined — caller must check the raw cell's content_tag.
///
/// Zero-copy: the pointer is into persistent Zig memory.
/// Valid until the next render_update() call.
export fn ghostty_vt_terminal_render_row_graphemes(
    ptr: ?*anyopaque,
    row: u16,
    out_len: ?*u16,
) callconv(.c) ?[*]const GraphemeSlice {
    if (ptr == null) return null;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    if (row >= handle.render_state.rows) return null;

    const cells = handle.render_state.row_data.items(.cells)[row];
    const graphemes = cells.items(.grapheme);
    if (out_len) |len| len.* = @intCast(graphemes.len);
    // ABI validated at comptime: u21 has same size/alignment as u32.
    return @ptrCast(graphemes.ptr);
}

/// Returns a pointer to the 256-entry palette sidecar.
/// Each entry is a 3-byte color.RGB.C (r, g, b — no padding).
/// The sidecar is refreshed during render_update() when palette is dirty.
export fn ghostty_vt_terminal_render_palette(
    ptr: ?*anyopaque,
) callconv(.c) ?[*]const color.RGB.C {
    if (ptr == null) return null;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    return &handle.palette_cache;
}
