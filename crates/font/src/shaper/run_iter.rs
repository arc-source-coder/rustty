use crate::shaper::hash::RunHasher;
use crate::shaper::shaper::Codepoint;
use crate::shared_grid::SharedGrid;
use crate::types::Presentation;
use crate::types::{FontIndex, Style, TextRun};
use ghostty::{CellStyle, GraphemeSlice, RawCell, StyleColor};

const KITTY_UNICODE_PLACEHOLDER: u32 = 0x10EEEE;

/// Row cell bundle for run iteration.
///
/// Ghostty reference:
/// `font/shape.zig` (`RunOptions.cells`).
pub struct RowCells<'a> {
    pub raw_cells: &'a [RawCell],
    pub styles: &'a [CellStyle],
    pub graphemes: &'a [GraphemeSlice],
}

pub struct RunOptions<'a> {
    pub grid: &'a SharedGrid,
    pub cells: RowCells<'a>,
    pub selection: Option<[u16; 2]>,
    pub cursor_x: Option<usize>,
}

/// Concrete hook that writes into the Shaper's owned buffers via raw pointer.
///
/// Ghostty: `Shaper.RunIteratorHook` (a concrete struct, NOT an interface).
/// Harfbuzz: `{ shaper: *Shaper }`, CoreText: `{ shaper: *Shaper }`.
///
/// SAFETY: The raw pointers are valid for the lifetime of the RunIterator.
/// Single-threaded renderer — no concurrent access. The hook only writes
/// during next(); between next() calls the buffers are stable for reading.
pub struct RunIteratorHook {
    codepoints: *mut Vec<Codepoint>,
    utf16_buf: *mut Vec<u16>,
}

impl RunIteratorHook {
    pub(crate) fn new(shaper: &mut crate::shaper::shaper::Shaper) -> Self {
        Self {
            codepoints: &mut shaper.codepoints as *mut _,
            utf16_buf: &mut shaper.utf16_buf as *mut _,
        }
    }

    #[cfg(test)]
    fn for_test(codepoints: &mut Vec<Codepoint>, utf16_buf: &mut Vec<u16>) -> Self {
        Self {
            codepoints: codepoints as *mut _,
            utf16_buf: utf16_buf as *mut _,
        }
    }

    /// Ghostty: `RunIteratorHook.prepare` — clear buffers, retain capacity.
    /// CoreText: `self.shaper.run_state.reset()` which calls
    /// `codepoints.clearRetainingCapacity()` + `unichars.clearRetainingCapacity()`.
    pub(crate) fn prepare(&mut self) {
        // SAFETY: single-threaded, pointer valid for iterator lifetime
        unsafe {
            (*self.codepoints).clear();
            (*self.utf16_buf).clear();
        }
    }

    /// Ghostty: `RunIteratorHook.addCodepoint`
    /// CoreText version: encodes to UTF-16 surrogates, appends dummy codepoint
    /// for surrogate pairs to keep indices aligned 1:1 with UTF-16 positions.
    pub(crate) fn add_codepoint(&mut self, cp: u32, cluster: u32) {
        // SAFETY: single-threaded, pointer valid for iterator lifetime
        unsafe {
            (*self.codepoints).push(Codepoint {
                codepoint: cp,
                cluster,
            });
            let c = char::from_u32(cp).unwrap_or('\u{FFFD}');
            let mut buf = [0u16; 2];
            let encoded = c.encode_utf16(&mut buf);
            (*self.utf16_buf).extend_from_slice(encoded);
            if encoded.len() == 2 {
                // Keep codepoints[] aligned 1:1 with UTF-16 positions so DWrite can
                // map shaped glyph indices back to terminal clusters without a separate
                // reverse-lookup buffer. Ghostty uses this approach with the CoreText backend.
                (*self.codepoints).push(Codepoint {
                    codepoint: 0,
                    cluster,
                });
            }
        }
    }

    /// Ghostty: `RunIteratorHook.finalize`
    pub(crate) fn finalize(&mut self) {
        // No-op for DWrite (HarfBuzz: guessSegmentProperties, CoreText: no-op)
    }
}

pub struct RunIterator<'a> {
    hooks: RunIteratorHook,
    opts: RunOptions<'a>,
    i: usize,
    max: usize,
}

impl<'a> RunIterator<'a> {
    pub fn new(opts: RunOptions<'a>, hooks: RunIteratorHook) -> Self {
        Self {
            max: trim_right_empty(opts.cells.raw_cells),
            hooks,
            opts,
            i: 0,
        }
    }

    #[allow(clippy::should_implement_trait)] // not a standard Iterator
    pub fn next(&mut self) -> Option<TextRun> {
        let raw_cells = self.opts.cells.raw_cells;
        let styles = self.opts.cells.styles;
        let graphemes = self.opts.cells.graphemes;

        while self.i < self.max {
            let raw = raw_cells[self.i];
            let style = style_at(styles, self.i, raw);
            if !(raw.style_id() != 0 && style.is_some_and(CellStyle::is_invisible)) {
                break;
            }
            self.i += 1;
        }

        if self.i >= self.max {
            return None;
        }

        let start = self.i;
        let start_style = style_at(styles, start, raw_cells[start]);
        let run_variant = font_variant_from_style(start_style);

        self.hooks.prepare();
        let mut hasher = RunHasher::new();
        let mut current_font = None;

        let mut j = start;
        while j < self.max {
            let cluster = (j - start) as u32;
            let cell = raw_cells[j];

            if let Some([sel_start, sel_end]) = self.opts.selection
                && j > start
            {
                let j_u16 = j as u16;
                if sel_start > 0 && j_u16 == sel_start {
                    break;
                }
                if sel_end > 0 && j_u16 > sel_end {
                    break;
                }
            }

            if cell.is_spacer() {
                j += 1;
                continue;
            }

            if j > start {
                let prev = raw_cells[j - 1];
                if prev.content_tag() == 0
                    && cell.content_tag() == 0
                    && split_bad_ligature(prev.codepoint(), cell.codepoint())
                {
                    break;
                }

                if prev.style_id() != cell.style_id() {
                    let c1 = comparable_style(start_style);
                    let c2 = comparable_style(style_at(styles, j, cell));
                    if c1 != c2 {
                        break;
                    }
                }
            }

            if !cell.has_grapheme()
                && let Some(cursor_x) = self.opts.cursor_x
            {
                if start == cursor_x && j == start + 1 {
                    break;
                }
                if start < cursor_x && j == cursor_x {
                    break;
                }
            }

            let cps: &[u32] = if cell.has_grapheme() {
                graphemes
                    .get(j)
                    .and_then(|slice| unsafe { slice.as_slice() })
                    .unwrap_or(&[])
            } else {
                &[]
            };
            let presentation = cell_presentation(cell, cps);

            let font_info = if let Some(idx) =
                self.opts
                    .grid
                    .index_for_cell(cell, cps, run_variant, presentation)
            {
                FontInfo {
                    idx,
                    fallback: None,
                }
            } else if let Some(idx) = self.opts.grid.get_index(0xFFFD, run_variant, presentation) {
                FontInfo {
                    idx,
                    fallback: Some(0xFFFD),
                }
            } else if let Some(idx) =
                self.opts
                    .grid
                    .get_index(b' ' as u32, run_variant, presentation)
            {
                FontInfo {
                    idx,
                    fallback: Some(b' ' as u32),
                }
            } else {
                return None;
            };

            if j == start {
                current_font = Some(font_info.idx);
            }
            if Some(font_info.idx) != current_font {
                break;
            }

            if let Some(cp) = font_info.fallback {
                self.add_codepoint(&mut hasher, cp, cluster);
                j += 1;
                continue;
            }

            if cell.codepoint() == KITTY_UNICODE_PLACEHOLDER {
                self.add_codepoint(&mut hasher, b' ' as u32, cluster);
                j += 1;
                continue;
            }

            self.add_codepoint(&mut hasher, normalize_codepoint(cell.codepoint()), cluster);
            if cell.has_grapheme() {
                for &cp in cps {
                    let cp = cp & 0x1F_FFFF;
                    // Ghostty omits presentation selectors from shaping input.
                    if cp == 0xFE0E || cp == 0xFE0F {
                        continue;
                    }
                    self.add_codepoint(&mut hasher, cp, cluster);
                }
            }

            j += 1;
        }

        let current_font = current_font?;
        self.hooks.finalize();
        self.i = j;
        let cells = (j - start) as u16;

        Some(TextRun {
            hash: hasher.finish(cells as u32, current_font.to_u16() as u64),
            offset: start as u16,
            cells,
            font_index: current_font,
        })
    }

    fn add_codepoint(&mut self, hasher: &mut RunHasher, cp: u32, cluster: u32) {
        hasher.add_codepoint(cp, cluster);
        self.hooks.add_codepoint(cp, cluster);
    }
}

#[derive(Clone, Copy)]
struct FontInfo {
    idx: FontIndex,
    fallback: Option<u32>,
}

#[inline]
fn trim_right_empty(raw_cells: &[RawCell]) -> usize {
    let mut max = raw_cells.len();
    while max > 0 && is_empty_cell(raw_cells[max - 1]) {
        max -= 1;
    }
    max
}

#[inline]
fn is_empty_cell(raw: RawCell) -> bool {
    !raw.has_text() && !raw.is_bg_only()
}

#[inline]
fn style_at(styles: &[CellStyle], idx: usize, raw: RawCell) -> Option<&CellStyle> {
    if raw.style_id() != 0 {
        styles.get(idx)
    } else {
        None
    }
}

#[inline]
fn comparable_style(style: Option<&CellStyle>) -> ComparableStyle {
    ComparableStyle {
        // Foreground color is deliberately excluded: runs are allowed to span
        // fg-color changes so that ligatures form across per-cell color
        // boundaries (e.g. PowerShell 5 tokenization coloring `-` and `>` in
        // different colors). The renderer handles multi-color ligatures by
        // splitting the emitted quad at cell boundaries when fg colors differ
        // (WT-style overlap splitting). Background color is also excluded
        // (matches Ghostty).
        //
        // This is a tradeoff with our shaped run cache. This should be revisited based
        // on profiling and any data about these patterns in common terminal workloads.
        //
        // Worse cases: Text that previously shared a common sub-run now won't.
        // Example: "Hello World!" with only the '!' colored on one line and just
        //          "Hello World" on another. Previously, both lines shared the cache
        //          for "Hello World", with only "!" being shaped fresh. Now they don't.
        //
        // Better cases: Identical text with different coloring patterns.
        // Example: Previously, hello in red-then-blue vs all-white would produce
        //          different run splits and be separate cache entries. Now both
        //          shape as one hello run - increased cache hits for this pattern.
        underline: packed_style_color(style.map(|s| s.underline)),
        flags: style.map_or(0, |s| s.flags),
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct ComparableStyle {
    underline: u32,
    flags: u16,
}

#[inline]
fn packed_style_color(c: Option<StyleColor>) -> u32 {
    if let Some(c) = c {
        u32::from_le_bytes([c.r, c.g, c.b, c.tag])
    } else {
        0
    }
}

#[inline]
fn font_variant_from_style(style: Option<&CellStyle>) -> Style {
    let bold = style.is_some_and(CellStyle::is_bold);
    let italic = style.is_some_and(CellStyle::is_italic);
    match (bold, italic) {
        (false, false) => Style::Normal,
        (true, false) => Style::Bold,
        (false, true) => Style::Italic,
        (true, true) => Style::BoldItalic,
    }
}

#[inline]
fn normalize_codepoint(cp: u32) -> u32 {
    if cp == 0 { b' ' as u32 } else { cp }
}

#[inline]
fn split_bad_ligature(prev: u32, current: u32) -> bool {
    (prev == b'f' as u32 && (current == b'l' as u32 || current == b'i' as u32))
        || (prev == b's' as u32 && current == b't' as u32)
}

#[inline]
fn cell_presentation(cell: RawCell, grapheme: &[u32]) -> Option<Presentation> {
    if !cell.has_grapheme() {
        return None;
    }
    let first = grapheme.first().copied().map(|cp| cp & 0x1F_FFFF)?;
    if first == 0xFE0E {
        Some(Presentation::Text)
    } else if first == 0xFE0F {
        Some(Presentation::Emoji)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "windows")]
    use crate::backend::dwrite::variation::StyleVariationRequest;
    use crate::collection::Collection;
    use crate::shared_grid::GridMetrics;
    #[cfg(target_os = "windows")]
    use windows::Win32::Graphics::DirectWrite::{
        DWRITE_FACTORY_TYPE_SHARED, DWriteCreateFactory, IDWriteFactory6,
    };

    #[inline]
    fn face(id: u16) -> FontIndex {
        FontIndex::new(Style::Normal, id)
    }

    #[inline]
    fn raw_codepoint(cp: u32, style_id: u16) -> RawCell {
        let bits = ((cp as u64) << 2) | ((style_id as u64) << 26);
        // SAFETY: RawCell is repr(transparent) over u64.
        unsafe { std::mem::transmute(bits) }
    }

    #[inline]
    fn raw_grapheme_codepoint(cp: u32, style_id: u16) -> RawCell {
        let bits = 1u64 | ((cp as u64) << 2) | ((style_id as u64) << 26);
        // SAFETY: RawCell is repr(transparent) over u64.
        unsafe { std::mem::transmute(bits) }
    }

    #[inline]
    fn raw_spacer(style_id: u16) -> RawCell {
        let bits = ((style_id as u64) << 26) | ((2u64) << 42);
        // SAFETY: RawCell is repr(transparent) over u64.
        unsafe { std::mem::transmute(bits) }
    }

    #[inline]
    fn style(fg_tag: u8, fg_r: u8, flags: u16) -> CellStyle {
        // SAFETY: CellStyle is POD FFI mirror.
        let mut s: CellStyle = unsafe { std::mem::zeroed() };
        s.fg.r = fg_r;
        s.fg.g = 0;
        s.fg.b = 0;
        s.fg.tag = fg_tag;
        s.underline.tag = 0;
        s.bg.tag = 0;
        s.flags = flags;
        s
    }

    #[inline]
    fn empty_grapheme_slice() -> GraphemeSlice {
        GraphemeSlice {
            ptr: std::ptr::null(),
            len: 0,
        }
    }

    fn build_grid(mapping: &[(u32, FontIndex)]) -> SharedGrid {
        let grid = SharedGrid::with_collection(Collection::new(), GridMetrics::default());
        for &(cp, idx) in mapping {
            grid.test_prime_index(cp, Style::Normal, None, Some(idx));
        }
        grid
    }

    #[cfg(target_os = "windows")]
    fn integration_grid() -> SharedGrid {
        let factory6: IDWriteFactory6 =
            unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).expect("create dwrite") };
        let mut grid = SharedGrid::with_collection(Collection::new(), GridMetrics::default());
        let requests: [StyleVariationRequest<'_>; Style::COUNT] =
            std::array::from_fn(|_| StyleVariationRequest {
                family: "Segoe UI Emoji",
                axes: Default::default(),
            });
        grid.configure_dwrite(
            &factory6,
            &requests,
            crate::backend::dwrite::face::DWriteGridMetricsConfig {
                font_size: 16.0,
                cell_width: 0.0,
                line_height: 0.0,
                baseline: 0.0,
            },
            16.0,
            None,
            "en-US",
        )
        .expect("configure dwrite");
        grid
    }

    fn collect_runs(
        raw: &[RawCell],
        styles: &[CellStyle],
        graphemes: &[GraphemeSlice],
        selection: Option<[u16; 2]>,
        cursor_x: Option<usize>,
        grid: &SharedGrid,
    ) -> Vec<TextRun> {
        let mut codepoints = Vec::new();
        let mut utf16_buf = Vec::new();
        let mut it = RunIterator::new(
            RunOptions {
                grid,
                cells: RowCells {
                    raw_cells: raw,
                    styles,
                    graphemes,
                },
                selection,
                cursor_x,
            },
            RunIteratorHook::for_test(&mut codepoints, &mut utf16_buf),
        );
        let mut out = Vec::new();
        while let Some(run) = it.next() {
            out.push(run);
        }
        out
    }

    fn collect_runs_with_buffers(
        raw: &[RawCell],
        styles: &[CellStyle],
        graphemes: &[GraphemeSlice],
        grid: &SharedGrid,
    ) -> (Vec<TextRun>, Vec<Codepoint>, Vec<u16>) {
        let mut codepoints = Vec::new();
        let mut utf16_buf = Vec::new();
        let mut it = RunIterator::new(
            RunOptions {
                grid,
                cells: RowCells {
                    raw_cells: raw,
                    styles,
                    graphemes,
                },
                selection: None,
                cursor_x: None,
            },
            RunIteratorHook::for_test(&mut codepoints, &mut utf16_buf),
        );
        let first = it.next().into_iter().collect();
        (first, codepoints, utf16_buf)
    }

    #[test]
    fn font_switch_splits_run() {
        let grid =
            build_grid(&[('a' as u32, face(1)), ('b' as u32, face(2)), (' ' as u32, face(1))]);

        let raw = [raw_codepoint('a' as u32, 0), raw_codepoint('b' as u32, 0)];
        let styles = [style(0, 0, 0), style(0, 0, 0)];
        let graphemes = [empty_grapheme_slice(), empty_grapheme_slice()];
        let runs = collect_runs(&raw, &styles, &graphemes, None, None, &grid);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].font_index, face(1));
        assert_eq!(runs[1].font_index, face(2));
    }

    #[test]
    fn relative_position_hash_stability() {
        let grid = build_grid(&[
            ('a' as u32, face(3)),
            ('b' as u32, face(3)),
            ('x' as u32, face(3)),
            (' ' as u32, face(3)),
        ]);

        let base_raw = [raw_codepoint('a' as u32, 0), raw_codepoint('b' as u32, 0)];
        let base_styles = [style(0, 0, 0), style(0, 0, 0)];
        let base_g = [empty_grapheme_slice(), empty_grapheme_slice()];
        let base_hash = collect_runs(&base_raw, &base_styles, &base_g, None, None, &grid)[0].hash;

        let shifted_raw = [
            raw_codepoint('x' as u32, 0),
            raw_codepoint('a' as u32, 0),
            raw_codepoint('b' as u32, 0),
        ];
        let shifted_styles = [style(0, 0, 0), style(0, 0, 0), style(0, 0, 0)];
        let shifted_g = [empty_grapheme_slice(), empty_grapheme_slice(), empty_grapheme_slice()];
        let runs = collect_runs(
            &shifted_raw,
            &shifted_styles,
            &shifted_g,
            Some([1, 2]),
            None,
            &grid,
        );
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[1].offset, 1);
        assert_eq!(runs[1].hash, base_hash);
    }

    #[test]
    fn run_boundary_parity_selection_cursor_style_and_ligature() {
        let grid = build_grid(&[
            ('a' as u32, face(1)),
            ('b' as u32, face(1)),
            ('c' as u32, face(1)),
            ('d' as u32, face(1)),
            ('f' as u32, face(1)),
            ('i' as u32, face(1)),
            (' ' as u32, face(1)),
        ]);

        // fg-only change does NOT split (multi-color ligature support)
        let raw = [raw_codepoint('a' as u32, 1), raw_codepoint('b' as u32, 2)];
        let styles = [style(2, 10, 0), style(2, 20, 0)];
        let g = [empty_grapheme_slice(), empty_grapheme_slice()];
        assert_eq!(collect_runs(&raw, &styles, &g, None, None, &grid).len(), 1);

        // flags change DOES split (e.g. strikethrough boundary)
        // Use a non-bold/italic flag (1 << 6 = strikethrough) so font_variant
        // stays Normal and the second run can still resolve in the test grid.
        let raw = [raw_codepoint('a' as u32, 1), raw_codepoint('b' as u32, 2)];
        let styles = [style(2, 10, 0), style(2, 10, 1 << 6)];
        let g = [empty_grapheme_slice(), empty_grapheme_slice()];
        assert_eq!(collect_runs(&raw, &styles, &g, None, None, &grid).len(), 2);

        // selection split
        let raw = [
            raw_codepoint('a' as u32, 0),
            raw_codepoint('b' as u32, 0),
            raw_codepoint('c' as u32, 0),
            raw_codepoint('d' as u32, 0),
        ];
        let styles = [style(0, 0, 0), style(0, 0, 0), style(0, 0, 0), style(0, 0, 0)];
        let g = [
            empty_grapheme_slice(),
            empty_grapheme_slice(),
            empty_grapheme_slice(),
            empty_grapheme_slice(),
        ];
        assert_eq!(
            collect_runs(&raw, &styles, &g, Some([1, 2]), None, &grid).len(),
            3
        );

        // cursor split
        let raw = [
            raw_codepoint('a' as u32, 0),
            raw_codepoint('b' as u32, 0),
            raw_codepoint('c' as u32, 0),
        ];
        let styles = [style(0, 0, 0), style(0, 0, 0), style(0, 0, 0)];
        let g = [empty_grapheme_slice(), empty_grapheme_slice(), empty_grapheme_slice()];
        assert_eq!(
            collect_runs(&raw, &styles, &g, None, Some(1), &grid).len(),
            3
        );

        // bad ligature split
        let raw = [raw_codepoint('f' as u32, 0), raw_codepoint('i' as u32, 0)];
        let styles = [style(0, 0, 0), style(0, 0, 0)];
        let g = [empty_grapheme_slice(), empty_grapheme_slice()];
        assert_eq!(collect_runs(&raw, &styles, &g, None, None, &grid).len(), 2);

        // spacer ignored
        let raw = [raw_codepoint('a' as u32, 0), raw_spacer(0), raw_codepoint('b' as u32, 0)];
        let styles = [style(0, 0, 0), style(0, 0, 0), style(0, 0, 0)];
        let g = [empty_grapheme_slice(), empty_grapheme_slice(), empty_grapheme_slice()];
        assert_eq!(collect_runs(&raw, &styles, &g, None, None, &grid).len(), 1);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn run_iterator_omits_variation_selectors_from_shaping_buffers() {
        let grid = integration_grid();
        let raw = [raw_grapheme_codepoint(0x2764, 0)];
        let styles = [style(0, 0, 0)];
        let grapheme_data = [0xFE0F];
        let graphemes = [GraphemeSlice {
            ptr: grapheme_data.as_ptr(),
            len: grapheme_data.len(),
        }];

        let (runs, codepoints, utf16_buf) =
            collect_runs_with_buffers(&raw, &styles, &graphemes, &grid);

        assert_eq!(runs.len(), 1);
        assert_eq!(codepoints.len(), 1);
        assert_eq!(codepoints[0].codepoint, 0x2764);
        assert_eq!(codepoints[0].cluster, 0);
        assert_eq!(utf16_buf, vec![0x2764]);
    }

    #[test]
    fn run_iterator_adds_dummy_codepoint_for_surrogate_pairs() {
        let grid = build_grid(&[(0x1F34E, face(1)), (' ' as u32, face(1))]);
        let raw = [raw_codepoint(0x1F34E, 0)];
        let styles = [style(0, 0, 0)];
        let graphemes = [empty_grapheme_slice()];

        let (runs, codepoints, utf16_buf) =
            collect_runs_with_buffers(&raw, &styles, &graphemes, &grid);

        assert_eq!(runs.len(), 1);
        assert_eq!(utf16_buf.len(), 2);
        assert_eq!(codepoints.len(), 2);
        assert_eq!(codepoints[0].codepoint, 0x1F34E);
        assert_eq!(codepoints[0].cluster, 0);
        assert_eq!(codepoints[1].codepoint, 0);
        assert_eq!(codepoints[1].cluster, 0);
    }
}
