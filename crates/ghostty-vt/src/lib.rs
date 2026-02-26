mod terminal;
mod types;

use core::ffi::{c_int, c_void};
pub(crate) use types::{BellCallback, TitleCallback};

pub use terminal::{RenderFrame, SelectionText, Terminal, VtEvent};
pub use types::{ColorRGB, ColorState, CursorState, DirtyState, FlatCell, MouseFormat, MouseMode};

unsafe extern "C" {
    pub(crate) fn ghostty_vt_terminal_new(cols: u16, rows: u16) -> *mut c_void;
    pub(crate) fn ghostty_vt_terminal_free(terminal: *mut c_void);

    pub(crate) fn ghostty_vt_terminal_set_callbacks(
        terminal: *mut c_void,
        userdata: *mut c_void,
        bell: Option<BellCallback>,
        title: Option<TitleCallback>,
    );

    pub(crate) fn ghostty_vt_terminal_feed(
        terminal: *mut c_void,
        bytes: *const u8,
        len: usize,
    ) -> c_int;

    pub(crate) fn ghostty_vt_terminal_resize(terminal: *mut c_void, cols: u16, rows: u16) -> c_int;

    pub(crate) fn ghostty_vt_terminal_get_mouse_mode(terminal: *mut c_void) -> u8;
    pub(crate) fn ghostty_vt_terminal_get_mouse_format(terminal: *mut c_void) -> u8;
    pub(crate) fn ghostty_vt_terminal_is_bracketed_paste(terminal: *mut c_void) -> u8;
    pub(crate) fn ghostty_vt_terminal_get_kitty_keyboard_flags(terminal: *mut c_void) -> u8;

    pub(crate) fn ghostty_vt_terminal_scroll_viewport(terminal: *mut c_void, delta: i32);
    pub(crate) fn ghostty_vt_terminal_scroll_viewport_top(terminal: *mut c_void);
    pub(crate) fn ghostty_vt_terminal_scroll_viewport_bottom(terminal: *mut c_void);

    pub(crate) fn ghostty_vt_terminal_render_update(terminal: *mut c_void) -> c_int;
    pub(crate) fn ghostty_vt_terminal_render_dirty(terminal: *mut c_void) -> u8;
    pub(crate) fn ghostty_vt_terminal_render_clear_dirty(terminal: *mut c_void);
    pub(crate) fn ghostty_vt_terminal_render_rows(terminal: *mut c_void) -> u16;
    pub(crate) fn ghostty_vt_terminal_render_cols(terminal: *mut c_void) -> u16;
    pub(crate) fn ghostty_vt_terminal_render_row_dirty(terminal: *mut c_void, row: u16) -> u8;

    pub(crate) fn ghostty_vt_terminal_render_cursor(
        terminal: *mut c_void,
        out: *mut CursorState,
    ) -> c_int;
    pub(crate) fn ghostty_vt_terminal_render_colors(
        terminal: *mut c_void,
        out: *mut ColorState,
    ) -> c_int;
    pub(crate) fn ghostty_vt_terminal_render_palette_color(
        terminal: *mut c_void,
        index: u8,
        out: *mut ColorRGB,
    ) -> c_int;

    pub(crate) fn ghostty_vt_terminal_render_row_cells(
        terminal: *mut c_void,
        row: u16,
        out_len: *mut u16,
    ) -> *const FlatCell;
    pub(crate) fn ghostty_vt_terminal_render_cell_grapheme(
        terminal: *mut c_void,
        row: u16,
        col: u16,
        out_len: *mut u8,
    ) -> *const u32;
    pub(crate) fn ghostty_vt_terminal_render_row_selection(
        terminal: *mut c_void,
        row: u16,
        start_x: *mut u16,
        end_x: *mut u16,
    ) -> u8;

    pub(crate) fn ghostty_vt_terminal_set_selection(
        terminal: *mut c_void,
        start_x: u16,
        start_y: u32,
        end_x: u16,
        end_y: u32,
        rectangular: u8,
    ) -> c_int;
    pub(crate) fn ghostty_vt_terminal_clear_selection(terminal: *mut c_void);
    pub(crate) fn ghostty_vt_terminal_get_selection_text(
        terminal: *mut c_void,
        out_len: *mut usize,
    ) -> *const u8;
    pub(crate) fn ghostty_vt_bytes_free(bytes: *const u8, len: usize);

    pub(crate) fn ghostty_vt_terminal_encode_key(
        terminal: *mut c_void,
        key: c_int,
        mods: u16,
        action: u8,
        text_ptr: *const u8,
        text_len: usize,
        buf: *mut u8,
        buf_len: usize,
    ) -> usize;
    pub fn ghostty_vt_terminal_encode_mouse(
        terminal: *mut c_void,
        button: u8,
        action: u8,
        mods: u8,
        x: u16,
        y: u16,
        buf: *mut u8,
        buf_len: usize,
    ) -> usize;
}

#[cfg(test)]
mod tests {
    mod core;
    mod input;
    mod modes;
    mod render;
    mod safe_terminal;
    mod scroll;
    mod selection;
}
