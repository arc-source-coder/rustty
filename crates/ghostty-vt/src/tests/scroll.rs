use crate::*;

const DEFAULT_FG: ColorRGB = ColorRGB::new(0xDD, 0xDD, 0xDD);
const DEFAULT_BG: ColorRGB = ColorRGB::new(0x1E, 0x1E, 0x2E);

#[test]
fn test_scroll_viewport() {
    let ptr = unsafe {
        ghostty_vt_terminal_new(
            80,
            24,
            DEFAULT_FG.r(),
            DEFAULT_FG.g(),
            DEFAULT_FG.b(),
            DEFAULT_BG.r(),
            DEFAULT_BG.g(),
            DEFAULT_BG.b(),
        )
    };
    // Generate scrollback: 50 newlines pushes content above viewport
    let newlines = "\n".repeat(50);
    unsafe { ghostty_vt_terminal_feed(ptr, newlines.as_ptr(), newlines.len()) };
    // These should not crash — scroll up, to top, to bottom
    unsafe { ghostty_vt_terminal_scroll_viewport(ptr, -5) };
    unsafe { ghostty_vt_terminal_scroll_viewport_top(ptr) };
    unsafe { ghostty_vt_terminal_scroll_viewport_bottom(ptr) };
    unsafe { ghostty_vt_terminal_free(ptr) };
}

#[test]
fn test_scrollbar_info_no_scrollback() {
    let term = Terminal::new(80, 24, DEFAULT_FG, DEFAULT_BG).unwrap();
    let info = term.scrollbar_info();
    assert_eq!(info.viewport_rows, 24);
    // With no scrollback content, total_rows == viewport_rows
    assert!(info.total_rows >= info.viewport_rows);
    assert!(term.viewport_is_bottom());
}

#[test]
fn test_scrollbar_info_with_scrollback() {
    let mut term = Terminal::new(80, 24, DEFAULT_FG, DEFAULT_BG).unwrap();
    // Push 100 newlines to create scrollback
    let newlines = "\n".repeat(100);
    term.feed(newlines.as_bytes());
    let info = term.scrollbar_info();
    assert!(info.total_rows > 24, "should have scrollback");
    assert!(
        term.viewport_is_bottom(),
        "should be at bottom after output"
    );

    // Scroll up
    term.scroll_viewport(-10);
    assert!(!term.viewport_is_bottom());
    let info2 = term.scrollbar_info();
    assert!(
        info2.top_row < info.top_row,
        "top_row should decrease after scrolling up"
    );

    // Scroll back to bottom
    term.scroll_to_bottom();
    assert!(term.viewport_is_bottom());
}

#[test]
fn test_scroll_to_row() {
    let mut term = Terminal::new(80, 24, DEFAULT_FG, DEFAULT_BG).unwrap();
    let newlines = "\n".repeat(100);
    term.feed(newlines.as_bytes());

    term.scroll_to_row(0); // scroll to top
    assert!(!term.viewport_is_bottom());
    let info = term.scrollbar_info();
    assert_eq!(info.top_row, 0);
}

#[test]
fn test_alternate_screen() {
    let mut term = Terminal::new(80, 24, DEFAULT_FG, DEFAULT_BG).unwrap();
    assert!(!term.is_alternate_screen());
    // Switch to alternate screen: CSI ?1049h
    term.feed(b"\x1b[?1049h");
    assert!(term.is_alternate_screen());
    // Switch back: CSI ?1049l
    term.feed(b"\x1b[?1049l");
    assert!(!term.is_alternate_screen());
}
