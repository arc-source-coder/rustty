const std = @import("std");
const Allocator = std.mem.Allocator;

const terminal = @import("ghostty/src/terminal/main.zig");
const StyleFlags = @TypeOf((@as(terminal.Style, .{})).flags);

// --- Callback function pointer types ---
pub const BellCallback = *const fn (?*anyopaque) callconv(.c) void;
pub const TitleCallback = *const fn (?*anyopaque, [*]const u8, usize) callconv(.c) void;

const Callbacks = struct {
    userdata: ?*anyopaque = null,
    bell: ?BellCallback = null,
    title: ?TitleCallback = null,
};

// --- ShimHandler: wraps ReadonlyHandler + fires callbacks ---
const ShimHandler = struct {
    readonly: terminal.ReadonlyHandler,
    callbacks: *Callbacks,

    const Action = terminal.StreamAction;

    pub fn deinit(self: *ShimHandler) void {
        self.readonly.deinit();
        self.* = undefined;
    }

    pub fn vt(
        self: *ShimHandler,
        comptime action: Action.Tag,
        value: Action.Value(action),
    ) !void {
        // Delegate ALL state mutation to ReadonlyHandler.
        // ReadonlyHandler no-ops on side-effect actions, so this is safe.
        try self.readonly.vt(action, value);

        // Intercept side-effect actions that ReadonlyHandler ignores.
        switch (action) {
            .bell => {
                if (self.callbacks.bell) |cb| cb(self.callbacks.userdata);
            },
            .window_title => {
                if (self.callbacks.title) |cb|
                    cb(self.callbacks.userdata, value.title.ptr, value.title.len);
            },
            else => {},
        }
    }
};

/// C-safe flattened cell for rendering
pub const FlatCell = extern struct {
    /// Primary codepoint (0 = empty cell)
    codepoint: u32,
    /// Number of extra codepoints in the grapheme cluster (0 for simple chars)
    grapheme_len: u8,
    /// Wide property: 0=narrow, 1=wide, 2=spacer_tail, 3=spacer_head
    wide: u8,

    // --- Style (resolved from style.Style) ---
    /// Foreground color type: 0=none/default, 1=palette, 2=rgb
    fg_color_type: u8,
    fg_r: u8,
    fg_g: u8,
    fg_b: u8,
    fg_palette: u8,

    /// Background color type: 0=none/default, 1=palette, 2=rgb
    /// Note: for bg_color_palette/bg_color_rgb content_tags, bg is set
    /// from the cell content directly (not from style).
    bg_color_type: u8,
    bg_r: u8,
    bg_g: u8,
    bg_b: u8,
    bg_palette: u8,

    /// Underline color type: 0=none, 1=palette, 2=rgb
    ul_color_type: u8,
    ul_r: u8,
    ul_g: u8,
    ul_b: u8,
    ul_palette: u8,

    /// Style flags packed into a u16 matching Zig's Style.Flags layout:
    /// bit 0: bold, 1: italic, 2: faint, 3: blink, 4: inverse,
    /// 5: invisible, 6: strikethrough, 7: overline
    /// bits 8-10: underline (0=none,1=single,2=double,3=curly,4=dotted,5=dashed)
    style_flags: u16,

    _padding: [2]u8,

    // Verify ABI stability at compile time
    comptime {
        std.debug.assert(@sizeOf(FlatCell) == 28);
        std.debug.assert(@alignOf(FlatCell) == 4);
        // Verify style_flags bitcast stays u16-sized without referencing
        // private Ghostty internals by name.
        std.debug.assert(@bitSizeOf(StyleFlags) == 16);
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
    flat_cells: []FlatCell = &.{},
    grapheme_buf: []u32 = &.{},

    pub fn ensureFlatCells(self: *TerminalHandle, cols: u16) !void {
        if (self.flat_cells.len >= cols) return;
        if (self.flat_cells.len > 0) self.alloc.free(self.flat_cells);
        self.flat_cells = try self.alloc.alloc(FlatCell, cols);
    }

    pub fn copyGraphemeToBuf(self: *TerminalHandle, grapheme: []const u21) !void {
        if (self.grapheme_buf.len < grapheme.len) {
            if (self.grapheme_buf.len > 0) self.alloc.free(self.grapheme_buf);
            self.grapheme_buf = try self.alloc.alloc(u32, grapheme.len);
        }
        for (grapheme, 0..) |cp, i| {
            self.grapheme_buf[i] = cp;
        }
    }

    pub fn init(alloc: Allocator, cols: u16, rows: u16) !*TerminalHandle {
        const handle = try alloc.create(TerminalHandle);
        errdefer alloc.destroy(handle);

        const t = try terminal.Terminal.init(alloc, .{
            .cols = cols,
            .rows = rows,
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
                .readonly = .{ .terminal = undefined },
                .callbacks = undefined,
            },
            .stream = undefined,
            .render_state = .empty,
        };

        // Fix up self-referential pointers
        handle.handler.readonly.terminal = &handle.terminal_inst;
        handle.handler.callbacks = &handle.callbacks;
        handle.stream = terminal.Stream(*ShimHandler).initAlloc(alloc, &handle.handler);

        return handle;
    }

    pub fn deinit(self: *TerminalHandle) void {
        const alloc = self.alloc;
        if (self.flat_cells.len > 0) alloc.free(self.flat_cells);
        if (self.grapheme_buf.len > 0) alloc.free(self.grapheme_buf);
        self.render_state.deinit(alloc);
        self.stream.deinit();
        self.terminal_inst.deinit(alloc);
        self.* = undefined;
        alloc.destroy(self);
    }
};
