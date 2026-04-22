const std = @import("std");

const handle_mod = @import("handle.zig");
const TerminalHandle = handle_mod.TerminalHandle;

const zconpty = @import("zconpty");
const abi = zconpty.Terminal;

const terminal = @import("../ghostty/src/terminal/main.zig");
const pagepkg = @import("../ghostty/src/terminal/page.zig");
const screenpkg = @import("../ghostty/src/terminal/Screen.zig");
const simd = @import("../ghostty/src/simd/main.zig");
const stylepkg = @import("../ghostty/src/terminal/style.zig");

const Row = pagepkg.Row;
const Screen = screenpkg;
const Pin = terminal.PageList.Pin;

const StyleIds = struct {
    ids: std.ArrayList(stylepkg.Id) = .empty,

    fn deinit(self: *StyleIds, allocator: std.mem.Allocator) void {
        self.ids.deinit(allocator);
        self.* = undefined;
    }

    fn ensureTotalCapacity(self: *StyleIds, allocator: std.mem.Allocator, count: usize) !void {
        try self.ids.ensureTotalCapacity(allocator, count);
    }

    fn append(self: *StyleIds, allocator: std.mem.Allocator, id: stylepkg.Id) !void {
        try self.ids.append(allocator, id);
    }

    fn appendAssumeCapacity(self: *StyleIds, id: stylepkg.Id) void {
        self.ids.appendAssumeCapacity(id);
    }

    fn items(self: *const StyleIds) []const stylepkg.Id {
        return self.ids.items;
    }

    fn addForRow(
        self: *StyleIds,
        allocator: std.mem.Allocator,
        pin: Pin,
        style: abi.Style,
    ) !void {
        const id = try addStyleIdForPin(pin, style);
        errdefer releaseStyleIdsForPin(pin, &.{id});
        try self.append(allocator, id);
    }

    fn releaseForRow(self: *const StyleIds, pin: Pin) void {
        releaseStyleIdsForPin(pin, self.items());
    }
};

pub fn readRect(
    ptr: *anyopaque,
    rect: abi.Rect,
    out: [*]abi.Cell,
) abi.Rect {
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));
    handle.lock();
    defer handle.unlock();

    const clipped = clipRect(handle, rect);
    if (clipped.width == 0 or clipped.height == 0) return clipped;

    const screen = handle.terminal_inst.screens.active;
    var row_pin = screen.pages.getTopLeft(.active).down(clipped.y).?;
    row_pin.x = clipped.x;

    var out_index: usize = 0;
    var row_offset: u16 = 0;
    while (row_offset < clipped.height) : (row_offset += 1) {
        const row_cells = row_pin.cells(.all);

        var col_offset: u16 = 0;
        while (col_offset < clipped.width) : (col_offset += 1) {
            const col = clipped.x + col_offset;
            out[out_index] = readCell(row_pin, row_cells, col);
            out_index += 1;
        }

        if (row_offset + 1 < clipped.height) {
            row_pin = row_pin.down(1).?;
            row_pin.x = clipped.x;
        }
    }

    return clipped;
}

pub fn writeRect(
    ptr: *anyopaque,
    rect: abi.Rect,
    cells: [*]const abi.Cell,
) abi.Rect {
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));
    handle.lock();
    defer handle.unlock();

    const clipped = clipRect(handle, rect);
    if (clipped.width == 0 or clipped.height == 0) return clipped;

    var rows_written: u16 = 0;
    while (rows_written < clipped.height) : (rows_written += 1) {
        const row_start = @as(usize, rows_written) * @as(usize, clipped.width);
        const row_slice = cells[row_start .. row_start + clipped.width];
        if (!writeRectRow(handle, clipped, rows_written, row_slice)) break;
    }

    if (rows_written > 0) handle.outputTrampoline();

    return .{
        .x = clipped.x,
        .y = clipped.y,
        .width = clipped.width,
        .height = rows_written,
    };
}

pub fn fillSpan(
    ptr: *anyopaque,
    start: abi.Point,
    len: u32,
    kind: abi.FillKind,
    cell: abi.Cell,
) u32 {
    if (len == 0) return 0;

    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));
    handle.lock();
    defer handle.unlock();

    const cols = handle.terminal_inst.cols;
    const rows = handle.terminal_inst.rows;
    if (start.x >= cols or start.y >= rows) return 0;

    var count_written: u32 = 0;
    var row_y = start.y;
    var row_x = start.x;
    while (count_written < len and row_y < rows) : (row_y += 1) {
        const remaining = len - count_written;
        const advanced = switch (kind) {
            .character, .cell => fillContentRow(handle, row_x, row_y, remaining, kind, cell),
            .style => fillStyleRow(handle, row_x, row_y, remaining, cell.style),
        };
        count_written += advanced;
        if (advanced < rowCapacity(cols, row_x)) break;
        row_x = 0;
    }

    if (count_written > 0) handle.outputTrampoline();
    return count_written;
}

fn writeRectRow(
    handle: *TerminalHandle,
    clipped: abi.Rect,
    row_offset: u16,
    source: []const abi.Cell,
) bool {
    var fallback = std.heap.stackFallback(4096, std.heap.smp_allocator);
    const allocator = fallback.get();

    var style_ids: StyleIds = .{};
    defer style_ids.deinit(allocator);
    style_ids.ensureTotalCapacity(allocator, source.len) catch return false;

    const screen = handle.terminal_inst.screens.active;
    const pin = screen.pages.pin(.{ .viewport = .{ .x = clipped.x, .y = clipped.y + row_offset } }).?;

    for (source) |cell| {
        const id = addStyleIdForPin(pin, cell.style) catch {
            style_ids.releaseForRow(pin);
            return false;
        };
        style_ids.appendAssumeCapacity(id);
    }

    const rac = pin.rowAndCell();
    const row_cells = pin.cells(.all);
    const target = row_cells[clipped.x .. clipped.x + clipped.width];

    splitRowBoundary(screen, pin, rac.row, clipped.x);
    splitRowBoundary(screen, pin, rac.row, clipped.x + clipped.width);
    screen.clearCells(&pin.node.data, rac.row, target);
    rac.row.dirty = true;

    var source_index: usize = 0;
    while (source_index < source.len) {
        const source_cell = source[source_index];
        switch (source_cell.width) {
            .narrow => {
                target[source_index] = makeNarrowCell(source_cell.codepoint, style_ids.items()[source_index]);
                if (style_ids.items()[source_index] != stylepkg.default_id) rac.row.styled = true;
                source_index += 1;
            },
            .wide_lead => {
                const has_tail = source_index + 1 < source.len and source[source_index + 1].width == .wide_trail;
                if (!has_tail) {
                    target[source_index] = makeNarrowCell(0, style_ids.items()[source_index]);
                    if (style_ids.items()[source_index] != stylepkg.default_id) rac.row.styled = true;
                    source_index += 1;
                    continue;
                }

                if (source_index + 1 >= target.len) {
                    target[source_index] = makeNarrowCell(0, style_ids.items()[source_index]);
                    if (style_ids.items()[source_index] != stylepkg.default_id) rac.row.styled = true;
                    source_index += 2;
                    continue;
                }

                target[source_index] = makeWideLeadCell(source_cell.codepoint, style_ids.items()[source_index]);
                target[source_index + 1] = makeWideTailCell();
                if (style_ids.items()[source_index] != stylepkg.default_id) {
                    pin.node.data.styles.use(pin.node.data.memory, style_ids.items()[source_index]);
                    rac.row.styled = true;
                }
                source_index += 2;
            },
            .wide_trail => {
                target[source_index] = makeNarrowCell(0, style_ids.items()[source_index]);
                if (style_ids.items()[source_index] != stylepkg.default_id) rac.row.styled = true;
                source_index += 1;
            },
        }
    }

    return true;
}

fn fillContentRow(
    handle: *TerminalHandle,
    start_x: u16,
    row_y: u16,
    remaining: u32,
    kind: abi.FillKind,
    cell: abi.Cell,
) u32 {
    const cols = handle.terminal_inst.cols;
    const screen = handle.terminal_inst.screens.active;
    const pin = screen.pages.pin(.{ .viewport = .{ .x = start_x, .y = row_y } }).?;
    const rac = pin.rowAndCell();
    const row_cells = pin.cells(.all);

    var fallback = std.heap.stackFallback(4096, std.heap.smp_allocator);
    const allocator = fallback.get();
    var style_ids: StyleIds = .{};
    defer style_ids.deinit(allocator);

    var cursor_x = start_x;
    var count: u32 = 0;
    while (count < remaining and cursor_x < cols) : (count += 1) {
        const style = switch (kind) {
            .character => readCell(pin, row_cells, cursor_x).style,
            .cell => cell.style,
            .style => unreachable,
        };
        style_ids.addForRow(allocator, pin, style) catch {
            style_ids.releaseForRow(pin);
            return count;
        };

        const step = fillPatternWidth(kind, cell, cols, cursor_x);
        cursor_x += step;
    }

    if (count == 0) return 0;

    const end_x = cursor_x;
    splitRowBoundary(screen, pin, rac.row, start_x);
    splitRowBoundary(screen, pin, rac.row, end_x);
    screen.clearCells(&pin.node.data, rac.row, row_cells[start_x..end_x]);
    rac.row.dirty = true;

    cursor_x = start_x;
    var index: usize = 0;
    while (index < count) : (index += 1) {
        const style_id = style_ids.items()[index];
        switch (fillContentWidth(kind, cell)) {
            .narrow => {
                row_cells[cursor_x] = makeNarrowCell(cell.codepoint, style_id);
                if (style_id != stylepkg.default_id) rac.row.styled = true;
                cursor_x += 1;
            },
            .wide_lead => {
                if (cursor_x + 1 >= cols) {
                    row_cells[cursor_x] = makeNarrowCell(0, style_id);
                    if (style_id != stylepkg.default_id) rac.row.styled = true;
                    cursor_x += 1;
                    continue;
                }

                row_cells[cursor_x] = makeWideLeadCell(cell.codepoint, style_id);
                row_cells[cursor_x + 1] = makeWideTailCell();
                if (style_id != stylepkg.default_id) {
                    pin.node.data.styles.use(pin.node.data.memory, style_id);
                    rac.row.styled = true;
                }
                cursor_x += 2;
            },
            .wide_trail => unreachable,
        }
    }

    return count;
}

fn fillStyleRow(
    handle: *TerminalHandle,
    start_x: u16,
    row_y: u16,
    remaining: u32,
    style: abi.Style,
) u32 {
    const cols = handle.terminal_inst.cols;
    const count = @min(remaining, rowCapacity(cols, start_x));
    if (count == 0) return 0;

    var fallback = std.heap.stackFallback(4096, std.heap.smp_allocator);
    const allocator = fallback.get();
    var style_ids: StyleIds = .{};
    defer style_ids.deinit(allocator);
    style_ids.ensureTotalCapacity(allocator, count) catch return 0;

    const screen = handle.terminal_inst.screens.active;
    const pin = screen.pages.pin(.{ .viewport = .{ .x = start_x, .y = row_y } }).?;

    var i: u32 = 0;
    while (i < count) : (i += 1) {
        const id = addStyleIdForPin(pin, style) catch {
            style_ids.releaseForRow(pin);
            return i;
        };
        style_ids.appendAssumeCapacity(id);
    }

    const rac = pin.rowAndCell();
    const row_cells = pin.cells(.all);

    var target_index: ?usize = null;
    i = 0;
    while (i < count) : (i += 1) {
        const col = @as(usize, start_x) + @as(usize, i);
        const actual = resolveStyleTargetColumn(row_cells, col);
        if (target_index) |prev| {
            if (actual == prev) continue;
        }
        target_index = actual;

        var target = &row_cells[actual];
        if (target.style_id != stylepkg.default_id) {
            pin.node.data.styles.release(pin.node.data.memory, target.style_id);
        }
        target.style_id = style_ids.items()[i];
        if (style_ids.items()[i] != stylepkg.default_id) rac.row.styled = true;
        rac.row.dirty = true;
    }

    pin.node.data.updateRowStyledFlag(rac.row);
    return count;
}

fn clipRect(handle: *TerminalHandle, rect: abi.Rect) abi.Rect {
    const cols = handle.terminal_inst.cols;
    const rows = handle.terminal_inst.rows;
    if (rect.x >= cols or rect.y >= rows) {
        return .{ .x = rect.x, .y = rect.y, .width = 0, .height = 0 };
    }

    const width = @min(rect.width, cols - rect.x);
    const height = @min(rect.height, rows - rect.y);
    return .{ .x = rect.x, .y = rect.y, .width = width, .height = height };
}

fn readCell(pin: Pin, row_cells: []const pagepkg.Cell, col: u16) abi.Cell {
    const index = @as(usize, col);
    const cell = row_cells[index];
    const width = switch (cell.wide) {
        .narrow => abi.Width.narrow,
        .wide => abi.Width.wide_lead,
        .spacer_tail, .spacer_head => abi.Width.wide_trail,
    };

    const style = readStyle(pin, row_cells, index);
    return .{
        .codepoint = switch (cell.wide) {
            .spacer_tail, .spacer_head => 0,
            else => cell.codepoint(),
        },
        .style = style,
        .width = width,
        ._padding = 0,
    };
}

fn readStyle(pin: Pin, row_cells: []const pagepkg.Cell, index: usize) abi.Style {
    const style_cell = styleSourceCell(pin, row_cells, index);
    const ghostty_style = style_cell.pin.style(style_cell.cell);
    const defaults = defaultStyle();

    return .{
        .fg = colorToIndex(ghostty_style.fg_color, defaults.fg),
        .bg = backgroundIndex(ghostty_style, style_cell.cell.*, defaults.bg),
        .underline = ghostty_style.flags.underline != .none,
        .inverse = ghostty_style.flags.inverse,
        ._reserved = 0,
    };
}

fn styleSourceCell(pin: Pin, row_cells: []const pagepkg.Cell, index: usize) struct {
    pin: Pin,
    cell: *const pagepkg.Cell,
} {
    const cell = &row_cells[index];
    switch (cell.wide) {
        .spacer_tail => {
            if (index > 0 and row_cells[index - 1].wide == .wide) {
                return .{ .pin = pin.left(1), .cell = &row_cells[index - 1] };
            }
        },
        .spacer_head => {
            if (pin.down(1)) |next_row| {
                var next = next_row;
                next.x = 0;
                const next_cells = next.cells(.all);
                if (next_cells[0].wide == .wide) {
                    return .{ .pin = next, .cell = &next_cells[0] };
                }
            }
        },
        .narrow, .wide => {},
    }

    return .{ .pin = pin, .cell = cell };
}

fn defaultStyle() abi.Style {
    return .{ .fg = 7, .bg = 0, .underline = false, .inverse = false, ._reserved = 0 };
}

fn colorToIndex(color: stylepkg.Style.Color, fallback: u4) u4 {
    return switch (color) {
        .none => fallback,
        .palette => |idx| if (idx < 16) @intCast(idx) else fallback,
        .rgb => fallback,
    };
}

fn backgroundIndex(style: stylepkg.Style, cell: pagepkg.Cell, fallback: u4) u4 {
    return switch (cell.content_tag) {
        .bg_color_palette => if (cell.content.color_palette < 16) @intCast(cell.content.color_palette) else fallback,
        .bg_color_rgb => fallback,
        else => colorToIndex(style.bg_color, fallback),
    };
}

fn addStyleIdForPin(pin: Pin, style: abi.Style) !stylepkg.Id {
    const ghostty_style = ghosttyStyle(style);
    return pin.node.data.styles.add(pin.node.data.memory, ghostty_style);
}

fn releaseStyleIdsForPin(pin: Pin, ids: []const stylepkg.Id) void {
    if (ids.len == 0) return;
    for (ids) |id| {
        if (id == stylepkg.default_id) continue;
        pin.node.data.styles.release(pin.node.data.memory, id);
    }
}

fn ghosttyStyle(style: abi.Style) stylepkg.Style {
    return .{
        .fg_color = .{ .palette = style.fg },
        .bg_color = .{ .palette = style.bg },
        .flags = .{
            .underline = if (style.underline) .single else .none,
            .inverse = style.inverse,
        },
    };
}

fn makeNarrowCell(codepoint: u32, style_id: stylepkg.Id) pagepkg.Cell {
    var cell = pagepkg.Cell.init(validCodepoint(codepoint));
    cell.style_id = style_id;
    return cell;
}

fn makeWideLeadCell(codepoint: u32, style_id: stylepkg.Id) pagepkg.Cell {
    var cell = pagepkg.Cell.init(validCodepoint(codepoint));
    cell.style_id = style_id;
    cell.wide = .wide;
    return cell;
}

fn makeWideTailCell() pagepkg.Cell {
    return .{
        .content_tag = .codepoint,
        .content = .{ .codepoint = 0 },
        .wide = .spacer_tail,
    };
}

fn validCodepoint(codepoint: u32) u21 {
    if (codepoint > 0x10FFFF) return 0;
    if (codepoint >= 0xD800 and codepoint <= 0xDFFF) return 0;
    return @intCast(codepoint);
}

fn rowCapacity(cols: u16, start_x: u16) u32 {
    return cols - start_x;
}

fn normalizeContentWidth(width: abi.Width) abi.Width {
    return switch (width) {
        .wide_trail => .narrow,
        else => width,
    };
}

fn fillContentWidth(kind: abi.FillKind, cell: abi.Cell) abi.Width {
    return switch (kind) {
        .character => codepointWidth(cell.codepoint),
        .cell => normalizeContentWidth(cell.width),
        .style => unreachable,
    };
}

fn fillPatternWidth(kind: abi.FillKind, cell: abi.Cell, cols: u16, x: u16) u16 {
    return switch (fillContentWidth(kind, cell)) {
        .narrow => 1,
        .wide_lead => if (x + 1 < cols) 2 else 1,
        .wide_trail => unreachable,
    };
}

fn codepointWidth(codepoint: u32) abi.Width {
    return switch (simd.codepointWidth(codepoint)) {
        2 => .wide_lead,
        else => .narrow,
    };
}

fn resolveStyleTargetColumn(row_cells: []const pagepkg.Cell, col: usize) usize {
    if (row_cells[col].wide == .spacer_tail and col > 0) {
        if (row_cells[col - 1].wide == .wide) return col - 1;
    }
    return col;
}

fn splitRowBoundary(screen: *Screen, pin: Pin, row: *Row, x: usize) void {
    const page = &pin.node.data;
    page.pauseIntegrityChecks(true);
    defer page.pauseIntegrityChecks(false);

    const cols = page.size.cols;
    std.debug.assert(x <= cols);

    if (x == cols) {
        if (!row.wrap) return;

        const cells = pin.cells(.all);
        if (cells[cols - 1].wide == .spacer_head) {
            screen.clearCells(page, row, cells[cols - 1 ..][0..1]);
            row.dirty = true;
        }
        return;
    }

    if ((x == 0 or x == 1) and row.wrap_continuation) {
        const cells = pin.cells(.all);
        if (cells[0].wide == .wide) {
            if (pin.up(1)) |prev_row| {
                const prev_rac = prev_row.rowAndCell();
                const prev_cells = prev_row.cells(.all);
                const prev_col = prev_row.node.data.size.cols - 1;
                if (prev_cells[prev_col].wide == .spacer_head) {
                    screen.clearCells(&prev_row.node.data, prev_rac.row, prev_cells[prev_col..][0..1]);
                    prev_rac.row.dirty = true;
                }
            }
        }
    }

    if (x == 0) return;

    const cells = pin.cells(.all);
    const left = cells[x - 1];
    switch (left.wide) {
        .wide => {
            screen.clearCells(page, row, cells[x - 1 ..][0..2]);
            row.dirty = true;
        },
        .narrow, .spacer_tail => {},
        .spacer_head => unreachable,
    }
}
