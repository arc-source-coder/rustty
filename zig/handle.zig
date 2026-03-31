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
    /// Backpointer to TerminalHandle
    handle: ?*TerminalHandle = null,
};

// --- ShimHandler: wraps TerminalStream.Handler + fires callbacks ---
const ShimHandler = struct {
    stream_handler: terminal.TerminalStream.Handler,
    callbacks: *Callbacks,

    const Action = terminal.StreamAction;

    pub fn deinit(self: *ShimHandler) void {
        self.stream_handler.deinit();
        self.* = undefined;
    }

    /// Fire the response callback with the given bytes.
    fn emitResponse(self: *ShimHandler, bytes: []const u8) void {
        if (self.callbacks.response) |cb|
            cb(self.callbacks.userdata, bytes.ptr, bytes.len);
    }

    pub fn vt(
        self: *ShimHandler,
        comptime action: Action.Tag,
        value: Action.Value(action),
    ) void {
        // Delegate terminal-state mutations to Ghostty's default terminal
        // stream handler. Side effects are still intercepted below.
        self.stream_handler.vt(action, value);

        // Intercept side-effect actions we expose through Rust callbacks.
        switch (action) {
            .bell => {
                if (self.callbacks.bell) |cb| cb(self.callbacks.userdata);
            },
            .window_title => {
                if (self.callbacks.title) |cb|
                    cb(self.callbacks.userdata, value.title.ptr, value.title.len);
            },

            // --- Device response actions ---
            .device_attributes => self.writeDeviceAttributes(value),
            .device_status => self.writeDeviceStatus(value.request),
            .request_mode => self.writeRequestMode(value.mode),
            .request_mode_unknown => self.writeRequestModeUnknown(value.mode, value.ansi),
            .kitty_keyboard_query => self.writeKittyKeyboardQuery(),
            .enquiry => self.writeEnquiry(),
            .size_report => self.writeSizeReport(value),

            else => {},
        }
    }

    fn writeDeviceAttributes(self: *ShimHandler, req: terminal.DeviceAttributeReq) void {
        // Ghostty includes `;52` (clipboard access) in DA1 conditionally on clipboard_write policy
        // TODO: Add clipboard support using OSC 52
        switch (req) {
            // VT220 level 2 conformance + color text
            .primary => self.emitResponse("\x1B[?62;22c"),
            .secondary => self.emitResponse("\x1B[>1;10;0c"),
            else => {},
        }
    }

    fn writeDeviceStatus(self: *ShimHandler, req: terminal.device_status.Request) void {
        // Based off Ghostty's StreamHandler.deviceStatusReport
        switch (req) {
            .operating_status => self.emitResponse("\x1B[0n"),
            .cursor_position => {
                const t = self.stream_handler.terminal;
                const x = t.screens.active.cursor.x;
                const y = t.screens.active.cursor.y;

                // Check origin mode — if set, report relative to scroll region.
                const report_x = if (t.modes.get(.origin))
                    x -| t.scrolling_region.left
                else
                    x;
                const report_y = if (t.modes.get(.origin))
                    y -| t.scrolling_region.top
                else
                    y;

                var buf: [32]u8 = undefined;
                const resp = std.fmt.bufPrint(&buf, "\x1B[{};{}R", .{
                    report_y + 1,
                    report_x + 1,
                }) catch return;
                self.emitResponse(resp);
            },
            // TODO: color_scheme — skipped for now
            else => {},
        }
    }

    fn writeRequestMode(self: *ShimHandler, mode: terminal.Mode) void {
        const tag: terminal.modes.ModeTag = @bitCast(@intFromEnum(mode));
        const code: u8 = if (self.stream_handler.terminal.modes.get(mode)) 1 else 2;

        var buf: [32]u8 = undefined;
        const resp = std.fmt.bufPrint(&buf, "\x1B[{s}{};{}$y", .{
            if (tag.ansi) "" else "?",
            tag.value,
            code,
        }) catch return;
        self.emitResponse(resp);
    }

    fn writeRequestModeUnknown(self: *ShimHandler, mode_raw: u16, ansi: bool) void {
        var buf: [32]u8 = undefined;
        const resp = std.fmt.bufPrint(&buf, "\x1B[{s}{};0$y", .{
            if (ansi) "" else "?",
            mode_raw,
        }) catch return;
        self.emitResponse(resp);
    }

    fn writeKittyKeyboardQuery(self: *ShimHandler) void {
        const flags = self.stream_handler.terminal.screens.active.kitty_keyboard.current();
        const int_flags: u5 = @bitCast(flags);

        var buf: [16]u8 = undefined;
        const resp = std.fmt.bufPrint(&buf, "\x1B[?{}u", .{@as(u8, int_flags)}) catch return;
        self.emitResponse(resp);
    }

    fn writeEnquiry(self: *ShimHandler) void {
        // TODO: Ghostty uses a configurable enquiry_response string.
        // Respond with empty string for now (programs have timeouts).
        _ = self;
    }
    fn writeSizeReport(self: *ShimHandler, style: terminal.SizeReportStyle) void {
        // Access cell dimensions from TerminalHandle via the Callbacks backpointer.
        const th = self.callbacks.handle orelse return;

        const t = self.stream_handler.terminal;
        const cols = t.cols;
        const rows = t.rows;

        switch (style) {
            .csi_14_t => {
                // Text area pixel size. Requires cell dimensions.
                if (th.cell_width_px == 0 or th.cell_height_px == 0) return;
                const width_px = @as(u32, cols) * @as(u32, th.cell_width_px);
                const height_px = @as(u32, rows) * @as(u32, th.cell_height_px);
                var buf: [48]u8 = undefined;
                const resp = std.fmt.bufPrint(&buf, "\x1B[4;{};{}t", .{ height_px, width_px }) catch return;
                self.emitResponse(resp);
            },
            .csi_16_t => {
                // Cell pixel size.
                if (th.cell_width_px == 0 or th.cell_height_px == 0) return;
                var buf: [32]u8 = undefined;
                const resp = std.fmt.bufPrint(&buf, "\x1B[6;{};{}t", .{ th.cell_height_px, th.cell_width_px }) catch return;
                self.emitResponse(resp);
            },
            .csi_18_t => {
                // Grid size in cells.
                var buf: [32]u8 = undefined;
                const resp = std.fmt.bufPrint(&buf, "\x1B[8;{};{}t", .{ rows, cols }) catch return;
                self.emitResponse(resp);
            },
            .csi_21_t => {
                // TODO: Title report — skipped for now
                // Ghostty delegates this to surfaceMessageWriter.
                // We'd need a separate callback for window title reporting.
            },
        }
    }
};

// --- TerminalHandle: owns all terminal state ---
pub const TerminalHandle = struct {
    alloc: Allocator,
    terminal_inst: terminal.Terminal,
    stream: terminal.Stream(*ShimHandler),
    handler: ShimHandler,
    callbacks: Callbacks,
    render_state: terminal.RenderState,
    /// Cell pixel dimensions — set by Rust via ghostty_terminal_set_cell_size().
    /// Used for size report responses (CSI 14t, CSI 16t).
    /// Zero means "not yet measured" — size reports that need pixel info
    /// will be skipped until the renderer provides real values.
    cell_width_px: u16 = 0,
    cell_height_px: u16 = 0,

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
            .callbacks = .{},
            .handler = .{
                .stream_handler = .{ .terminal = undefined },
                .callbacks = undefined,
            },
            .stream = undefined,
            .render_state = .empty,
        };

        // Fix up self-referential pointers
        handle.handler.stream_handler.terminal = &handle.terminal_inst;
        handle.handler.callbacks = &handle.callbacks;
        handle.stream = terminal.Stream(*ShimHandler).initAlloc(alloc, &handle.handler);

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
