const std = @import("std");
const handle_mod = @import("handle.zig");

const gesture = @import("../ghostty/src/terminal/SelectionGesture.zig");
const size = @import("../ghostty/src/renderer/size.zig");
const terminal = @import("../ghostty/src/terminal/main.zig");

const TerminalHandle = handle_mod.TerminalHandle;
const Screen = terminal.Screen;

const selection_codepoints = @import("../ghostty/src/terminal/selection_codepoints.zig");

const click_repeat_interval_ns = 500 * std.time.ns_per_ms; // 500ms

pub const SelectionUpdate = extern struct {
    needs_redraw: bool = false,
    autoscroll: bool = false,
};

fn posToViewport(x_pos: f32, y_pos: f32, sz: size.Size) terminal.point.Coordinate {
    const coord: size.Coordinate = .{ .surface = .{ .x = x_pos, .y = y_pos } };
    const grid = coord.convert(.grid, sz).grid;
    return .{ .x = grid.x, .y = grid.y };
}

pub export fn ghostty_terminal_gesture_press(
    ptr: *anyopaque,
    x_px: f32,
    y_px: f32,
    ctrl_or_super: bool,
    shift: bool,
    rectangular: bool,
) callconv(.c) SelectionUpdate {
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));

    handle.lock();
    defer handle.unlock();

    const screen: *Screen = handle.terminal_inst.screens.active;
    const now = std.time.Instant.now() catch null;
    const shift_capture = handle.terminal_inst.flags.mouse_shift_capture == .true;

    if (shift and !shift_capture and
        handle.gesture.left_click_count > 0 and screen.selection != null)
    extend: {
        const current_time = now orelse break :extend;
        const click_time = handle.gesture.left_click_time orelse break :extend;
        if (current_time.since(click_time) <= click_repeat_interval_ns) break :extend;

        // An accepted extension with an invalid anchor is a no-op, not a new press.
        return applyGestureDrag(handle, x_px, y_px, rectangular);
    }

    const position = posToViewport(x_px, y_px, handle.size);
    const pin = screen.pages.pin(.{ .viewport = position }) orelse return .{};

    const press: gesture.Press = .{
        .time = now,
        .pin = pin,
        .xpos = x_px,
        .ypos = y_px,
        .repeat_interval = click_repeat_interval_ns,
        .max_distance = @floatFromInt(handle.size.cell.width),
        .word_boundary_codepoints = &selection_codepoints.default_word_boundaries,
        .behaviors = &.{
            .cell,
            .word,
            if (ctrl_or_super) .output else .line,
        },
    };

    const selection = handle.gesture.press(&handle.terminal_inst, press) catch return .{};

    if (selection != null or handle.gesture.left_click_count == 1) {
        return .{ .needs_redraw = applySelection(screen, selection) };
    }
    return .{};
}

pub export fn ghostty_terminal_gesture_release(
    ptr: *anyopaque,
    x_px: f32,
    y_px: f32,
) callconv(.c) void {
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));

    handle.lock();
    defer handle.unlock();

    const screen: *Screen = handle.terminal_inst.screens.active;
    const position = posToViewport(x_px, y_px, handle.size);
    const pin = screen.pages.pin(.{ .viewport = position });
    handle.gesture.release(&handle.terminal_inst, .{ .pin = pin });
}

pub export fn ghostty_terminal_gesture_drag(
    ptr: *anyopaque,
    x_px: f32,
    y_px: f32,
    rectangular: bool,
) callconv(.c) SelectionUpdate {
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));

    handle.lock();
    defer handle.unlock();

    return applyGestureDrag(handle, x_px, y_px, rectangular);
}

/// Caller must currently hold handle.mutex.
fn applyGestureDrag(handle: *TerminalHandle, x_px: f32, y_px: f32, rectangular: bool) SelectionUpdate {
    if (handle.gesture.left_click_count == 0 or
        handle.gesture.validatedLeftClickPin(&handle.terminal_inst.screens) == null)
    {
        // Keep a running timer alive so autoscrollTick can cancel an invalid anchor.
        return .{ .autoscroll = handle.gesture.left_drag_autoscroll != .none };
    }

    const screen: *Screen = handle.terminal_inst.screens.active;
    const position = posToViewport(x_px, y_px, handle.size);

    const pin = screen.pages.pin(.{ .viewport = position }) orelse return .{};
    const drag: gesture.Drag = .{
        .pin = pin,
        .xpos = x_px,
        .ypos = y_px,
        .rectangle = rectangular,
        .word_boundary_codepoints = &selection_codepoints.default_word_boundaries,
        .geometry = .{
            .cell_width = handle.size.cell.width,
            .columns = @intCast(handle.size.grid().columns),
            .padding_left = handle.size.padding.left,
            .screen_height = handle.size.screen.height,
        },
    };

    const selection = handle.gesture.drag(&handle.terminal_inst, drag);
    // A valid drag may return null to clear the selection at the anchor.
    return .{
        .needs_redraw = applySelection(screen, selection),
        .autoscroll = handle.gesture.left_drag_autoscroll != .none,
    };
}

pub export fn ghostty_terminal_gesture_autoscroll_tick(
    ptr: *anyopaque,
    x_px: f32,
    y_px: f32,
    rectangular: bool,
) callconv(.c) SelectionUpdate {
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));
    handle.lock();
    defer handle.unlock();

    if (handle.gesture.left_drag_autoscroll == .none) return .{};
    const t = &handle.terminal_inst;

    const screen = t.screens.active;
    const before = screen.pages.pin(.{ .viewport = .{ .x = 0, .y = 0 } });
    const selection = handle.gesture.autoscrollTick(t, .{
        .viewport = posToViewport(x_px, y_px, handle.size),
        .xpos = x_px,
        .ypos = y_px,
        .rectangle = rectangular,
        .word_boundary_codepoints = &selection_codepoints.default_word_boundaries,
        .geometry = .{
            .cell_width = handle.size.cell.width,
            .columns = @intCast(handle.size.grid().columns),
            .padding_left = handle.size.padding.left,
            .screen_height = handle.size.screen.height,
        },
    });

    // Invalid-screen ticks reset the gesture, not the new screen's selection.
    if (handle.gesture.left_click_count == 0) return .{};

    const after = screen.pages.pin(.{ .viewport = .{ .x = 0, .y = 0 } });
    const viewport_changed = changed: {
        if (before) |previous| {
            if (after) |current| break :changed !previous.eql(current);
            break :changed true;
        }
        break :changed true;
    };

    const selection_changed = applySelection(screen, selection);
    return .{
        .needs_redraw = viewport_changed or selection_changed,
        .autoscroll = handle.gesture.left_drag_autoscroll != .none,
    };
}

/// Caller holds the terminal lock. Compare before select frees the old tracked pins.
fn applySelection(screen: *Screen, next: ?terminal.Selection) bool {
    if (screen.selection) |previous| {
        if (next) |selection| {
            if (selection.eql(previous)) return false;
        }
    } else if (next == null) return false;

    screen.select(next) catch return false;
    return true;
}

pub export fn ghostty_terminal_gesture_reset(ptr: *anyopaque) callconv(.c) void {
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));
    handle.lock();
    defer handle.unlock();
    handle.gesture.reset(&handle.terminal_inst);
}

/// Clear any active selection. Returns whether a change occurred
pub export fn ghostty_terminal_clear_selection(ptr: *anyopaque) callconv(.c) bool {
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));

    handle.lock();
    defer handle.unlock();

    const screen: *Screen = handle.terminal_inst.screens.active;
    const changed = screen.selection != null;
    screen.clearSelection();

    return changed;
}

/// Get the currently selected text as a UTF-8 null-terminated string and then clear it.
/// Returns a pointer to the string, or null if no selection or error.
/// The caller must free the returned pointer with ghostty_terminal_bytes_free().
pub export fn ghostty_terminal_take_selection_text(
    ptr: *anyopaque,
    // Safety: Rust passes a &mut usize reference, which is noalias
    noalias out_len: *usize,
) callconv(.c) ?[*]const u8 {
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));

    handle.lock();
    defer handle.unlock();

    const screen: *Screen = handle.terminal_inst.screens.active;
    const selection = screen.selection orelse return null;
    const text = screen.selectionString(handle.alloc, .{ .sel = selection }) catch return null;
    screen.clearSelection();

    out_len.* = text.len;
    return text.ptr;
}

/// Free a byte buffer returned by get_selection_text.
pub export fn ghostty_terminal_bytes_free(
    ptr: *anyopaque,
    bytes: [*]const u8,
    len: usize,
) callconv(.c) void {
    const handle: *TerminalHandle = @ptrCast(@alignCast(ptr));
    const slice: [:0]u8 = @as([*:0]u8, @ptrCast(@constCast(bytes)))[0..len :0];
    handle.alloc.free(slice);
}
