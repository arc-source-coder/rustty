use crate::*;

const DEFAULT_FG: ColorRGB = ColorRGB::new(0xDD, 0xDD, 0xDD);
const DEFAULT_BG: ColorRGB = ColorRGB::new(0x1E, 0x1E, 0x2E);

fn scrollbar_info(term: &Terminal) -> ScrollbarInfo {
    unsafe {
        term.lock();
        let info = term.scrollbar_info();
        term.unlock();
        info
    }
}

#[test]
fn test_scrollbar_info_no_scrollback() {
    let term = Terminal::new(80, 24, DEFAULT_FG, DEFAULT_BG).unwrap();
    let info = scrollbar_info(&term);
    assert_eq!(info.viewport_rows, 24);
    // With no scrollback content, total_rows == viewport_rows
    assert!(info.total_rows >= info.viewport_rows);
    assert!(term.viewport_is_bottom());
}
