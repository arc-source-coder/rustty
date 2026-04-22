const std = @import("std");
const Allocator = std.mem.Allocator;

const terminal = @import("../ghostty/src/terminal/main.zig");
const color = terminal.color;

const renderer_dimensions = @import("../ghostty/src/renderer/size.zig");

// --- Callback function pointer types ---
pub const BellCallback = *const fn (?*anyopaque) callconv(.c) void;
pub const TitleCallback = *const fn (?*anyopaque, [*]const u8, usize) callconv(.c) void;
pub const OutputCallback = *const fn (?*anyopaque) callconv(.c) void;
pub const WriteInputCallback = *const fn (?*anyopaque, [*]const u8, usize) callconv(.c) void;

const EventCallbacks = struct {
    userdata: ?*anyopaque = null,
    bell: ?BellCallback = null,
    title: ?TitleCallback = null,
};

const OutputCallbackState = struct {
    userdata: ?*anyopaque = null,
    output: ?OutputCallback = null,
};

const WriteInput = struct {
    userdata: ?*anyopaque = null,
    callback: ?WriteInputCallback = null,
};

// --- TerminalHandle: owns all terminal state ---
pub const TerminalHandle = struct {
    alloc: Allocator,
    mutex: std.Thread.Mutex = .{},
    terminal_inst: terminal.Terminal,
    handler: terminal.TerminalStream.Handler,
    stream: terminal.TerminalStream,
    event_callbacks: EventCallbacks,
    output_callback: OutputCallbackState,
    write_input: WriteInput,
    render_state: terminal.RenderState,

    size: renderer_dimensions.Size,

    inline fn fromEffectsHandler(handler_ptr: *terminal.TerminalStream.Handler) *TerminalHandle {
        const stream_ptr: *terminal.TerminalStream = @fieldParentPtr("handler", handler_ptr);
        return @fieldParentPtr("stream", stream_ptr);
    }

    fn writePtyTrampoline(handler_ptr: *terminal.TerminalStream.Handler, data: [:0]const u8) void {
        const handle = fromEffectsHandler(handler_ptr);
        const callback = handle.write_input.callback orelse return;
        callback(handle.write_input.userdata, data.ptr, data.len);
    }

    pub inline fn lock(self: *TerminalHandle) void {
        self.mutex.lock();
    }

    pub inline fn unlock(self: *TerminalHandle) void {
        self.mutex.unlock();
    }

    fn bellTrampoline(handler_ptr: *terminal.TerminalStream.Handler) void {
        const handle = fromEffectsHandler(handler_ptr);
        const cb = handle.event_callbacks.bell orelse return;
        cb(handle.event_callbacks.userdata);
    }

    pub fn outputTrampoline(self: *TerminalHandle) void {
        const cb = self.output_callback.output orelse return;
        cb(self.output_callback.userdata);
    }

    fn titleChangedTrampoline(handler_ptr: *terminal.TerminalStream.Handler) void {
        const handle = fromEffectsHandler(handler_ptr);
        const cb = handle.event_callbacks.title orelse return;
        const title = handler_ptr.terminal.getTitle() orelse "";
        cb(handle.event_callbacks.userdata, title.ptr, title.len);
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
        if (handle.size.cell.width == 0 or handle.size.cell.height == 0) return null;

        return .{
            .rows = handler_ptr.terminal.rows,
            .columns = handler_ptr.terminal.cols,
            .cell_width = handle.size.cell.width,
            .cell_height = handle.size.cell.height,
        };
    }

    pub fn init(alloc: Allocator, cols: u16, rows: u16, fg: color.RGB, bg: color.RGB) !*TerminalHandle {
        const handle = try alloc.create(TerminalHandle);
        errdefer alloc.destroy(handle);

        const t = try terminal.Terminal.init(alloc, .{
            .cols = cols,
            .rows = rows,
            .max_scrollback = 10_000_000,
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
            .event_callbacks = .{},
            .output_callback = .{},
            .write_input = .{},
            .render_state = .empty,

            // The Rust renderer will set these values via
            // ghostty_terminal_set_render_dimensions().
            .size = .{
                .screen = .{
                    .width = 0,
                    .height = 0,
                },
                .cell = .{
                    .width = 0,
                    .height = 0,
                },
                .padding = .{
                    .top = 0,
                    .bottom = 0,
                    .right = 0,
                    .left = 0,
                },
            },
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
