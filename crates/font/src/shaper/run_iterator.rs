use std::hash::Hasher as _;

use crate::shared_grid::SharedGrid;
use crate::types::{FontError, FontIndex, FontStyle, Presentation, TextRun};
use ghostty::{CellStyle, CellView, ContentTag, StyleColor, U21, Width};
use rustc_hash::FxHasher;
use utils::asserts::assert;

const KITTY_UNICODE_PLACEHOLDER: u32 = 0x10EEEE;

/// Hook that writes into the Shaper's owned buffers.
/// Ghostty: `Shaper.RunIteratorHook`.
pub trait RunIteratorHook {
    fn prepare(&mut self);
    fn add_codepoint(&mut self, codepoint: u32, cluster: u32);
    fn finalize(&mut self);
}

pub struct RunOptions<'a> {
    pub cells: &'a CellView<'a>,
    pub grid: &'a SharedGrid,
    pub cursor_x: Option<usize>,
    pub selection: Option<[u16; 2]>,
}

pub struct RunIterator<'a> {
    options: RunOptions<'a>,
    max: usize,
    i: usize,
}

impl<'a> RunIterator<'a> {
    pub fn new(options: RunOptions<'a>) -> Self {
        let max: usize = options
            .cells
            .raw_cells()
            .iter()
            .rposition(|cell| !cell.is_empty())
            .map_or(0, |position| position + 1);

        Self { options, max, i: 0 }
    }

    pub fn next(
        &mut self,
        hooks: &mut impl RunIteratorHook,
    ) -> Result<Option<TextRun<'a>>, FontError> {
        let cells = self.options.cells.raw_cells();
        let styles = self.options.cells.styles();
        let grapheme_views = self.options.cells.graphemes();

        assert(self.i <= cells.len());
        assert(styles.len() == cells.len());
        assert(grapheme_views.len() == cells.len());

        assert(self.i <= self.max);

        self.i += cells[self.i..self.max]
            .iter()
            .zip(&styles[self.i..self.max])
            .take_while(|&(cell, style)| {
                cell.has_styling() && unsafe { style.assume_init_ref() }.is_invisible()
            })
            .count();

        if self.i >= self.max {
            return Ok(None);
        }

        hooks.prepare();
        let mut current_font = FontIndex::DEFAULT;
        let mut hasher = FxHasher::default();

        let style = match cells[self.i].has_styling() {
            true => unsafe { styles[self.i].assume_init_ref() },
            false => CellStyle::DEFAULT,
        };

        let current_comparable_style = comparable_style(style);
        let font_style = match (style.is_bold(), style.is_italic()) {
            (false, false) => FontStyle::Regular,
            (true, false) => FontStyle::Bold,
            (false, true) => FontStyle::Italic,
            (true, true) => FontStyle::BoldItalic,
        };

        let mut j = self.i;
        while j < self.max {
            // Use relative cluster positions (offset from the run start)
            // to make the shaping cache position-independent
            let cluster = (j - self.i) as u32;
            let cell = cells[j];

            if let Some([sel_start, sel_end]) = self.options.selection {
                // Break the run at the boundary of a selection.
                if j > self.i {
                    if sel_start > 0 && (j == sel_start as usize) {
                        break;
                    }
                    if sel_end > 0 && (j == (sel_end + 1) as usize) {
                        break;
                    }
                }
            }

            match cell.width() {
                Width::Narrow | Width::Wide => {}
                Width::SpacerHead | Width::SpacerTail => {
                    j += 1;
                    continue;
                }
            }

            if j > self.i {
                // Split the run on common bad ligatures
                const F: u32 = b'f' as u32;
                const S: u32 = b's' as u32;

                let prev_cell = cells[j - 1];
                if prev_cell.content_tag() == ContentTag::Codepoint
                    && cell.content_tag() == ContentTag::Codepoint
                {
                    match prev_cell.codepoint() {
                        F => {
                            // fl, fi
                            let cp = cell.codepoint();
                            if cp == u32::from('l') || cp == u32::from('i') {
                                break;
                            }
                        }
                        // st
                        S if cell.codepoint() == u32::from('t') => break,
                        _ => {}
                    }
                }

                if prev_cell.style_id() != cell.style_id() {
                    let candidate_style = match cell.has_styling() {
                        true => unsafe { styles[j].assume_init_ref() },
                        false => CellStyle::DEFAULT,
                    };
                    if current_comparable_style != comparable_style(candidate_style) {
                        break;
                    };
                }
            }

            let graphemes = match cell.has_grapheme() {
                true => {
                    let view = unsafe { grapheme_views[j].assume_init_ref() };
                    unsafe { Some(view.as_slice().unwrap()) }
                }
                false => None,
            };

            let presentation = graphemes.and_then(|graphemes| match graphemes[0].get() {
                0xFE0E => Some(Presentation::Text),
                0xFE0F => Some(Presentation::Emoji),
                _ => None,
            });

            if graphemes.is_none() {
                // Break the run around the cursor. The cursor does
                // not break the run if it is part of a grapheme
                if let Some(cursor_x) = self.options.cursor_x {
                    // This cell is the cursor.
                    // Break so the cursor is on a separate run
                    if self.i == cursor_x && j == self.i + 1 {
                        break;
                    }
                    // Just before the cursor. Break so the cursor gets a
                    // new run (caught by the previous condition).
                    if self.i < cursor_x && j == cursor_x {
                        break;
                    }
                    // After the cursor. Continue normally.
                }
            }

            let cp = cell.codepoint();
            let grid = self.options.grid;
            let (idx, fallback): (FontIndex, Option<char>) = 'font_info: {
                // Look for a font that supports the entire grapheme.
                if let Some(idx) = self.index_for_cell(cp, font_style, graphemes, presentation)? {
                    break 'font_info (idx, None);
                }

                // Look for a font that has the Unicode Replacement Character (0xFFFD)
                if let Some(idx) = grid.get_index(0xFFFD, font_style, presentation)? {
                    break 'font_info (idx, Some('\u{FFFD}'));
                }

                // Fallback to space
                if let Some(idx) = grid.get_index(u32::from(' '), font_style, presentation)? {
                    break 'font_info (idx, Some(' '));
                }

                // Reaching this is a bug. There should be at least
                // one font that can render a space.
                unreachable!()
            };

            if j == self.i {
                current_font = idx;
            }

            // Break the run if the font changes
            if idx != current_font {
                break;
            }

            // If this is a fallback character, add it and
            // continue instead of adding the entire grapheme.
            if let Some(codepoint) = fallback.map(|cp| u32::from(cp)) {
                Self::add_codepoint(&mut hasher, codepoint, cluster);
                hooks.add_codepoint(codepoint, cluster);
                j += 1;
                continue;
            }

            let codepoint = match cp {
                // Add a blank if the cell is empty or a Kitty unicode placeholder
                0 | KITTY_UNICODE_PLACEHOLDER => u32::from(' '),
                _ => cp,
            };

            Self::add_codepoint(&mut hasher, codepoint, cluster);
            hooks.add_codepoint(codepoint, cluster);

            if let Some(graphemes) = graphemes
                && cp != KITTY_UNICODE_PLACEHOLDER
            {
                for &grapheme in graphemes {
                    let grapheme = grapheme.get();
                    // Ghostty omits presentation selectors from shaping input.
                    if grapheme == 0xFE0E || grapheme == 0xFE0F {
                        continue;
                    }
                    Self::add_codepoint(&mut hasher, grapheme, cluster);
                    hooks.add_codepoint(grapheme, cluster);
                }
            }

            j += 1;
        }
        hooks.finalize();
        hasher.write_usize(j - self.i);
        hasher.write_u16(current_font.to_u16());

        let run = TextRun {
            hash: hasher.finish(),
            grid: self.options.grid,
            offset: self.i as u16,
            cells: (j - self.i) as u16,
            font_index: current_font,
        };
        self.i = j;
        Ok(Some(run))
    }

    #[inline]
    fn add_codepoint(hasher: &mut FxHasher, cp: u32, cluster: u32) {
        hasher.write_u32(cp);
        hasher.write_u32(cluster);
    }

    #[inline]
    fn index_for_cell(
        &self,
        primary_cp: u32,
        font_style: FontStyle,
        graphemes: Option<&[U21]>,
        presentation: Option<Presentation>,
    ) -> Result<Option<FontIndex>, FontError> {
        let grid = self.options.grid;

        if primary_cp == 0 || primary_cp == KITTY_UNICODE_PLACEHOLDER {
            return grid.get_index(u32::from(' '), font_style, presentation);
        }

        let Some(primary) = grid.get_index(primary_cp, font_style, presentation)? else {
            return Ok(None);
        };

        let Some(graphemes) = graphemes else {
            return Ok(Some(primary));
        };

        // Check if the primary font has all codepoints needed for the grapheme
        if self.candidate_supports_grapheme(primary, primary_cp, graphemes, presentation) {
            return Ok(Some(primary));
        }

        // Search for a candidate that has the needed grapheme codepoint, then
        // check if it supports all other codepoints in the grapheme.
        for &codepoint in graphemes {
            let cp = codepoint.get();
            // Ignore presentation selectors and emoji ZWJs
            if cp == 0xFE0E || cp == 0xFE0F || cp == 0x200D {
                continue;
            }
            let Some(idx) = grid.get_index(cp, font_style, None)? else {
                return Ok(None);
            };

            if idx == primary {
                continue;
            }
            if self.candidate_supports_grapheme(idx, primary_cp, graphemes, presentation) {
                return Ok(Some(idx));
            }
        }
        Ok(None)
    }

    fn candidate_supports_grapheme(
        &self,
        idx: FontIndex,
        primary_cp: u32,
        graphemes: &[U21],
        presentation: Option<Presentation>,
    ) -> bool {
        let grid = self.options.grid;
        if !grid.has_codepoint(idx, primary_cp, presentation) {
            return false;
        }
        graphemes
            .iter()
            .map(|&cp| cp.get())
            .filter(|&cp| !matches!(cp, 0xFE0E | 0xFE0F | 0x200D))
            .all(|cp| grid.has_codepoint(idx, cp, None))
    }
}

#[inline]
fn comparable_style(style: &CellStyle) -> (StyleColor, u16) {
    // Excluded: Background color (doesn't affect shaping), matching Ghostty
    //
    // Excluded: Foreground color, so ligatures can span fg-color changes. This
    // is a tradeoff with the shaped run cache. Revisit if profiling says so.
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
    (style.underline, style.flags)
}
