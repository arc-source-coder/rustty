mod ffi;
pub mod font;
pub mod sprite;
mod terminal;
mod types;
mod zig;

use core::ffi::{c_int, c_void};
use std::ptr::NonNull;
pub(crate) use types::{BellCallback, OutputCallback, TitleCallback};

pub use ffi::{
    BoldColor, CellStyle, Color, ColorRGB, CursorViewport, CursorVisualStyle, OptionalColorRGB,
    OptionalCursorViewport, OptionalSelection, RenderColors, RenderCursor, StyleColor, U21,
    UnderlineStyle,
};
pub use terminal::{CallbackHandle, RenderFrame, Terminal, TerminalEvent};
pub use types::{
    CellSize, ContentTag, CursorCoordinate, Dirty, MouseMode, Padding, RawCell, ScreenSize,
    ScrollbarInfo, SelectionUpdate, TerminalDimensions, Width,
};
pub use zig::{CellView, GraphemeView, RowView, ZigMultiArrayList};

unsafe extern "C" {
    fn ghostty_terminal_new(cols: u16, rows: u16, fg: u32, bg: u32) -> *mut c_void;
    fn ghostty_terminal_free(terminal: NonNull<c_void>);
    fn ghostty_terminal_lock(terminal: NonNull<c_void>);
    fn ghostty_terminal_unlock(terminal: NonNull<c_void>);

    fn ghostty_terminal_set_callbacks(
        terminal: NonNull<c_void>,
        userdata: *mut c_void,
        bell: Option<BellCallback>,
        title: Option<TitleCallback>,
        output: Option<OutputCallback>,
    );

    fn ghostty_terminal_feed(terminal: NonNull<c_void>, bytes: *const u8, len: usize);

    fn ghostty_terminal_resize(terminal: NonNull<c_void>, cols: u16, rows: u16) -> c_int;

    fn ghostty_terminal_set_screen_dimensions(terminal: NonNull<c_void>, size: ScreenSize);
    fn ghostty_terminal_set_cell_dimensions(terminal: NonNull<c_void>, size: CellSize);
    fn ghostty_terminal_get_render_dimensions(terminal: NonNull<c_void>) -> TerminalDimensions;

    fn ghostty_terminal_is_synchronized_output(terminal: NonNull<c_void>) -> bool;
    fn ghostty_terminal_reset_synchronized_output(terminal: NonNull<c_void>);
    fn ghostty_terminal_is_focus_event_mode(terminal: NonNull<c_void>) -> bool;
    fn ghostty_terminal_get_mouse_mode(terminal: NonNull<c_void>) -> MouseMode;

    fn ghostty_terminal_scroll_viewport(terminal: NonNull<c_void>, delta: i32);
    fn ghostty_terminal_scroll_viewport_top(terminal: NonNull<c_void>);
    fn ghostty_terminal_scroll_viewport_bottom(terminal: NonNull<c_void>);
    fn ghostty_terminal_scrollbar_info(terminal: NonNull<c_void>) -> ScrollbarInfo;
    fn ghostty_terminal_viewport_is_bottom(terminal: NonNull<c_void>) -> bool;
    fn ghostty_terminal_scroll_to_row(terminal: NonNull<c_void>, row: u64);

    fn ghostty_terminal_render_update(terminal: NonNull<c_void>) -> u8;
    fn ghostty_terminal_render_dirty(terminal: NonNull<c_void>) -> c_int;
    fn ghostty_terminal_render_clear_dirty(terminal: NonNull<c_void>);
    fn ghostty_terminal_get_dimensions(terminal: NonNull<c_void>, rows: &mut u16, cols: &mut u16);

    fn ghostty_terminal_render_cursor(terminal: NonNull<c_void>) -> *const RenderCursor;
    fn ghostty_terminal_render_colors(terminal: NonNull<c_void>) -> *const RenderColors;

    fn ghostty_terminal_render_row_data(terminal: NonNull<c_void>) -> *const ZigMultiArrayList;

    fn ghostty_terminal_gesture_press(
        terminal: NonNull<c_void>,
        x_px: f32,
        y_px: f32,
        ctrl_or_super: bool,
        shift: bool,
        rectangular: bool,
    ) -> SelectionUpdate;
    fn ghostty_terminal_gesture_release(terminal: NonNull<c_void>, x_px: f32, y_px: f32);
    fn ghostty_terminal_gesture_drag(
        terminal: NonNull<c_void>,
        x_px: f32,
        y_px: f32,
        rectangular: bool,
    ) -> SelectionUpdate;
    fn ghostty_terminal_gesture_autoscroll_tick(
        terminal: NonNull<c_void>,
        x_px: f32,
        y_px: f32,
        rectangular: bool,
    ) -> SelectionUpdate;
    fn ghostty_terminal_gesture_reset(terminal: NonNull<c_void>);

    fn ghostty_terminal_clear_selection(terminal: NonNull<c_void>) -> bool;
    fn ghostty_terminal_take_selection_text(
        terminal: NonNull<c_void>,
        out_len: &mut usize,
    ) -> *const u8;

    fn ghostty_terminal_bytes_free(terminal: NonNull<c_void>, bytes: *const u8, len: usize);
}

#[cfg(test)]
mod tests {
    mod terminal;
}
