mod terminal;
mod types;

use core::ffi::{c_int, c_void};
use std::ptr::NonNull;
pub(crate) use types::{BellCallback, OutputCallback, TitleCallback};

pub use terminal::{CallbackHandle, Event, RenderFrame, SelectionText, Terminal};
pub use types::{
    CellStyle, ColorRGB, CursorState, DirtyState, GraphemeSlice, RawCell, RenderColors,
    ScrollbarInfo, StyleColor, TerminalDimensions,
};

unsafe extern "C" {
    pub(crate) fn ghostty_terminal_new(
        cols: u16,
        rows: u16,
        fg_r: u8,
        fg_g: u8,
        fg_b: u8,
        bg_r: u8,
        bg_g: u8,
        bg_b: u8,
    ) -> *mut c_void;
    pub(crate) fn ghostty_terminal_free(terminal: NonNull<c_void>);
    pub(crate) fn ghostty_terminal_lock(terminal: NonNull<c_void>);
    pub(crate) fn ghostty_terminal_unlock(terminal: NonNull<c_void>);

    pub(crate) fn ghostty_terminal_set_callbacks(
        terminal: NonNull<c_void>,
        userdata: *mut c_void,
        bell: Option<BellCallback>,
        title: Option<TitleCallback>,
        output: Option<OutputCallback>,
    );

    pub(crate) fn ghostty_terminal_resize(terminal: NonNull<c_void>, cols: u16, rows: u16)
    -> c_int;

    pub(crate) fn ghostty_terminal_set_render_dimensions(
        terminal: NonNull<c_void>,
        screen_width_px: u32,
        screen_height_px: u32,
        cell_width_px: u32,
        cell_height_px: u32,
    );

    pub(crate) fn ghostty_terminal_is_synchronized_output(terminal: NonNull<c_void>) -> bool;
    pub(crate) fn ghostty_terminal_reset_synchronized_output(terminal: NonNull<c_void>);
    pub(crate) fn ghostty_terminal_is_focus_event_mode(terminal: NonNull<c_void>) -> bool;
    pub(crate) fn ghostty_terminal_is_alternate_screen(terminal: NonNull<c_void>) -> bool;
    pub(crate) fn ghostty_terminal_is_mouse_reporting(terminal: NonNull<c_void>) -> bool;
    pub(crate) fn ghostty_terminal_is_mouse_alternate_scroll(terminal: NonNull<c_void>) -> bool;

    pub(crate) fn ghostty_terminal_scroll_viewport(terminal: NonNull<c_void>, delta: i32);
    pub(crate) fn ghostty_terminal_scroll_viewport_top(terminal: NonNull<c_void>);
    pub(crate) fn ghostty_terminal_scroll_viewport_bottom(terminal: NonNull<c_void>);
    pub(crate) fn ghostty_terminal_scrollbar_info(
        terminal: NonNull<c_void>,
        out: NonNull<ScrollbarInfo>,
    );
    pub(crate) fn ghostty_terminal_viewport_is_bottom(terminal: NonNull<c_void>) -> bool;
    pub(crate) fn ghostty_terminal_scroll_to_row(terminal: NonNull<c_void>, row: u64);

    pub(crate) fn ghostty_terminal_render_update(terminal: NonNull<c_void>) -> u8;
    pub(crate) fn ghostty_terminal_render_dirty(terminal: NonNull<c_void>) -> u8;
    pub(crate) fn ghostty_terminal_render_clear_dirty(terminal: NonNull<c_void>);
    pub(crate) fn ghostty_terminal_render_rows(terminal: NonNull<c_void>) -> u16;
    pub(crate) fn ghostty_terminal_render_cols(terminal: NonNull<c_void>) -> u16;
    pub(crate) fn ghostty_terminal_render_row_dirty(terminal: NonNull<c_void>, row: u16) -> bool;

    pub(crate) fn ghostty_terminal_render_cursor(
        terminal: NonNull<c_void>,
        out: NonNull<CursorState>,
    );
    pub(crate) fn ghostty_terminal_render_colors(terminal: NonNull<c_void>) -> *const RenderColors;
    pub(crate) fn ghostty_terminal_render_row_raw(
        terminal: NonNull<c_void>,
        row: u16,
        out_len: NonNull<u16>,
    ) -> *const u64;

    pub(crate) fn ghostty_terminal_render_row_styles(
        terminal: NonNull<c_void>,
        row: u16,
        out_len: NonNull<u16>,
    ) -> *const CellStyle;

    pub(crate) fn ghostty_terminal_render_row_graphemes(
        terminal: NonNull<c_void>,
        row: u16,
        out_len: NonNull<u16>,
    ) -> *const GraphemeSlice;
    pub(crate) fn ghostty_terminal_render_row_selection(
        terminal: NonNull<c_void>,
        row: u16,
        start_x: NonNull<u16>,
        end_x: NonNull<u16>,
    ) -> bool;

    pub(crate) fn ghostty_terminal_set_selection(
        terminal: NonNull<c_void>,
        start_x: u16,
        start_y: u32,
        end_x: u16,
        end_y: u32,
        rectangular: u8,
    ) -> c_int;
    pub(crate) fn ghostty_terminal_select_word_at(
        terminal: NonNull<c_void>,
        x: u16,
        y: u32,
    ) -> c_int;
    pub(crate) fn ghostty_terminal_select_line_at(
        terminal: NonNull<c_void>,
        x: u16,
        y: u32,
    ) -> c_int;
    pub(crate) fn ghostty_terminal_select_output_at(
        terminal: NonNull<c_void>,
        x: u16,
        y: u32,
    ) -> c_int;
    pub(crate) fn ghostty_terminal_select_word_drag(
        terminal: NonNull<c_void>,
        click_x: u16,
        click_y: u32,
        drag_x: u16,
        drag_y: u32,
    ) -> c_int;
    pub(crate) fn ghostty_terminal_select_line_drag(
        terminal: NonNull<c_void>,
        click_x: u16,
        click_y: u32,
        drag_x: u16,
        drag_y: u32,
    ) -> c_int;
    pub(crate) fn ghostty_terminal_clear_selection(terminal: NonNull<c_void>);
    pub(crate) fn ghostty_terminal_get_selection_text(
        terminal: NonNull<c_void>,
        out_len: NonNull<usize>,
    ) -> *const u8;
    pub(crate) fn ghostty_terminal_bytes_free(
        terminal: NonNull<c_void>,
        bytes: NonNull<u8>,
        len: usize,
    );
}

#[cfg(test)]
mod tests {
    mod core;
    mod render;
    mod safe_terminal;
    mod scroll;
    mod zig_shim_tests;
}
