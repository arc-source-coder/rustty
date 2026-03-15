use anyhow::Result;
use windows::Win32::Graphics::DirectWrite::IDWriteFontFace2;

use crate::backend::dwrite::analyzer::DWriteAnalyzer;
use crate::shaper::run_iter::{RunIterator, RunIteratorHook, RunOptions};
use crate::types::{ShapeOptions, ShapedCells, TextRun};

/// Ghostty: `Shaper.Codepoint` (both harfbuzz.zig and coretext.zig)
#[derive(Clone, Copy)]
pub struct Codepoint {
    pub codepoint: u32,
    pub cluster: u32,
}

/// DWrite shaping engine with Ghostty-aligned buffer ownership.
///
/// Mirrors Ghostty's CoreText `Shaper` struct: owns run_state
/// (codepoints + UTF-16 unichars), features (parsed at init),
/// and the shaping backend.
///
/// Ghostty reference: `font/shaper/coretext.zig::Shaper`
pub struct Shaper {
    pub(crate) analyzer: DWriteAnalyzer,
    /// Ghostty CoreText: `RunState.codepoints`
    pub(crate) codepoints: Vec<Codepoint>,
    /// Ghostty CoreText: `RunState.unichars`
    pub(crate) utf16_buf: Vec<u16>,
    /// Shape configuration owned by the shaper (Ghostty-style init-time options).
    pub(crate) shape_options: ShapeOptions,
}

impl Shaper {
    pub fn new(analyzer: DWriteAnalyzer, shape_options: ShapeOptions) -> Self {
        Self {
            analyzer,
            codepoints: Vec::new(),
            utf16_buf: Vec::new(),
            shape_options,
        }
    }

    /// Reconfigure shape options when renderer text config changes
    /// (font size, locale, feature spec, etc.).
    pub fn reconfigure(&mut self, shape_options: ShapeOptions) {
        self.shape_options = shape_options;
    }

    /// Returns a RunIterator whose hook points back to this Shaper
    /// via raw pointer (matching Ghostty's `shaper: *Shaper`).
    ///
    /// Ghostty: `Shaper.runIterator(opts) -> RunIterator`
    pub fn run_iterator<'a>(&mut self, opts: RunOptions<'a>) -> RunIterator<'a> {
        RunIterator::new(opts, RunIteratorHook::new(self))
    }

    /// Shape the current run using codepoints/UTF-16 collected during
    /// the most recent run iteration.
    ///
    /// Ghostty: `Shaper.shape(run) -> []const Cell`
    pub fn shape<'a>(
        &'a mut self,
        run: TextRun,
        face: &IDWriteFontFace2,
    ) -> Result<ShapedCells<'a>> {
        self.analyzer.shape(
            run,
            &self.codepoints,
            &self.utf16_buf,
            &self.shape_options,
            face,
        )
    }
}

#[cfg(all(test, target_os = "windows"))]
mod windows_tests {
    use super::*;

    use crate::backend::dwrite::analyzer::DWriteAnalyzer;
    use crate::backend::dwrite::fallback::FontFallbackContext;
    use crate::backend::dwrite::variation::StyleVariationRequest;
    use crate::collection::Collection;
    use crate::shaper::run_iter::{RowCells, RunOptions};
    use crate::shared_grid::{GridMetrics, SharedGrid};
    use crate::types::{FontFeatureSpec, Style};
    use ghostty_vt::{ColorRGB, Terminal};
    use windows::Win32::Graphics::DirectWrite::{
        DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_FAMILY_MODEL_TYPOGRAPHIC, DWriteCreateFactory,
        IDWriteFactory2, IDWriteFactory6,
    };
    use windows_core::{HSTRING, Interface};

    fn integration_grid(factory6: &IDWriteFactory6) -> SharedGrid {
        let grid = SharedGrid::with_collection(Collection::new(), GridMetrics::default());
        let requests: [StyleVariationRequest<'_>; Style::COUNT] =
            std::array::from_fn(|_| StyleVariationRequest {
                family: "Cascadia Code",
                axes: Default::default(),
            });
        grid.configure_dwrite_primary_faces(factory6, &requests)
            .expect("configure primary faces");

        let base_collection = unsafe {
            factory6.GetSystemFontCollection(false, DWRITE_FONT_FAMILY_MODEL_TYPOGRAPHIC)
        }
        .expect("system font collection");
        let fallback = unsafe { factory6.GetSystemFontFallback() }.expect("system font fallback");
        grid.set_dwrite_fallback(
            FontFallbackContext {
                base_family: HSTRING::from("Segoe UI"),
                base_collection: base_collection.cast().expect("font collection cast"),
                fallback,
            },
            "en-US",
        );
        grid
    }

    fn integration_shaper(factory2: &IDWriteFactory2) -> Shaper {
        Shaper::new(
            DWriteAnalyzer::new(factory2).expect("create dwrite analyzer"),
            ShapeOptions {
                locale: "en-US".into(),
                font_size: 16.0,
                cell_width: 8.0,
                variant: Style::Normal,
                features: FontFeatureSpec::default(),
            },
        )
    }

    #[test]
    fn adjacent_emoji_keep_terminal_cell_positions() {
        let factory2: IDWriteFactory2 =
            unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).expect("create dwrite") };
        let factory6 = factory2.cast::<IDWriteFactory6>().expect("factory6 cast");
        let grid = integration_grid(&factory6);

        let mut terminal = Terminal::new(
            80,
            4,
            ColorRGB {
                r: 255,
                g: 255,
                b: 255,
            },
            ColorRGB { r: 0, g: 0, b: 0 },
        )
        .expect("create terminal");
        terminal.feed("🍎🌶❤️🔥🌈🏴‍☠️".as_bytes());
        let frame = terminal.render_frame();
        let raw_cells = frame.row_raw(0).expect("row cells");
        let styles = frame.row_styles(0).unwrap_or(&[]);
        let graphemes = frame.row_graphemes(0).unwrap_or(&[]);

        let mut shaper = integration_shaper(&factory2);
        let mut run_iter = shaper.run_iterator(RunOptions {
            grid: &grid,
            cells: RowCells {
                raw_cells,
                styles,
                graphemes,
            },
            selection: None,
            cursor_x: None,
        });

        let first_run = run_iter.next().expect("first run");
        let face = grid
            .face_for_index(first_run.font_index)
            .expect("face for first run");
        let shaped = shaper.shape(first_run, &face).expect("shape first run");
        let xs = shaped.cells.iter().map(|cell| cell.x).collect::<Vec<_>>();

        assert_eq!(xs, vec![0, 2, 3, 4, 6, 8]);
        for &x in &xs {
            assert!(
                !raw_cells[x as usize].is_spacer(),
                "glyph landed on spacer cell at column {x}"
            );
        }
    }

    #[test]
    fn mixed_emoji_runs_never_land_on_spacer_cells() {
        let factory2: IDWriteFactory2 =
            unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).expect("create dwrite") };
        let factory6 = factory2.cast::<IDWriteFactory6>().expect("factory6 cast");
        let grid = integration_grid(&factory6);

        let mut terminal = Terminal::new(
            80,
            4,
            ColorRGB {
                r: 255,
                g: 255,
                b: 255,
            },
            ColorRGB { r: 0, g: 0, b: 0 },
        )
        .expect("create terminal");
        terminal.feed("🍎 🌶 ❤️ 🔥 🌈 🏴‍☠️ 🇺🇸".as_bytes());
        let frame = terminal.render_frame();
        let raw_cells = frame.row_raw(0).expect("row cells");
        let styles = frame.row_styles(0).unwrap_or(&[]);
        let graphemes = frame.row_graphemes(0).unwrap_or(&[]);

        let mut shaper = integration_shaper(&factory2);
        let mut run_iter = shaper.run_iterator(RunOptions {
            grid: &grid,
            cells: RowCells {
                raw_cells,
                styles,
                graphemes,
            },
            selection: None,
            cursor_x: None,
        });

        while let Some(run) = run_iter.next() {
            let face = grid.face_for_index(run.font_index).expect("face for run");
            let shaped = shaper.shape(run, &face).expect("shape run");
            for cell in shaped.cells {
                let col = run.offset + cell.x;
                assert!(
                    !raw_cells[col as usize].is_spacer(),
                    "glyph landed on spacer cell at column {col}"
                );
            }
        }
    }
}
