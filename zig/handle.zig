const std = @import("std");
const Allocator = std.mem.Allocator;

const terminal = @import("ghostty/src/terminal/main.zig");
const color = terminal.color;

// --- Callback function pointer types ---
pub const BellCallback = *const fn (?*anyopaque) callconv(.c) void;
pub const TitleCallback = *const fn (?*anyopaque, [*]const u8, usize) callconv(.c) void;
pub const ResponseCallback = *const fn (?*anyopaque, [*]const u8, usize) callconv(.c) void;

const Callbacks = struct {
    userdata: ?*anyopaque = null,
    bell: ?BellCallback = null,
    title: ?TitleCallback = null,
    response: ?ResponseCallback = null,
};

// --- TerminalHandle: owns all terminal state ---
pub const TerminalHandle = struct {
    alloc: Allocator,
    terminal_inst: terminal.Terminal,
    handler: terminal.TerminalStream.Handler,
    stream: terminal.TerminalStream,
    callbacks: Callbacks,
    render_state: terminal.RenderState,

    /// Cell pixel dimensions — set by Rust via ghostty_terminal_set_cell_size().
    /// Used by Ghostty's Effects.size callback for XTWINOPS size reports.
    /// Zero means "not yet measured", in which case size reports are skipped.
    cell_width_px: u16 = 0,
    cell_height_px: u16 = 0,

    inline fn fromEffectsHandler(handler_ptr: *terminal.TerminalStream.Handler) *TerminalHandle {
        const stream_ptr: *terminal.TerminalStream = @fieldParentPtr("handler", handler_ptr);
        return @fieldParentPtr("stream", stream_ptr);
    }

    fn emitResponse(self: *TerminalHandle, bytes: []const u8) void {
        const cb = self.callbacks.response orelse return;
        cb(self.callbacks.userdata, bytes.ptr, bytes.len);
    }

    fn writePtyTrampoline(handler_ptr: *terminal.TerminalStream.Handler, data: [:0]const u8) void {
        const handle = fromEffectsHandler(handler_ptr);
        handle.emitResponse(data);
    }

    fn bellTrampoline(handler_ptr: *terminal.TerminalStream.Handler) void {
        const handle = fromEffectsHandler(handler_ptr);
        const cb = handle.callbacks.bell orelse return;
        cb(handle.callbacks.userdata);
    }

    fn titleChangedTrampoline(handler_ptr: *terminal.TerminalStream.Handler) void {
        const handle = fromEffectsHandler(handler_ptr);
        const cb = handle.callbacks.title orelse return;
        const title = handler_ptr.terminal.getTitle() orelse "";
        cb(handle.callbacks.userdata, title.ptr, title.len);
    }

    fn colorSchemeTrampoline(handler_ptr: *terminal.TerminalStream.Handler) ?terminal.device_status.ColorScheme {
        const bg = handler_ptr.terminal.colors.background.get() orelse return .dark;
        // Relative luminance midpoint gives a stable light/dark split.
        return if (bg.luminance() > 0.5) .light else .dark;
    }

    fn deviceAttributesTrampoline(_: *terminal.TerminalStream.Handler) terminal.device_attributes.Attributes {
        // Use Ghostty's default attributes and encoding behavior.
        return .{};
    }

    fn enquiryTrampoline(_: *terminal.TerminalStream.Handler) []const u8 {
        // Keep ENQ behavior disabled unless we add explicit configuration.
        return "";
    }

    fn sizeTrampoline(handler_ptr: *terminal.TerminalStream.Handler) ?terminal.size_report.Size {
        const handle = fromEffectsHandler(handler_ptr);
        if (handle.cell_width_px == 0 or handle.cell_height_px == 0) return null;

        return .{
            .rows = handler_ptr.terminal.rows,
            .columns = handler_ptr.terminal.cols,
            .cell_width = handle.cell_width_px,
            .cell_height = handle.cell_height_px,
        };
    }

    pub fn init(alloc: Allocator, cols: u16, rows: u16, fg: color.RGB, bg: color.RGB) !*TerminalHandle {
        const handle = try alloc.create(TerminalHandle);
        errdefer alloc.destroy(handle);

        const t = try terminal.Terminal.init(alloc, .{
            .cols = cols,
            .rows = rows,
            .colors = .{
                .background = color.DynamicRGB.init(bg),
                .foreground = color.DynamicRGB.init(fg),
                .cursor = .unset,
                .palette = .default,
            },
        });
        errdefer {
            var tmp = t;
            tmp.deinit(alloc);
        }

        handle.* = .{
            .alloc = alloc,
            .terminal_inst = t,
            .handler = .init(&handle.terminal_inst),
            .stream = undefined,
            .callbacks = .{},
            .render_state = .empty,
        };

        // Install effects callbacks once. They dispatch through Callbacks,
        // so updating callbacks later takes effect immediately.
        handle.handler.effects = .{
            .write_pty = &writePtyTrampoline,
            .bell = &bellTrampoline,
            .color_scheme = &colorSchemeTrampoline,
            .device_attributes = &deviceAttributesTrampoline,
            .enquiry = &enquiryTrampoline,
            .size = &sizeTrampoline,
            .title_changed = &titleChangedTrampoline,
            .xtversion = null,
        };

        handle.stream = terminal.TerminalStream.initAlloc(alloc, handle.handler);
        return handle;
    }

    pub fn deinit(self: *TerminalHandle) void {
        const alloc = self.alloc;
        self.render_state.deinit(alloc);
        self.stream.deinit();
        self.terminal_inst.deinit(alloc);
        self.* = undefined;
        alloc.destroy(self);
    }
};
