use crate::Terminal;
use crate::VtEvent;

#[test]
fn da1_primary_response() {
    let mut term = Terminal::new(80, 24).unwrap();
    // Send DA1 query: ESC [ c
    term.feed(b"\x1B[c");
    let events = term.drain_events();
    let responses: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            VtEvent::DeviceResponse(bytes) => Some(bytes.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0], b"\x1B[?62;22c");
}

#[test]
fn da2_secondary_response() {
    let mut term = Terminal::new(80, 24).unwrap();
    // Send DA2 query: ESC [ > c
    term.feed(b"\x1B[>c");
    let events = term.drain_events();
    let responses: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            VtEvent::DeviceResponse(bytes) => Some(bytes.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0], b"\x1B[>1;10;0c");
}

#[test]
fn dsr_cursor_position() {
    let mut term = Terminal::new(80, 24).unwrap();
    // Move cursor to row 5, col 10 (1-indexed: CSI 5;10 H)
    term.feed(b"\x1B[5;10H");
    // Clear events from cursor move
    term.drain_events();
    // Query cursor position: ESC [ 6 n
    term.feed(b"\x1B[6n");
    let events = term.drain_events();
    let responses: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            VtEvent::DeviceResponse(bytes) => Some(bytes.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(responses.len(), 1);
    // Response: ESC [ 5;10 R
    assert_eq!(responses[0], b"\x1B[5;10R");
}

#[test]
fn dsr_operating_status() {
    let mut term = Terminal::new(80, 24).unwrap();
    // Query operating status: ESC [ 5 n
    term.feed(b"\x1B[5n");
    let events = term.drain_events();
    let responses: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            VtEvent::DeviceResponse(bytes) => Some(bytes.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0], b"\x1B[0n");
}

#[test]
fn kitty_keyboard_query() {
    let mut term = Terminal::new(80, 24).unwrap();
    // Query kitty keyboard: ESC [ ? u
    term.feed(b"\x1B[?u");
    let events = term.drain_events();
    let responses: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            VtEvent::DeviceResponse(bytes) => Some(bytes.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(responses.len(), 1);
    // Default flags = 0
    assert_eq!(responses[0], b"\x1B[?0u");
}

#[test]
fn size_report_csi_18t() {
    let mut term = Terminal::new(80, 24).unwrap();
    // Query grid size: CSI 18 t
    term.feed(b"\x1B[18t");
    let events = term.drain_events();
    let responses: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            VtEvent::DeviceResponse(bytes) => Some(bytes.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0], b"\x1B[8;24;80t");
}

#[test]
fn size_report_csi_14t_with_cell_size() {
    let mut term = Terminal::new(80, 24).unwrap();
    term.set_cell_size(8, 16);
    // Query text area pixel size: CSI 14 t
    term.feed(b"\x1B[14t");
    let events = term.drain_events();
    let responses: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            VtEvent::DeviceResponse(bytes) => Some(bytes.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(responses.len(), 1);
    // 80*8=640 width, 24*16=384 height
    assert_eq!(responses[0], b"\x1B[4;384;640t");
}

#[test]
fn size_report_csi_14t_without_cell_size() {
    let mut term = Terminal::new(80, 24).unwrap();
    // No cell size set — should produce no response.
    term.feed(b"\x1B[14t");
    let events = term.drain_events();
    let responses: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            VtEvent::DeviceResponse(bytes) => Some(bytes.clone()),
            _ => None,
        })
        .collect();
    assert!(responses.is_empty());
}

#[test]
fn multiple_responses_in_single_feed() {
    let mut term = Terminal::new(80, 24).unwrap();
    // Send DA1 + DSR operating status in one feed
    term.feed(b"\x1B[c\x1B[5n");
    let events = term.drain_events();
    let responses: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            VtEvent::DeviceResponse(bytes) => Some(bytes.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[0], b"\x1B[?62;22c");
    assert_eq!(responses[1], b"\x1B[0n");
}
