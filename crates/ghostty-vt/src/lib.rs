mod terminal;
mod types;

use core::ffi::{c_int, c_void};
pub(crate) use types::{BellCallback, ResponseCallback, TitleCallback};

pub use terminal::key_from_w3c;
pub use terminal::{
    InputOpts, RenderFrame, SelectionText, Terminal, VtEvent, encode_key, encode_mouse,
};
pub use types::{
    CellStyle, ColorRGB, CursorState, DirtyState, GraphemeSlice, MouseFormat, MouseMode, RawCell,
    RenderColors, ScrollbarInfo, StyleColor,
};

unsafe extern "C" {
    pub(crate) fn ghostty_vt_terminal_new(
        cols: u16,
        rows: u16,
        fg_r: u8,
        fg_g: u8,
        fg_b: u8,
        bg_r: u8,
        bg_g: u8,
        bg_b: u8,
    ) -> *mut c_void;
    pub(crate) fn ghostty_vt_terminal_free(terminal: *mut c_void);

    pub(crate) fn ghostty_vt_terminal_set_callbacks(
        terminal: *mut c_void,
        userdata: *mut c_void,
        bell: Option<BellCallback>,
        title: Option<TitleCallback>,
        response: Option<ResponseCallback>,
    );

    pub(crate) fn ghostty_vt_terminal_feed(
        terminal: *mut c_void,
        bytes: *const u8,
        len: usize,
    ) -> c_int;

    pub(crate) fn ghostty_vt_terminal_resize(terminal: *mut c_void, cols: u16, rows: u16) -> c_int;

    pub(crate) fn ghostty_vt_terminal_set_cell_size(
        terminal: *mut c_void,
        width_px: u16,
        height_px: u16,
    );

    pub(crate) fn ghostty_vt_terminal_get_mouse_mode(terminal: *mut c_void) -> u8;
    pub(crate) fn ghostty_vt_terminal_get_mouse_format(terminal: *mut c_void) -> u8;
    pub(crate) fn ghostty_vt_terminal_is_bracketed_paste(terminal: *mut c_void) -> u8;
    pub(crate) fn ghostty_vt_terminal_get_kitty_keyboard_flags(terminal: *mut c_void) -> u8;
    pub(crate) fn ghostty_vt_terminal_is_synchronized_output(terminal: *mut c_void) -> u8;
    pub(crate) fn ghostty_vt_terminal_reset_synchronized_output(terminal: *mut c_void);
    pub(crate) fn ghostty_vt_terminal_is_focus_event_mode(terminal: *mut c_void) -> u8;
    pub(crate) fn ghostty_vt_terminal_is_alternate_screen(terminal: *mut c_void) -> u8;
    pub(crate) fn ghostty_vt_terminal_is_mouse_alternate_scroll(terminal: *mut c_void) -> u8;

    pub(crate) fn ghostty_vt_terminal_scroll_viewport(terminal: *mut c_void, delta: i32);
    pub(crate) fn ghostty_vt_terminal_scroll_viewport_top(terminal: *mut c_void);
    pub(crate) fn ghostty_vt_terminal_scroll_viewport_bottom(terminal: *mut c_void);
    pub(crate) fn ghostty_vt_terminal_scrollbar_info(
        terminal: *mut c_void,
        out: *mut ScrollbarInfo,
    ) -> c_int;
    pub(crate) fn ghostty_vt_terminal_viewport_is_bottom(terminal: *mut c_void) -> u8;
    pub(crate) fn ghostty_vt_terminal_scroll_to_row(terminal: *mut c_void, row: u64);

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
    pub(crate) fn ghostty_vt_terminal_render_colors(terminal: *mut c_void) -> *const RenderColors;
    pub(crate) fn ghostty_vt_terminal_render_row_raw(
        terminal: *mut c_void,
        row: u16,
        out_len: *mut u16,
    ) -> *const u64;

    pub(crate) fn ghostty_vt_terminal_render_row_styles(
        terminal: *mut c_void,
        row: u16,
        out_len: *mut u16,
    ) -> *const CellStyle;

    pub(crate) fn ghostty_vt_terminal_render_row_graphemes(
        terminal: *mut c_void,
        row: u16,
        out_len: *mut u16,
    ) -> *const GraphemeSlice;
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
    pub(crate) fn ghostty_vt_terminal_select_word_at(
        terminal: *mut c_void,
        x: u16,
        y: u32,
    ) -> c_int;
    pub(crate) fn ghostty_vt_terminal_select_line_at(
        terminal: *mut c_void,
        x: u16,
        y: u32,
    ) -> c_int;
    pub(crate) fn ghostty_vt_terminal_select_output_at(
        terminal: *mut c_void,
        x: u16,
        y: u32,
    ) -> c_int;
    pub(crate) fn ghostty_vt_terminal_select_word_drag(
        terminal: *mut c_void,
        click_x: u16,
        click_y: u32,
        drag_x: u16,
        drag_y: u32,
    ) -> c_int;
    pub(crate) fn ghostty_vt_terminal_select_line_drag(
        terminal: *mut c_void,
        click_x: u16,
        click_y: u32,
        drag_x: u16,
        drag_y: u32,
    ) -> c_int;
    pub(crate) fn ghostty_vt_terminal_clear_selection(terminal: *mut c_void);
    pub(crate) fn ghostty_vt_terminal_get_selection_text(
        terminal: *mut c_void,
        out_len: *mut usize,
    ) -> *const u8;
    pub(crate) fn ghostty_vt_bytes_free(bytes: *const u8, len: usize);

    /// Snapshot all input-relevant mode flags into a C struct.
    /// Must be called under the terminal mutex; returns a plain-data copy.
    pub(crate) fn ghostty_vt_terminal_get_input_opts(terminal: *mut c_void) -> InputOptsC;

    /// Encode a key event using a pre-captured opts snapshot. No terminal handle needed.
    pub(crate) fn ghostty_vt_encode_key(
        opts: InputOptsC,
        key: c_int,
        mods: u16,
        action: u8,
        text_ptr: *const u8,
        text_len: usize,
        unshifted_codepoint: u32,
        buf: *mut u8,
        buf_len: usize,
    ) -> usize;

    /// Encode a mouse event using a pre-captured opts snapshot. No terminal handle needed.
    pub(crate) fn ghostty_vt_encode_mouse(
        opts: InputOptsC,
        button: u8,
        action: u8,
        mods: u8,
        x: u16,
        y: u16,
        buf: *mut u8,
        buf_len: usize,
    ) -> usize;

    pub fn ghostty_vt_key_from_w3c(code_ptr: *const u8, code_len: usize) -> c_int;
}

/// C ABI mirror of `InputOptsC` in input.zig.
/// 10 u8 fields, no padding — layout is stable across Rust/Zig ABI boundary.
/// Private: callers use the higher-level `InputOpts` type.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct InputOptsC {
    pub cursor_key_application: u8,
    pub keypad_key_application: u8,
    pub ignore_keypad_with_numlock: u8,
    pub alt_esc_prefix: u8,
    pub modify_other_keys_state_2: u8,
    pub kitty_flags: u8,
    pub mouse_event: u8,
    pub mouse_format: u8,
    pub bracketed_paste: u8,
    pub focus_event_mode: u8,
}

const _: () = assert!(std::mem::size_of::<InputOptsC>() == 10);
const _: () = assert!(std::mem::align_of::<InputOptsC>() == 1);

#[cfg(test)]
mod tests {
    mod core;
    mod device_response;
    mod input;
    mod modes;
    mod render;
    mod safe_terminal;
    mod scroll;
    mod selection;
}
