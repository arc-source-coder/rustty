const std = @import("std");
const font = @import("../ghostty/src/font/main.zig");
const special = @import("../ghostty/src/font/sprite/draw/special.zig");

const Canvas = font.sprite.Canvas;
const Metrics = font.Metrics;
const Sprite = font.sprite.Sprite;

const SpriteFace = font.SpriteFace;

const sprite_allocator = std.heap.smp_allocator;

const Range = struct {
    min: u32,
    max: u32,
    draw: DrawFn,
};

const DrawFn = SpriteFace.DrawFn;
const DrawFnError = SpriteFace.DrawFnError;

const ranges: []const Range = ranges: {
    @setEvalBranchQuota(1_000_000);

    const draw_modules: [8]type = .{
        @import("../ghostty/src/font/sprite/draw/block.zig"),
        @import("../ghostty/src/font/sprite/draw/box.zig"),
        @import("../ghostty/src/font/sprite/draw/braille.zig"),
        @import("../ghostty/src/font/sprite/draw/branch.zig"),
        @import("../ghostty/src/font/sprite/draw/geometric_shapes.zig"),
        @import("../ghostty/src/font/sprite/draw/powerline.zig"),
        @import("../ghostty/src/font/sprite/draw/symbols_for_legacy_computing.zig"),
        @import("../ghostty/src/font/sprite/draw/symbols_for_legacy_computing_supplement.zig"),
    };

    var range_count: usize = 0;
    for (draw_modules) |module| {
        for (@typeInfo(module).@"struct".decls) |decl| {
            if (!@hasDecl(module, decl.name)) continue;
            if (!std.mem.startsWith(u8, decl.name, "draw")) continue;

            range_count += 1;
        }
    }

    var result: [range_count]Range = undefined;
    var names: [range_count][:0]const u8 = undefined;
    var index: usize = 0;

    for (draw_modules) |module| {
        for (@typeInfo(module).@"struct".decls) |decl| {
            if (!@hasDecl(module, decl.name)) continue;
            if (!std.mem.startsWith(u8, decl.name, "draw")) continue;

            // Everything after "draw" is hexadecimal.
            // An underscore separates the lower and upper bounds.
            const sep = std.mem.indexOfScalar(u8, decl.name, '_') orelse decl.name.len;

            const min = std.fmt.parseInt(u21, decl.name[4..sep], 16) catch unreachable;
            const max = blk: {
                if (sep == decl.name.len) break :blk min;
                break :blk std.fmt.parseInt(u21, decl.name[sep + 1 ..], 16) catch unreachable;
            };

            result[index] = .{
                .min = min,
                .max = max,
                .draw = @field(module, decl.name),
            };
            names[index] = decl.name;
            index += 1;
        }
    }

    // Sort ranges in ascending order
    const SortContext = struct {
        ranges: []Range,
        names: [][:0]const u8,

        pub fn lessThan(self: @This(), a: usize, b: usize) bool {
            return self.ranges[a].min < self.ranges[b].min;
        }

        pub fn swap(self: @This(), a: usize, b: usize) void {
            std.mem.swap(Range, &self.ranges[a], &self.ranges[b]);
            std.mem.swap([:0]const u8, &self.names[a], &self.names[b]);
        }
    };

    const context: SortContext = .{ .ranges = &result, .names = &names };
    std.mem.sortUnstableContext(0, result.len, context);

    var i = 0;
    for (result, 0..) |range, k| {
        if (range.min <= i) {
            @compileError(std.fmt.comptimePrint(
                "Codepoint range for {s}(...) overlaps range for {s}(...), {X} <= {X} <= {X}",
                .{ names[k], names[k - 1], result[k - 1].min, range.min, result[k - 1].max },
            ));
        }
        i = range.max;
    }

    const a = result;
    break :ranges &a;
};

fn getDrawFn(cp: u32) ?*const DrawFn {
    if (cp >= Sprite.start) switch (std.enums.fromInt(Sprite, cp) orelse return null) {
        inline else => |tag| {
            return @field(special, @tagName(tag));
        },
    };

    inline for (ranges) |range| {
        if (cp >= range.min and cp <= range.max) return range.draw;
    }

    return null;
}

const SpriteRasterizer = struct {
    metrics: font.Metrics,
    arena: std.heap.ArenaAllocator,

    pub fn init(allocator: std.mem.Allocator, metrics: font.Metrics) SpriteRasterizer {
        return .{
            .metrics = metrics,
            .arena = .init(allocator),
        };
    }

    pub fn rasterize(self: *SpriteRasterizer, cp: u32, cell_width: u2) !SpriteBitmap {
        _ = self.arena.reset(.retain_capacity);
        const alloc = self.arena.allocator();

        const width = switch (cell_width) {
            0, 1 => self.metrics.cell_width,
            2, 3 => self.metrics.cell_width * cell_width,
        };
        const height = self.metrics.cell_height;
        const padding_x = width / 4;
        const padding_y = height / 4;

        var canvas = try Canvas.init(alloc, width, height, padding_x, padding_y);
        const draw = getDrawFn(cp) orelse return error.UnsupportedSprite;
        try draw(cp, &canvas, width, height, self.metrics);

        const sfc_width: u32 = @intCast(canvas.sfc.getWidth());
        const sfc_height: u32 = @intCast(canvas.sfc.getHeight());

        const pixels = std.mem.sliceAsBytes(canvas.sfc.image_surface_alpha8.buf);

        top: while (canvas.clip_top < sfc_height - canvas.clip_bottom) {
            const y = canvas.clip_top;
            const x0 = canvas.clip_left;
            const x1 = sfc_width - canvas.clip_right;
            for (pixels[y * sfc_width ..][x0..x1]) |v| {
                if (v != 0) break :top;
            }
            canvas.clip_top += 1;
        }

        bottom: while (canvas.clip_bottom < sfc_height - canvas.clip_top) {
            const y = sfc_height - canvas.clip_bottom -| 1;
            const x0 = canvas.clip_left;
            const x1 = sfc_width - canvas.clip_right;
            for (pixels[y * sfc_width ..][x0..x1]) |v| {
                if (v != 0) break :bottom;
            }
            canvas.clip_bottom += 1;
        }

        left: while (canvas.clip_left < sfc_width - canvas.clip_right) {
            const x = canvas.clip_left;
            const y0 = canvas.clip_top;
            const y1 = sfc_height - canvas.clip_bottom;
            for (y0..y1) |y| {
                if (pixels[y * sfc_width + x] != 0) break :left;
            }
            canvas.clip_left += 1;
        }

        right: while (canvas.clip_right < sfc_width - canvas.clip_left) {
            const x = sfc_width - canvas.clip_right -| 1;
            const y0 = canvas.clip_top;
            const y1 = sfc_height - canvas.clip_bottom;
            for (y0..y1) |y| {
                if (pixels[y * sfc_width + x] != 0) break :right;
            }
            canvas.clip_right += 1;
        }

        const region_width = sfc_width -| canvas.clip_left -| canvas.clip_right;
        const region_height = sfc_height -| canvas.clip_top -| canvas.clip_bottom;

        const ptr = ptr: {
            if (region_width == 0 or region_height == 0) break :ptr pixels.ptr;

            const row_offset = @as(usize, canvas.clip_top) * @as(usize, sfc_width);
            break :ptr pixels.ptr + row_offset + @as(usize, canvas.clip_left);
        };

        return .{
            .pixels = ptr,
            .stride = sfc_width,
            .width = region_width,
            .height = region_height,
            .offset_x = @as(i32, @intCast(canvas.clip_left)) - @as(i32, @intCast(padding_x)),
            .offset_y = @as(i32, @intCast(canvas.clip_top)) - @as(i32, @intCast(padding_y)),
        };
    }

    pub fn deinit(self: *SpriteRasterizer) void {
        self.arena.deinit();
        self.* = undefined;
    }
};

/// FFI mirror of Ghostty's `font.Metrics`
const SpriteMetrics = extern struct {
    cell_width: u32,
    cell_height: u32,
    cell_baseline: u32,

    underline_position: u32,
    underline_thickness: u32,

    strikethrough_position: u32,
    strikethrough_thickness: u32,

    overline_position: i32,
    overline_thickness: u32,

    box_thickness: u32,
    cursor_thickness: u32,
    cursor_height: u32,

    icon_height: f64,
    icon_height_single: f64,
    face_width: f64,
    face_height: f64,
    face_y: f64,
};

const SpriteBitmap = extern struct {
    pixels: [*]const u8,
    stride: u32,
    width: u32,
    height: u32,
    offset_x: i32,
    offset_y: i32,
};

pub export fn ghostty_sprite_has_codepoint(cp: u32) callconv(.c) bool {
    return getDrawFn(cp) != null;
}

pub export fn ghostty_sprite_rasterizer_new(metrics: *const SpriteMetrics) callconv(.c) ?*anyopaque {
    const font_metrics: font.Metrics = .{
        .cell_width = metrics.cell_width,
        .cell_height = metrics.cell_height,
        .cell_baseline = metrics.cell_baseline,

        .underline_position = metrics.underline_position,
        .underline_thickness = metrics.underline_thickness,

        .overline_position = metrics.overline_position,
        .overline_thickness = metrics.overline_thickness,

        .strikethrough_position = metrics.strikethrough_position,
        .strikethrough_thickness = metrics.strikethrough_thickness,

        .box_thickness = metrics.box_thickness,
        .cursor_thickness = metrics.cursor_thickness,
        .cursor_height = metrics.cursor_height,

        .icon_height = metrics.icon_height,
        .icon_height_single = metrics.icon_height_single,

        .face_width = metrics.face_width,
        .face_height = metrics.face_height,
        .face_y = metrics.face_y,
    };

    const rasterizer = sprite_allocator.create(SpriteRasterizer) catch return null;
    errdefer sprite_allocator.destroy(rasterizer);

    rasterizer.* = SpriteRasterizer.init(sprite_allocator, font_metrics);

    return @ptrCast(rasterizer);
}

pub export fn ghostty_sprite_rasterizer_free(ptr: *anyopaque) callconv(.c) void {
    const rasterizer: *SpriteRasterizer = @ptrCast(@alignCast(ptr));
    rasterizer.deinit();
    sprite_allocator.destroy(rasterizer);
}

pub export fn ghostty_sprite_rasterize(
    ptr: *anyopaque,
    cp: u32,
    cell_width: u8,
    // Safety: Rust passes &mut MaybeUninit<RawSpriteBitmap> to this
    // function, which guarantees noalias.
    noalias out: *SpriteBitmap,
) callconv(.c) c_int {
    const rasterizer: *SpriteRasterizer = @ptrCast(@alignCast(ptr));
    out.* = rasterizer.rasterize(cp, @truncate(cell_width)) catch return 1;
    return 0;
}
