use crate::*;

#[test]
fn test_scroll_viewport() {
    let ptr = unsafe { ghostty_vt_terminal_new(80, 24) };
    // Generate scrollback: 50 newlines pushes content above viewport
    let newlines = "\n".repeat(50);
    unsafe { ghostty_vt_terminal_feed(ptr, newlines.as_ptr(), newlines.len()) };
    // These should not crash — scroll up, to top, to bottom
    unsafe { ghostty_vt_terminal_scroll_viewport(ptr, -5) };
    unsafe { ghostty_vt_terminal_scroll_viewport_top(ptr) };
    unsafe { ghostty_vt_terminal_scroll_viewport_bottom(ptr) };
    unsafe { ghostty_vt_terminal_free(ptr) };
}
