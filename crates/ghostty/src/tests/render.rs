use crate::*;

const DEFAULT_FG: (u8, u8, u8) = (0xDD, 0xDD, 0xDD);
const DEFAULT_BG: (u8, u8, u8) = (0x1E, 0x1E, 0x2E);

fn render_update(ptr: NonNull<c_void>) -> u8 {
    unsafe {
        ghostty_terminal_lock(ptr);
        let rc = ghostty_terminal_render_update(ptr);
        ghostty_terminal_unlock(ptr);
        rc
    }
}

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
    assert!(!ptr.is_null());
    let p = unsafe { NonNull::new_unchecked(ptr) };

    let rc = render_update(p);
    assert_eq!(rc, 0);
    // First update is always full dirty
    assert_eq!(unsafe { ghostty_terminal_render_dirty(p) }, 2);
    assert_eq!(unsafe { ghostty_terminal_render_rows(p) }, 24);
    assert_eq!(unsafe { ghostty_terminal_render_cols(p) }, 80);
    unsafe { ghostty_terminal_render_clear_dirty(p) };
    assert_eq!(unsafe { ghostty_terminal_render_dirty(p) }, 0);
    unsafe { ghostty_terminal_free(p) };
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

    assert!(!ptr.is_null());
    let p = unsafe { NonNull::new_unchecked(ptr) };

    render_update(p);
    let mut cursor = CursorState::default();
    let cursor_ptr = unsafe { NonNull::new_unchecked(&mut cursor) };
    unsafe { ghostty_terminal_render_cursor(p, cursor_ptr) };
    assert_eq!(cursor.x, 0);
    assert_eq!(cursor.y, 0);
    assert_eq!(cursor.in_viewport, 1);
    assert_eq!(cursor.visible, 1);
    assert_eq!(cursor.style, 1); // block
    unsafe { ghostty_terminal_free(p) };
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
    assert!(!ptr.is_null());
    let p = unsafe { NonNull::new_unchecked(ptr) };

    render_update(p);
    let colors = unsafe { ghostty_terminal_render_colors(p) };
    assert!(!colors.is_null());
    // Default colors should be set (exact values depend on Ghostty defaults)
    unsafe { ghostty_terminal_free(p) };
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
    assert!(!ptr.is_null());
    let p = unsafe { NonNull::new_unchecked(ptr) };

    render_update(p);

    let colors_ptr = unsafe { ghostty_terminal_render_colors(p) };
    assert!(!colors_ptr.is_null());

    let colors = unsafe { &*colors_ptr };
    // Palette index 1 is red in Ghostty's default palette (#cc6666)
    assert_eq!(colors.palette[1].r(), 204);
    assert_eq!(colors.palette[1].g(), 102);
    assert_eq!(colors.palette[1].b(), 102);

    unsafe { ghostty_terminal_free(p) };
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
    assert!(!ptr.is_null());
    let p = unsafe { NonNull::new_unchecked(ptr) };

    render_update(p);
    let mut start_x: u16 = 0;
    let mut end_x: u16 = 0;

    let start_ptr = unsafe { NonNull::new_unchecked(&mut start_x) };
    let end_ptr = unsafe { NonNull::new_unchecked(&mut end_x) };

    let has_selection = unsafe { ghostty_terminal_render_row_selection(p, 0, start_ptr, end_ptr) };
    assert!(!has_selection); // no selection
    unsafe { ghostty_terminal_free(p) };
}
