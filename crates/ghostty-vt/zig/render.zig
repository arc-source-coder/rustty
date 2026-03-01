const handle_mod = @import("handle.zig");
const terminal = @import("ghostty/src/terminal/main.zig");

const FlatCell = handle_mod.FlatCell;
const TerminalHandle = handle_mod.TerminalHandle;

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

/// Update the persistent RenderState from current terminal state.
/// Returns 0 on success, 1 if null handle, 2 on allocation error.
export fn ghostty_vt_terminal_render_update(ptr: ?*anyopaque) callconv(.c) c_int {
    if (ptr == null) return 1;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    handle.render_state.update(handle.alloc, &handle.terminal_inst) catch return 2;
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

/// Get a palette color by index (0–255). Returns the RGB via out pointer.
export fn ghostty_vt_terminal_render_palette_color(
    ptr: ?*anyopaque,
    index: u8,
    out: ?*ColorRGB,
) callconv(.c) c_int {
    if (ptr == null or out == null) return 1;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    const rgb = handle.render_state.colors.palette[index];
    const result = out.?;
    result.* = .{ .r = rgb.r, .g = rgb.g, .b = rgb.b };
    return 0;
}

fn flattenStyleColor(c: terminal.Style.Color) struct { color_type: u8, r: u8, g: u8, b: u8, palette: u8 } {
    return switch (c) {
        .none => .{ .color_type = 0, .r = 0, .g = 0, .b = 0, .palette = 0 },
        .palette => |p| .{ .color_type = 1, .r = 0, .g = 0, .b = 0, .palette = p },
        .rgb => |rgb| .{ .color_type = 2, .r = rgb.r, .g = rgb.g, .b = rgb.b, .palette = 0 },
    };
}

/// Get flattened cell data for a row. Returns pointer to `cols` FlatCell entries.
/// The returned pointer is valid until the next render_update() or terminal mutation.
/// Returns null if row is out of bounds.
///
/// Grapheme codepoints (for cells with grapheme_len > 0) can be retrieved
/// via ghostty_vt_terminal_render_cell_grapheme().
export fn ghostty_vt_terminal_render_row_cells(
    ptr: ?*anyopaque,
    row: u16,
    out_len: ?*u16,
) callconv(.c) ?[*]const FlatCell {
    if (ptr == null) return null;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    if (row >= handle.render_state.rows) return null;

    const cells = handle.render_state.row_data.items(.cells)[row];
    const raws = cells.items(.raw);
    const styles = cells.items(.style);
    const graphemes = cells.items(.grapheme);

    // Flatten into the pre-allocated flat_cells buffer
    const cols = handle.render_state.cols;
    if (cols == 0) return null;

    // Ensure flat_cells buffer is large enough
    handle.ensureFlatCells(cols) catch return null;

    for (0..cols) |i| {
        const raw = raws[i];
        // The style field is only valid if raw.style_id > 0
        const style: terminal.Style = if (raw.style_id > 0) styles[i] else .{};
        const fg = flattenStyleColor(style.fg_color);
        var bg = flattenStyleColor(style.bg_color);

        // Handle bg_color_palette and bg_color_rgb content tags
        // where the background comes from the cell content, not style
        switch (raw.content_tag) {
            .bg_color_palette => {
                bg = .{ .color_type = 1, .r = 0, .g = 0, .b = 0, .palette = raw.content.color_palette };
            },
            .bg_color_rgb => {
                const c = raw.content.color_rgb;
                bg = .{ .color_type = 2, .r = c.r, .g = c.g, .b = c.b, .palette = 0 };
            },
            else => {},
        }
        const ul = flattenStyleColor(style.underline_color);

        handle.flat_cells[i] = .{
            .codepoint = switch (raw.content_tag) {
                .codepoint, .codepoint_grapheme => raw.content.codepoint,
                .bg_color_palette, .bg_color_rgb => 0,
            },
            .grapheme_len = if (raw.content_tag == .codepoint_grapheme)
                @intCast(graphemes[i].len)
            else
                0,
            .wide = @intFromEnum(raw.wide),

            .fg_color_type = fg.color_type,
            .fg_r = fg.r,
            .fg_g = fg.g,
            .fg_b = fg.b,
            .fg_palette = fg.palette,

            .bg_color_type = bg.color_type,
            .bg_r = bg.r,
            .bg_g = bg.g,
            .bg_b = bg.b,
            .bg_palette = bg.palette,

            .ul_color_type = ul.color_type,
            .ul_r = ul.r,
            .ul_g = ul.g,
            .ul_b = ul.b,
            .ul_palette = ul.palette,

            // Populate style_flags by bitcasting Ghostty's packed flags.
            // Fragility: this assumes Ghostty keeps the same bit layout/order
            // for Style.flags; if that changes in a Ghostty update, this
            // export must be updated (or switched to explicit bit packing).
            .style_flags = @bitCast(style.flags),
            ._padding = .{ 0, 0 },
        };
    }

    if (out_len) |len| len.* = cols;
    return handle.flat_cells.ptr;
}

/// Get grapheme codepoints for a cell. Returns pointer to grapheme_len u32 values.
/// Only valid for cells where grapheme_len > 0.
export fn ghostty_vt_terminal_render_cell_grapheme(
    ptr: ?*anyopaque,
    row: u16,
    col: u16,
    out_len: ?*u8,
) callconv(.c) ?[*]const u32 {
    if (ptr == null) return null;
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr.?));
    if (row >= handle.render_state.rows) return null;

    const cells = handle.render_state.row_data.items(.cells)[row];
    if (col >= cells.len) return null;

    const raw = cells.items(.raw)[col];
    if (raw.content_tag != .codepoint_grapheme) return null;

    const grapheme = cells.items(.grapheme)[col];
    if (out_len) |len| len.* = @intCast(grapheme.len);
    // u21 and u32 have different sizes, so we need a cast.
    // The grapheme data lives in the row's arena, valid until next update.
    // We can't directly cast []u21 to [*]u32 — need the grapheme_buf.
    handle.copyGraphemeToBuf(grapheme) catch return null;
    return handle.grapheme_buf.ptr;
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
