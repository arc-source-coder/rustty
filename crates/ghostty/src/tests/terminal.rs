use std::assert_matches;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use crate::*;
use TerminalEvent::{Bell, TitleChanged};

const DEFAULT_FG: ColorRGB = ColorRGB::new(0xDD, 0xDD, 0xDD);
const DEFAULT_BG: ColorRGB = ColorRGB::new(0x1E, 0x1E, 0x2E);

fn terminal(columns: u16, rows: u16) -> Terminal {
    let dimensions = TerminalDimensions {
        grid: GridSize { columns, rows },
        screen: ScreenSize::new(u32::from(columns), u32::from(rows)).unwrap(),
        cell: CellSize::new(1, 1).unwrap(),
    };
    Terminal::new(dimensions, DEFAULT_FG, DEFAULT_BG).expect("failed to create terminal")
}

fn render_frame<'a>(terminal: &Terminal, state: &'a mut RenderState) -> RenderFrame<'a> {
    terminal.with_lock(|t| t.begin_update(state)).expect("failed to update render state").finish()
}

fn scrollbar_info(terminal: &Terminal) -> ScrollbarInfo {
    terminal.with_lock(|terminal| terminal.scrollbar_info())
}

#[test]
fn borrowed_render_rows_preserve_data_and_track_damage() {
    let terminal = terminal(8, 3);
    let mut render_state = RenderState::new().expect("failed to allocate render state");
    // Clear the initial full damage so feeding text only dirties its row.
    render_frame(&terminal, &mut render_state).mark_clean();

    terminal.feed(
        concat!(
            "A",
            "\x1b[1;38;2;12;34;56mB", // Bold, RGB foreground.
            "\x1b[0m界e\u{301}",      // Reset, wide character, combining accent.
        )
        .as_bytes(),
    );

    let frame = render_frame(&terminal, &mut render_state);
    assert_eq!(frame.dimensions(), (3, 8));
    assert_eq!(frame.dirty(), Dirty::Partial);

    let rows = frame.render_rows();
    assert_eq!(rows.dirty_rows(), &[true, false, false]);
    let cells = CellView::from(&rows.cell_multi_array_lists()[0]);
    let raw = cells.raw_cells();

    assert_eq!(raw[0].codepoint(), 'A' as u32);
    assert_eq!(raw[0].width(), Width::Narrow);
    assert!(!raw[0].has_styling());

    assert_eq!(raw[1].codepoint(), 'B' as u32);
    assert!(raw[1].has_styling());
    let style = unsafe { cells.styles()[1].assume_init_ref() };
    assert!(style.is_bold());
    assert_matches!(
        style.fg_color.color(),
        Color::Rgb(color) if color == ColorRGB::new(12, 34, 56)
    );

    assert_eq!(raw[2].codepoint(), '界' as u32);
    assert_eq!(raw[2].width(), Width::Wide);
    assert_eq!(raw[3].width(), Width::SpacerTail);

    assert_eq!(raw[4].codepoint(), 'e' as u32);
    assert_eq!(raw[4].content_tag(), ContentTag::CodepointGrapheme);
    let grapheme = unsafe { cells.graphemes()[4].assume_init_ref().as_slice() }.unwrap();
    assert_eq!(grapheme.len(), 1);
    assert_eq!(grapheme[0].get(), '\u{301}' as u32);

    let cursor = frame.render_cursor();
    assert_eq!(cursor.active, CursorCoordinate { x: 5, y: 0 });
    assert_eq!(cursor.viewport.into_option().unwrap().x, 5);
    frame.mark_clean();

    let frame = render_frame(&terminal, &mut render_state);
    assert_eq!(frame.dirty(), Dirty::Clean);
    assert_eq!(frame.render_rows().dirty_rows(), &[false, false, false]);
    frame.mark_clean();

    // Cursor movement also dirties the row it leaves. Consume that separately
    // so the next frame isolates a text/style update to the second row.
    terminal.feed(b"\x1b[2;1H");
    render_frame(&terminal, &mut render_state).mark_clean();
    terminal.feed(b"\x1b[48;2;91;82;73mZ");
    let frame = render_frame(&terminal, &mut render_state);
    assert_eq!(frame.dirty(), Dirty::Partial);
    let rows = frame.render_rows();
    assert_eq!(rows.dirty_rows(), &[false, true, false]);
    let first = CellView::from(&rows.cell_multi_array_lists()[0]);
    let second = CellView::from(&rows.cell_multi_array_lists()[1]);
    assert_eq!(second.raw_cells()[0].codepoint(), 'Z' as u32);
    assert!(second.raw_cells()[0].has_styling());
    let style = unsafe { second.styles()[0].assume_init_ref() };
    assert_matches!(
        style.bg_color.color(),
        Color::Rgb(color) if color == ColorRGB::new(91, 82, 73)
    );
    assert_eq!(first.raw_cells()[0].codepoint(), 'A' as u32);
    assert_eq!(first.raw_cells()[4].codepoint(), 'e' as u32);
    let grapheme = unsafe { first.graphemes()[4].assume_init_ref().as_slice() }.unwrap();
    assert_eq!(grapheme[0].get(), '\u{301}' as u32);
    frame.mark_clean();

    let frame = render_frame(&terminal, &mut render_state);
    assert_eq!(frame.dirty(), Dirty::Clean);
    assert_eq!(frame.render_rows().dirty_rows(), &[false, false, false]);
}

#[test]
fn feed_delivers_events_in_order_and_wakes_once() {
    let terminal = terminal(8, 3);
    let (event_tx, event_rx) = async_channel::unbounded();
    let wake_count = Arc::new(AtomicUsize::new(0));
    let callback_wake_count = Arc::clone(&wake_count);
    let callbacks = terminal.set_event_sender(event_tx, move || {
        callback_wake_count.fetch_add(1, Ordering::Relaxed);
    });

    terminal.feed(b"");
    assert_eq!(wake_count.load(Ordering::Relaxed), 0);

    terminal.feed(
        concat!(
            "\x07",               // Bell.
            "\x1b]2;alpha\x1b\\", // Title terminated by ST.
            "text\x07",           // Text followed by another bell.
            "\x1b]0;β\x07",       // Unicode title terminated by BEL (not a bell event).
        )
        .as_bytes(),
    );
    let events: Vec<_> = std::iter::from_fn(|| event_rx.try_recv().ok()).collect();
    assert_eq!(events, [Bell, TitleChanged("alpha".into()), Bell, TitleChanged("β".into())]);
    assert_eq!(wake_count.load(Ordering::Relaxed), 1);

    drop(callbacks);
    terminal.feed(b"\x07");
    assert!(event_rx.try_recv().is_err());
    assert_eq!(wake_count.load(Ordering::Relaxed), 1);
}

#[test]
fn selection_autoscroll_moves_viewport_until_release_and_is_consumed() {
    let terminal = terminal(8, 4);
    terminal.resize(TerminalDimensions {
        grid: GridSize { columns: 8, rows: 4 },
        screen: ScreenSize::new(80, 80).unwrap(),
        cell: CellSize::new(10, 20).unwrap(),
    });

    terminal.feed(b"00\r\n01\r\n02\r\n03\r\n04\r\n05\r\n06\r\n07\r\n08\r\n09\r\n10");
    terminal.scroll_viewport(-2);

    let before = scrollbar_info(&terminal);
    assert!(before.total_rows > before.viewport_rows);
    terminal.send_gesture_press(5.0, 50.0, false, false, false);
    let drag = terminal.send_gesture_drag(15.0, 1.0, false);
    assert!(drag.needs_redraw);
    assert!(drag.autoscroll);

    let tick = terminal.send_gesture_autoscroll_tick(15.0, 1.0, false);
    let after_tick = scrollbar_info(&terminal);
    assert!(tick.needs_redraw);
    assert!(tick.autoscroll);
    assert_eq!(after_tick.top_row + 1, before.top_row);

    terminal.send_gesture_release(15.0, 1.0);
    let stopped = terminal.send_gesture_autoscroll_tick(15.0, 1.0, false);
    assert!(!stopped.needs_redraw);
    assert!(!stopped.autoscroll);
    assert_eq!(scrollbar_info(&terminal), after_tick);

    // The tick exposes line 04 and selects from its second cell through line 06.
    assert_eq!(terminal.take_selection_text().as_deref(), Some("4\n05\n06"));
    assert_eq!(terminal.take_selection_text(), None);
}
