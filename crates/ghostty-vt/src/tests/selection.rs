use crate::*;

const DEFAULT_FG: (u8, u8, u8) = (0xDD, 0xDD, 0xDD);
const DEFAULT_BG: (u8, u8, u8) = (0x1E, 0x1E, 0x2E);

#[test]
fn test_selection_set_clear() {
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
    let text = b"Hello, World!";
    unsafe { ghostty_vt_terminal_feed(ptr, text.as_ptr(), text.len()) };

    // Set selection covering "Hello"
    let rc = unsafe { ghostty_vt_terminal_set_selection(ptr, 0, 0, 4, 0, 0) };
    assert_eq!(rc, 0);

    // Update render state to pick up selection
    unsafe { ghostty_vt_terminal_render_update(ptr) };

    // Row 0 should have a selection
    let mut sx: u16 = 0;
    let mut ex: u16 = 0;
    let has = unsafe { ghostty_vt_terminal_render_row_selection(ptr, 0, &mut sx, &mut ex) };
    assert_eq!(has, 1);
    assert_eq!(sx, 0);
    assert_eq!(ex, 4);

    // Get selection text
    let mut len: usize = 0;
    let text_ptr = unsafe { ghostty_vt_terminal_get_selection_text(ptr, &mut len) };
    assert!(!text_ptr.is_null());
    let selected =
        unsafe { std::str::from_utf8(std::slice::from_raw_parts(text_ptr, len)).unwrap() };
    assert_eq!(selected, "Hello");
    unsafe { ghostty_vt_bytes_free(text_ptr, len) };

    // Clear selection
    unsafe { ghostty_vt_terminal_clear_selection(ptr) };
    unsafe { ghostty_vt_terminal_render_update(ptr) };
    let has = unsafe { ghostty_vt_terminal_render_row_selection(ptr, 0, &mut sx, &mut ex) };
    assert_eq!(has, 0);

    unsafe { ghostty_vt_terminal_free(ptr) };
}
