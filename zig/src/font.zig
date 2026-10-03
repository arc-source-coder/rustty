//! In-progress bindings to Ghostty font functions
const std = @import("std");
const feature_mod = @import("../ghostty/src/font/shaper/feature.zig");

const FeatureList = feature_mod.FeatureList;
const Feature = feature_mod.Feature;
const default_features = feature_mod.default_features;

const allocator = std.heap.smp_allocator;

pub export fn ghostty_font_parse_features(
    bytes: [*]const u8,
    len: usize,
    // Safety: Rust passes a &mut usize reference, which is noalias
    noalias out_len: *usize,
) callconv(.c) ?[*]Feature {
    var feature_list: FeatureList = .{};
    defer feature_list.deinit(allocator);
    feature_list.features.appendSlice(allocator, &default_features) catch return null;
    feature_list.appendFromString(allocator, bytes[0..len]) catch return null;
    const features = feature_list.features.toOwnedSlice(allocator) catch return null;
    out_len.* = features.len;
    return features.ptr;
}

pub export fn ghostty_font_bytes_free(bytes: [*]const Feature, len: usize) callconv(.c) void {
    allocator.free(bytes[0..len]);
}
