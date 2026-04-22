use crate::*;

const DEFAULT_FG: ColorRGB = ColorRGB::new(0xDD, 0xDD, 0xDD);
const DEFAULT_BG: ColorRGB = ColorRGB::new(0x1E, 0x1E, 0x2E);

fn render_frame(term: &Terminal) -> RenderFrame {
    unsafe {
        term.lock();
        let frame = term.render_frame();
        term.unlock();
        frame
    }
}

#[test]
fn new_and_drop() {
    let term = Terminal::new(80, 24, DEFAULT_FG, DEFAULT_BG).expect("failed to create terminal");
    drop(term);
}

#[test]
fn resize() {
    let term = Terminal::new(80, 24, DEFAULT_FG, DEFAULT_BG).unwrap();
    term.resize(120, 40);
}

#[test]
fn render_frame_dirty_lifecycle() {
    let term = Terminal::new(80, 24, DEFAULT_FG, DEFAULT_BG).unwrap();
    {
        let frame = render_frame(&term);
        // First update is always full dirty
        assert_eq!(frame.dirty(), DirtyState::Full);
        assert_eq!(frame.rows(), 24);
        assert_eq!(frame.cols(), 80);
    } // frame dropped — dirty cleared
    {
        let frame = render_frame(&term);
        assert_eq!(frame.dirty(), DirtyState::Clean);
    }
}

#[test]
fn render_frame_cursor() {
    let term = Terminal::new(80, 24, DEFAULT_FG, DEFAULT_BG).unwrap();
    let frame = render_frame(&term);
    let cursor = frame.cursor();
    assert_eq!(cursor.x, 0);
    assert_eq!(cursor.y, 0);
    assert_eq!(cursor.in_viewport, 1);
    assert_eq!(cursor.visible, 1);
    assert_eq!(cursor.style, 1); // block
}

#[test]
fn render_frame_colors_and_palette() {
    let term = Terminal::new(80, 24, DEFAULT_FG, DEFAULT_BG).unwrap();
    let frame = render_frame(&term);
    let colors = frame.colors();
    // Palette index 1 is red (#cc6666 in Ghostty defaults)
    let red = colors.palette[1];
    assert_eq!(red.r(), 204);
    assert_eq!(red.g(), 102);
    assert_eq!(red.b(), 102);
}

#[test]
fn row_raw_out_of_bounds_returns_none() {
    let term = Terminal::new(80, 24, DEFAULT_FG, DEFAULT_BG).unwrap();
    let frame = render_frame(&term);
    assert!(frame.row_raw(100).is_none());
    assert!(frame.row_graphemes(100).is_none());
}

#[test]
fn row_selection_none_by_default() {
    let term = Terminal::new(80, 24, DEFAULT_FG, DEFAULT_BG).unwrap();
    let frame = render_frame(&term);
    assert!(frame.row_selection(0).is_none());
}

#[test]
fn no_selection_returns_none() {
    let term = Terminal::new(80, 24, DEFAULT_FG, DEFAULT_BG).unwrap();
    assert!(term.selection_text().is_none());
}
