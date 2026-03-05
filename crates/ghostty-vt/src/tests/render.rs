use crate::*;

const DEFAULT_FG: (u8, u8, u8) = (0xDD, 0xDD, 0xDD);
const DEFAULT_BG: (u8, u8, u8) = (0x1E, 0x1E, 0x2E);

#[test]
fn test_render_update_empty() {
    let ptr = unsafe {
        ghostty_vt_terminal_new(
            80,
            24,
            DEFAULT_FG.0,
            DEFAULT_FG.1,
            DEFAULT_FG.2,
            DEFAULT_BG.0,
            DEFAULT_BG.1,
            DEFAULT_BG.2,
        )
    };
    let rc = unsafe { ghostty_vt_terminal_render_update(ptr) };
    assert_eq!(rc, 0);
    // First update is always full dirty
    assert_eq!(unsafe { ghostty_vt_terminal_render_dirty(ptr) }, 2);
    assert_eq!(unsafe { ghostty_vt_terminal_render_rows(ptr) }, 24);
    assert_eq!(unsafe { ghostty_vt_terminal_render_cols(ptr) }, 80);
    unsafe { ghostty_vt_terminal_render_clear_dirty(ptr) };
    assert_eq!(unsafe { ghostty_vt_terminal_render_dirty(ptr) }, 0);
    unsafe { ghostty_vt_terminal_free(ptr) };
}

#[test]
fn test_render_partial_dirty() {
    let ptr = unsafe {
        ghostty_vt_terminal_new(
            80,
            24,
            DEFAULT_FG.0,
            DEFAULT_FG.1,
            DEFAULT_FG.2,
            DEFAULT_BG.0,
            DEFAULT_BG.1,
            DEFAULT_BG.2,
        )
    };
    // First update → full dirty
    unsafe { ghostty_vt_terminal_render_update(ptr) };
    unsafe { ghostty_vt_terminal_render_clear_dirty(ptr) };
    // Feed some text — only touched rows should be dirty
    let text = b"Hello";
    unsafe { ghostty_vt_terminal_feed(ptr, text.as_ptr(), text.len()) };
    unsafe { ghostty_vt_terminal_render_update(ptr) };
    let dirty = unsafe { ghostty_vt_terminal_render_dirty(ptr) };
    assert!(dirty > 0); // partial or full
    // Row 0 should be dirty (cursor starts at 0,0)
    assert_eq!(unsafe { ghostty_vt_terminal_render_row_dirty(ptr, 0) }, 1);
    unsafe { ghostty_vt_terminal_free(ptr) };
}

#[test]
fn test_render_cursor() {
    let ptr = unsafe {
        ghostty_vt_terminal_new(
            80,
            24,
            DEFAULT_FG.0,
            DEFAULT_FG.1,
            DEFAULT_FG.2,
            DEFAULT_BG.0,
            DEFAULT_BG.1,
            DEFAULT_BG.2,
        )
    };
    unsafe { ghostty_vt_terminal_render_update(ptr) };
    let mut cursor = CursorState::default();
    let rc = unsafe { ghostty_vt_terminal_render_cursor(ptr, &mut cursor) };
    assert_eq!(rc, 0);
    assert_eq!(cursor.x, 0);
    assert_eq!(cursor.y, 0);
    assert_eq!(cursor.in_viewport, 1);
    assert_eq!(cursor.visible, 1);
    assert_eq!(cursor.style, 1); // block
    unsafe { ghostty_vt_terminal_free(ptr) };
}

#[test]
fn test_render_colors() {
    let ptr = unsafe {
        ghostty_vt_terminal_new(
            80,
            24,
            DEFAULT_FG.0,
            DEFAULT_FG.1,
            DEFAULT_FG.2,
            DEFAULT_BG.0,
            DEFAULT_BG.1,
            DEFAULT_BG.2,
        )
    };
    unsafe { ghostty_vt_terminal_render_update(ptr) };
    let mut colors = ColorState::default();
    let rc = unsafe { ghostty_vt_terminal_render_colors(ptr, &mut colors) };
    assert_eq!(rc, 0);
    // Default colors should be set (exact values depend on Ghostty defaults)
    unsafe { ghostty_vt_terminal_free(ptr) };
}

#[test]
fn test_render_palette() {
    let ptr = unsafe {
        ghostty_vt_terminal_new(
            80,
            24,
            DEFAULT_FG.0,
            DEFAULT_FG.1,
            DEFAULT_FG.2,
            DEFAULT_BG.0,
            DEFAULT_BG.1,
            DEFAULT_BG.2,
        )
    };
    unsafe { ghostty_vt_terminal_render_update(ptr) };
    let mut color = ColorRGB::default();
    let rc = unsafe { ghostty_vt_terminal_render_palette_color(ptr, 1, &mut color) };
    assert_eq!(rc, 0);
    // Palette index 1 is red in Ghostty's default palette (#cc6666)
    assert_eq!(color.r, 204);
    assert_eq!(color.g, 102);
    assert_eq!(color.b, 102);
    unsafe { ghostty_vt_terminal_free(ptr) };
}

#[test]
fn test_render_row_cells() {
    let ptr = unsafe {
        ghostty_vt_terminal_new(
            80,
            24,
            DEFAULT_FG.0,
            DEFAULT_FG.1,
            DEFAULT_FG.2,
            DEFAULT_BG.0,
            DEFAULT_BG.1,
            DEFAULT_BG.2,
        )
    };
    let text = b"ABC";
    unsafe { ghostty_vt_terminal_feed(ptr, text.as_ptr(), text.len()) };
    unsafe { ghostty_vt_terminal_render_update(ptr) };

    let mut len: u16 = 0;
    let cells = unsafe { ghostty_vt_terminal_render_row_cells(ptr, 0, &mut len) };
    assert!(!cells.is_null());
    assert_eq!(len, 80);

    let cells_slice = unsafe { std::slice::from_raw_parts(cells, len as usize) };
    assert_eq!(cells_slice[0].codepoint, b'A' as u32);
    assert_eq!(cells_slice[1].codepoint, b'B' as u32);
    assert_eq!(cells_slice[2].codepoint, b'C' as u32);
    assert_eq!(cells_slice[0].wide, 0); // narrow
    assert_eq!(cells_slice[0].grapheme_len, 0);
    unsafe { ghostty_vt_terminal_free(ptr) };
}

#[test]
fn test_render_styled_cell() {
    let ptr = unsafe {
        ghostty_vt_terminal_new(
            80,
            24,
            DEFAULT_FG.0,
            DEFAULT_FG.1,
            DEFAULT_FG.2,
            DEFAULT_BG.0,
            DEFAULT_BG.1,
            DEFAULT_BG.2,
        )
    };
    // SGR 1 (bold) + SGR 31 (red fg) + "X"
    let seq = b"\x1b[1;31mX";
    unsafe { ghostty_vt_terminal_feed(ptr, seq.as_ptr(), seq.len()) };
    unsafe { ghostty_vt_terminal_render_update(ptr) };

    let mut len: u16 = 0;
    let cells = unsafe { ghostty_vt_terminal_render_row_cells(ptr, 0, &mut len) };
    let cells_slice = unsafe { std::slice::from_raw_parts(cells, len as usize) };
    assert_eq!(cells_slice[0].codepoint, b'X' as u32);
    // Bold flag should be set (bit 0)
    assert!(cells_slice[0].style_flags & 1 != 0);
    // Foreground should be palette color (red = palette index 1)
    assert_eq!(cells_slice[0].fg_color_type, 1); // palette
    assert_eq!(cells_slice[0].fg_palette, 1); // red
    unsafe { ghostty_vt_terminal_free(ptr) };
}

#[test]
fn test_render_row_selection_none() {
    let ptr = unsafe {
        ghostty_vt_terminal_new(
            80,
            24,
            DEFAULT_FG.0,
            DEFAULT_FG.1,
            DEFAULT_FG.2,
            DEFAULT_BG.0,
            DEFAULT_BG.1,
            DEFAULT_BG.2,
        )
    };
    unsafe { ghostty_vt_terminal_render_update(ptr) };
    let mut sx: u16 = 0;
    let mut ex: u16 = 0;
    let has = unsafe { ghostty_vt_terminal_render_row_selection(ptr, 0, &mut sx, &mut ex) };
    assert_eq!(has, 0); // no selection
    unsafe { ghostty_vt_terminal_free(ptr) };
}
