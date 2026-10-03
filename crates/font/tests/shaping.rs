#![cfg(target_os = "windows")]

use std::sync::Arc;

use font::backend::dwrite::{discovery::DirectWrite, face::Face};
use font::collection::{Collection, FontEntry};
use font::resolver::CodepointResolver;
use font::shaper::Shaper;
use font::shaper::run_iterator::{RunIterator, RunOptions};
use font::shared_grid::SharedGrid;
use font::types::{FontIndex, FontSize, FontStyle, ShapeOptions};
use ghostty::{CellSize, CellView, ColorRGB, GridSize, ScreenSize, Terminal, TerminalDimensions};
use utils::floats::NotNan;
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_FACE_TYPE_TRUETYPE, DWRITE_FONT_SIMULATIONS_NONE,
    DWriteCreateFactory, IDWriteFactory7,
};
use windows::core::{HSTRING, Interface as _};

fn jetbrains_mono_grid() -> SharedGrid {
    // Load a pinned font without installing it or relying on system font discovery.
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../zig/ghostty/src/font/res/JetBrainsMonoNoNF-Regular.ttf");
    let factory: IDWriteFactory7 =
        unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) }.unwrap();
    let file =
        unsafe { factory.CreateFontFileReference(&HSTRING::from(path.as_os_str()), None) }.unwrap();
    let face = unsafe {
        factory.CreateFontFace(
            DWRITE_FONT_FACE_TYPE_TRUETYPE,
            &[Some(file)],
            0,
            DWRITE_FONT_SIMULATIONS_NONE,
        )
    }
    .unwrap();
    let mut collection = Collection::new(FontSize {
        points: NotNan::new(12.0).unwrap(),
        x_dpi: 96,
        y_dpi: 96,
    });
    let entry = FontEntry {
        face: Face::new(face.cast().unwrap()),
        fallback: false,
    };
    collection.add(entry, FontStyle::Regular).unwrap();
    SharedGrid::new(CodepointResolver {
        collection,
        styles: [true, false, false, false],
        discovery: Arc::new(DirectWrite::new().unwrap()),
    })
    .unwrap()
}

#[test]
fn terminal_runs_shape_with_position_independent_hashes() {
    let grid = jetbrains_mono_grid();
    let terminal = Terminal::new(
        TerminalDimensions {
            grid: GridSize {
                columns: 12,
                rows: 4,
            },
            screen: ScreenSize::new(12, 4).unwrap(),
            cell: CellSize::new(1, 1).unwrap(),
        },
        ColorRGB::new(255, 255, 255),
        ColorRGB::new(0, 0, 0),
    )
    .unwrap();
    terminal.feed(
        concat!(
            "Ae\u{301}B\r\n",         // Baseline: e + combining acute occupies one cell.
            "xxAe\u{301}B\r\n",       // Same text shifted two cells; selection isolates it.
            "Ae\u{301}C\r\n",         // One changed letter must miss the baseline cache entry.
            "AB\x1b[9mCD\x1b[0mEFGH", // Strikethrough on CD adds style boundaries.
        )
        .as_bytes(),
    );
    let frame = unsafe {
        terminal.lock();
        let frame = terminal.render_frame();
        terminal.unlock();
        frame
    };
    let rows = frame.render_rows();
    let mut shaper = Shaper::new(ShapeOptions { features: &[] }).unwrap();

    // Glyph IDs are from this font's cmap, not from the shaper under test:
    // A=1, B=26, C=27, D=33, E=37, F=56, G=57, H=64, x=367, é=226.
    // Every expected glyph here occupies one cell (including composed é), so
    // grouping the glyph IDs specifies both the run boundaries and cell positions.
    let mut check_row = |options: RunOptions<'_>, expected: &[&[u32]]| {
        let mut runs = RunIterator::new(options);
        let mut verified_hashes = Vec::new();
        let mut offset = 0;
        for &glyphs in expected {
            let width = glyphs.len() as u16;
            let run = runs.next(&mut shaper).unwrap().expect("missing run");
            assert_eq!((run.offset, run.cells), (offset, width));
            assert!(
                run.font_index == FontIndex::DEFAULT,
                "unexpected font fallback"
            );
            let shaped = shaper.shape(&run).unwrap();
            assert_eq!(shaped.len(), glyphs.len(), "column {offset}");
            for (x, (cell, &glyph)) in shaped.iter().zip(glyphs).enumerate() {
                assert_eq!(
                    (cell.x, cell.glyph_index, cell.x_offset, cell.y_offset),
                    (x as u16, glyph, 0, 0),
                    "column {offset}, glyph {x}"
                );
            }
            verified_hashes.push(run.hash);
            offset += width;
        }
        assert!(runs.next(&mut shaper).unwrap().is_none(), "extra run");
        verified_hashes
    };

    let cells: [_; 4] = std::array::from_fn(|y| CellView::from(&rows.cell_multi_array_lists()[y]));
    let options = |row| RunOptions {
        cells: &cells[row],
        grid: &grid,
        selection: None,
        cursor_x: None,
    };
    let original = check_row(options(0), &[&[1, 226, 26]]);
    let shifted = check_row(
        RunOptions {
            selection: Some([2, 4]),
            ..options(1)
        },
        &[&[367, 367], &[1, 226, 26]],
    );
    let changed = check_row(options(2), &[&[1, 226, 27]]);

    // Selection B..E, strikethrough CD, and cursor G split ABCDEFGH as:
    // A | B | CD | E | F | G | H.
    check_row(
        RunOptions {
            selection: Some([1, 4]),
            cursor_x: Some(6),
            ..options(3)
        },
        &[&[1], &[26], &[27, 33], &[37], &[56], &[57], &[64]],
    );

    let original_hash = original[0];
    let shifted_hash = shifted[1];
    let changed_hash = changed[0];

    // The same text and shape must produce the same cache
    // key even when its screen position changes.
    assert_eq!(shifted_hash, original_hash);

    // Changed text must produce a different cache key.
    assert_ne!(changed_hash, original_hash);
}
