use crate::*;

const DEFAULT_FG: (u8, u8, u8) = (0xDD, 0xDD, 0xDD);
const DEFAULT_BG: (u8, u8, u8) = (0x1E, 0x1E, 0x2E);

#[test]
fn test_render_update_empty() {
    let ptr = unsafe {
        ghostty_terminal_new(
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
    let rc = unsafe { ghostty_terminal_render_update(ptr) };
    assert_eq!(rc, 0);
    // First update is always full dirty
    assert_eq!(unsafe { ghostty_terminal_render_dirty(ptr) }, 2);
    assert_eq!(unsafe { ghostty_terminal_render_rows(ptr) }, 24);
    assert_eq!(unsafe { ghostty_terminal_render_cols(ptr) }, 80);
    unsafe { ghostty_terminal_render_clear_dirty(ptr) };
    assert_eq!(unsafe { ghostty_terminal_render_dirty(ptr) }, 0);
    unsafe { ghostty_terminal_free(ptr) };
}

#[test]
fn test_render_partial_dirty() {
    let ptr = unsafe {
        ghostty_terminal_new(
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
    unsafe { ghostty_terminal_render_update(ptr) };
    unsafe { ghostty_terminal_render_clear_dirty(ptr) };
    // Feed some text — only touched rows should be dirty
    let text = b"Hello";
    unsafe { ghostty_terminal_feed(ptr, text.as_ptr(), text.len()) };
    unsafe { ghostty_terminal_render_update(ptr) };
    let dirty = unsafe { ghostty_terminal_render_dirty(ptr) };
    assert!(dirty > 0); // partial or full
    // Row 0 should be dirty (cursor starts at 0,0)
    assert_eq!(unsafe { ghostty_terminal_render_row_dirty(ptr, 0) }, 1);
    unsafe { ghostty_terminal_free(ptr) };
}

#[test]
fn test_render_cursor() {
    let ptr = unsafe {
        ghostty_terminal_new(
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
    unsafe { ghostty_terminal_render_update(ptr) };
    let mut cursor = CursorState::default();
    let rc = unsafe { ghostty_terminal_render_cursor(ptr, &mut cursor) };
    assert_eq!(rc, 0);
    assert_eq!(cursor.x, 0);
    assert_eq!(cursor.y, 0);
    assert_eq!(cursor.in_viewport, 1);
    assert_eq!(cursor.visible, 1);
    assert_eq!(cursor.style, 1); // block
    unsafe { ghostty_terminal_free(ptr) };
}

#[test]
fn test_render_colors() {
    let ptr = unsafe {
        ghostty_terminal_new(
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
    unsafe { ghostty_terminal_render_update(ptr) };
    let colors = unsafe { ghostty_terminal_render_colors(ptr) };
    assert!(!colors.is_null());
    // Default colors should be set (exact values depend on Ghostty defaults)
    unsafe { ghostty_terminal_free(ptr) };
}

#[test]
fn test_render_row_raw() {
    let ptr = unsafe {
        ghostty_terminal_new(
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
    unsafe { ghostty_terminal_feed(ptr, text.as_ptr(), text.len()) };
    unsafe { ghostty_terminal_render_update(ptr) };

    let mut len: u16 = 0;
    let raw_ptr = unsafe { ghostty_terminal_render_row_raw(ptr, 0, &mut len) };
    assert!(!raw_ptr.is_null());
    assert_eq!(len, 80);

    let cells = unsafe { std::slice::from_raw_parts(raw_ptr as *const RawCell, len as usize) };
    assert_eq!(cells[0].codepoint(), b'A' as u32);
    assert_eq!(cells[1].codepoint(), b'B' as u32);
    assert_eq!(cells[2].codepoint(), b'C' as u32);
    assert_eq!(cells[0].content_tag(), 0); // codepoint
    assert_eq!(cells[0].wide(), 0); // narrow
    assert!(!cells[0].has_grapheme());

    unsafe { ghostty_terminal_free(ptr) };
}

#[test]
fn test_render_row_styles() {
    let ptr = unsafe {
        ghostty_terminal_new(
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
    unsafe { ghostty_terminal_feed(ptr, seq.as_ptr(), seq.len()) };
    unsafe { ghostty_terminal_render_update(ptr) };

    // Raw cells
    let mut len: u16 = 0;
    let raw_ptr = unsafe { ghostty_terminal_render_row_raw(ptr, 0, &mut len) };
    let cells = unsafe { std::slice::from_raw_parts(raw_ptr as *const RawCell, len as usize) };
    assert_eq!(cells[0].codepoint(), b'X' as u32);
    assert!(cells[0].style_id() != 0); // has a non-default style

    // Styles
    let mut slen: u16 = 0;
    let styles_ptr = unsafe { ghostty_terminal_render_row_styles(ptr, 0, &mut slen) };
    let styles =
        unsafe { std::slice::from_raw_parts(styles_ptr as *const CellStyle, slen as usize) };
    assert!(styles[0].is_bold());
    // Foreground should be palette color (red = palette index 1)
    assert_eq!(styles[0].fg.tag, 1); // palette
    assert_eq!(styles[0].fg.r, 1); // palette index 1 = red

    unsafe { ghostty_terminal_free(ptr) };
}

#[test]
fn test_render_palette_batch() {
    let ptr = unsafe {
        ghostty_terminal_new(
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
    unsafe { ghostty_terminal_render_update(ptr) };

    let colors_ptr = unsafe { ghostty_terminal_render_colors(ptr) };
    assert!(!colors_ptr.is_null());

    let colors = unsafe { &*colors_ptr };
    // Palette index 1 is red in Ghostty's default palette (#cc6666)
    assert_eq!(colors.palette[1].r(), 204);
    assert_eq!(colors.palette[1].g(), 102);
    assert_eq!(colors.palette[1].b(), 102);

    unsafe { ghostty_terminal_free(ptr) };
}

#[test]
fn test_render_row_selection_none() {
    let ptr = unsafe {
        ghostty_terminal_new(
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
    unsafe { ghostty_terminal_render_update(ptr) };
    let mut sx: u16 = 0;
    let mut ex: u16 = 0;
    let has = unsafe { ghostty_terminal_render_row_selection(ptr, 0, &mut sx, &mut ex) };
    assert_eq!(has, 0); // no selection
    unsafe { ghostty_terminal_free(ptr) };
}
