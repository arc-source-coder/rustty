use std::cell::UnsafeCell;

use anyhow::{Context, Result, anyhow};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_GLYPH_OFFSET, DWRITE_LINE_BREAKPOINT, DWRITE_NUMBER_SUBSTITUTION_METHOD_CONTEXTUAL,
    DWRITE_READING_DIRECTION, DWRITE_READING_DIRECTION_LEFT_TO_RIGHT, DWRITE_SCRIPT_ANALYSIS,
    DWRITE_SCRIPT_SHAPES, DWRITE_TYPOGRAPHIC_FEATURES, IDWriteFactory2, IDWriteFontFace,
    IDWriteFontFace2, IDWriteNumberSubstitution, IDWriteTextAnalysisSink,
    IDWriteTextAnalysisSink_Impl, IDWriteTextAnalysisSource, IDWriteTextAnalysisSource_Impl,
    IDWriteTextAnalyzer1,
};
use windows::core::{OutRef, PCWSTR, Ref, implement};
use windows_core::{BOOL, Interface};

use super::arena::{
    ArenaResetMode, OutputArena, OutputCaps, OutputUsed, ScratchArena, ScratchCaps,
};
use crate::shaper::shaper::Codepoint;
use crate::types::{
    BidiRun, Cell, GlyphOffset, RunSpan, ScriptRun, ShapeOptions, ShapedCells, TextRun,
};

/// Windows-only DirectWrite shaping engine.
///
/// Design goals:
/// - Ghostty-like stable run/cell model (`ShapedCells`) with reusable storage.
/// - WT-like backend logic (`AnalyzeScript`, `MapCharacters`, `GetGlyphs`,
///   `GetGlyphPlacements`) and fast/simple text path usage.
/// - Zero-allocation steady-state in hot `shape()` calls after warm-up.
pub struct DWriteAnalyzer {
    factory: IDWriteFactory2,
    analyzer: IDWriteTextAnalyzer1,

    locale_name: String,
    locale_utf16: Vec<u16>,
    number_substitution: Option<IDWriteNumberSubstitution>,

    script_runs_used: usize,
    bidi_runs_used: usize,
    output: OutputArena,
    scratch: ScratchArena,

    runs_used: usize,
    run_spans_used: usize,
    cells_used: usize,
    cluster_used: usize,
    glyph_used: usize,
}

impl DWriteAnalyzer {
    pub fn new(factory: &IDWriteFactory2) -> Result<Self> {
        // SAFETY: COM factory is valid for this call.
        let analyzer = unsafe { factory.CreateTextAnalyzer() }?.cast::<IDWriteTextAnalyzer1>()?;
        Ok(Self {
            factory: factory.clone(),
            analyzer,
            locale_name: String::new(),
            locale_utf16: Vec::new(),
            number_substitution: None,
            script_runs_used: 0,
            bidi_runs_used: 0,
            output: OutputArena::new(),
            scratch: ScratchArena::new(),
            runs_used: 0,
            run_spans_used: 0,
            cells_used: 0,
            cluster_used: 0,
            glyph_used: 0,
        })
    }

    /// Optional viewport-driven reservation hook used by renderer thread setup.
    ///
    /// Pre-sizing avoids mid-frame growth and keeps shape() on the fast path.
    pub fn reserve_for(&mut self, max_cells: usize, max_runs: usize) -> Result<()> {
        let projected_glyphs = projected_glyphs(max_cells);
        self.reserve_output(max_cells, projected_glyphs, max_cells + max_runs, max_runs)?;
        self.reserve_scratch(max_cells, projected_glyphs)?;
        Ok(())
    }

    pub fn shape<'a>(
        &'a mut self,
        base_run: TextRun,
        codepoints: &[Codepoint],
        text_utf16: &[u16],
        options: &ShapeOptions,
        face2: &IDWriteFontFace2,
    ) -> Result<ShapedCells<'a>> {
        self.reset_used_lengths();
        if text_utf16.is_empty() {
            return Ok(self.current_shape());
        }
        if text_utf16.len() != codepoints.len() {
            return Err(anyhow!(
                "utf16/codepoint length mismatch: utf16={} codepoints={}",
                text_utf16.len(),
                codepoints.len()
            ));
        }

        let projected = projected_glyphs(text_utf16.len());
        self.reserve_output(
            text_utf16.len(),
            projected,
            text_utf16.len() + 1,
            text_utf16.len().max(1),
        )?;
        self.reserve_scratch(text_utf16.len(), projected)?;

        self.set_locale(&options.locale);
        let source_impl = BorrowedTextAnalysisSource::new(
            &self.locale_utf16,
            text_utf16,
            self.number_substitution.clone(),
        );
        let source_iface: IDWriteTextAnalysisSource = source_impl.into();
        let face = face2
            .cast::<IDWriteFontFace>()
            .context("failed to cast IDWriteFontFace2 to IDWriteFontFace")?;

        self.segment_scripts(&source_iface, text_utf16.len() as u32)?;
        self.segment_bidi(&source_iface, text_utf16.len() as u32)?;

        let mut s_i = 0usize;
        let mut b_i = 0usize;
        while s_i < self.script_runs_used && b_i < self.bidi_runs_used {
            // SAFETY: indices are bounded by `*_used`, which are produced by DWrite analysis.
            let script = unsafe { *self.scratch.script_runs_ptr().add(s_i) };
            // SAFETY: indices are bounded by `*_used`, which are produced by DWrite analysis.
            let bidi = unsafe { *self.scratch.bidi_runs_ptr().add(b_i) };

            let s_start = script.text_position;
            let s_end = script.text_position + script.text_length;
            let b_start = bidi.text_position;
            let b_end = bidi.text_position + bidi.text_length;

            let seg_start = s_start.max(b_start);
            let seg_end = s_end.min(b_end);

            if seg_start < seg_end {
                let rtl = (bidi.resolved_level & 1) != 0;
                let run_text_start = seg_start as usize;
                let run_text_len = (seg_end - seg_start) as usize;
                let run = TextRun {
                    hash: base_run.hash,
                    offset: base_run.offset + seg_start as u16,
                    cells: (seg_end - seg_start) as u16,
                    font_index: base_run.font_index,
                };

                let cell_start = self.cells_used as u32;
                let glyph_start = self.glyph_used as u32;
                let cluster_start = self.cluster_used as u32;

                self.shape_mapped_segment(
                    &codepoints[run_text_start..run_text_start + run_text_len],
                    &text_utf16[run_text_start..run_text_start + run_text_len],
                    options,
                    &script.analysis,
                    &face,
                    rtl,
                )?;

                self.push_run_with_spans(run, cell_start, glyph_start, cluster_start);
            }

            if s_end <= b_end {
                s_i += 1;
            } else {
                b_i += 1;
            }
        }

        Ok(self.current_shape())
    }

    fn shape_mapped_segment(
        &mut self,
        codepoints: &[Codepoint],
        text_slice: &[u16],
        options: &ShapeOptions,
        analysis: &DWRITE_SCRIPT_ANALYSIS,
        face: &IDWriteFontFace,
        rtl: bool,
    ) -> Result<()> {
        let text_len = text_slice.len();
        let projected = projected_glyphs(text_len);
        self.reserve_scratch(text_len, projected)?;

        let locale_pcw = PCWSTR(self.locale_utf16.as_ptr());
        let features = &options.features.features;
        let has_features = !features.is_empty();
        let feature_block = if has_features {
            Some(DWRITE_TYPOGRAPHIC_FEATURES {
                features: features.as_ptr() as *mut _,
                featureCount: features.len() as u32,
            })
        } else {
            None
        };
        let feature_ptrs: [*const DWRITE_TYPOGRAPHIC_FEATURES; 1] = [feature_block
            .as_ref()
            .map_or(std::ptr::null(), |v| v as *const _)];
        let feature_range_lengths = [text_len as u32];
        let features_arg = feature_block.as_ref().map(|_| feature_ptrs.as_ptr());
        let feature_range_arg = feature_block
            .as_ref()
            .map(|_| feature_range_lengths.as_ptr());
        let feature_ranges_count = u32::from(feature_block.is_some());

        if !has_features {
            let mut simple = BOOL(0);
            let mut read = 0u32;
            // SAFETY: buffers are valid for `text_len` writes.
            unsafe {
                self.analyzer.GetTextComplexity(
                    PCWSTR(text_slice.as_ptr()),
                    text_len as u32,
                    face,
                    &mut simple,
                    &mut read,
                    Some(self.scratch.glyph_indices_mut_ptr()),
                )?;
            }
            if simple.as_bool() && read as usize == text_len {
                self.emit_simple_segment(codepoints, options.cell_width)?;
                return Ok(());
            }
        }

        self.shape_complex_segment(
            codepoints,
            text_slice,
            options.font_size,
            analysis,
            &face,
            locale_pcw,
            features_arg,
            feature_range_arg,
            feature_ranges_count,
            rtl,
        )
    }

    fn shape_complex_segment(
        &mut self,
        codepoints: &[Codepoint],
        text_slice: &[u16],
        font_size: f32,
        analysis: &DWRITE_SCRIPT_ANALYSIS,
        face: &IDWriteFontFace,
        locale_pcw: PCWSTR,
        features_arg: Option<*const *const DWRITE_TYPOGRAPHIC_FEATURES>,
        feature_ranges_arg: Option<*const u32>,
        feature_ranges_count: u32,
        rtl: bool,
    ) -> Result<()> {
        let text_len = text_slice.len();
        let text_pcw = PCWSTR(text_slice.as_ptr());
        let mut actual_glyph_count = 0u32;

        let mut ok = false;
        for _ in 0..8 {
            // SAFETY: scratch pointers are valid and sized by `reserve_scratch`.
            let hr = unsafe {
                self.analyzer.GetGlyphs(
                    text_pcw,
                    text_len as u32,
                    face,
                    false,
                    rtl,
                    analysis as *const _,
                    locale_pcw,
                    self.number_substitution.as_ref(),
                    features_arg,
                    feature_ranges_arg,
                    feature_ranges_count,
                    self.scratch.glyph_cap() as u32,
                    self.scratch.cluster_map_mut_ptr(),
                    self.scratch.text_props_mut_ptr(),
                    self.scratch.glyph_indices_mut_ptr(),
                    self.scratch.glyph_props_mut_ptr(),
                    &mut actual_glyph_count,
                )
            };
            if hr.is_ok() {
                ok = true;
                break;
            }
            let grow_to = self
                .scratch
                .glyph_cap()
                .saturating_add(self.scratch.glyph_cap() / 2)
                .max(text_len + 8);
            self.reserve_scratch(text_len, grow_to)?;
        }
        if !ok {
            return Err(anyhow!("GetGlyphs failed after bounded retries"));
        }

        let glyph_len = actual_glyph_count as usize;
        // SAFETY: scratch/output pointers are valid for `glyph_len` and `text_len`.
        unsafe {
            self.analyzer.GetGlyphPlacements(
                text_pcw,
                self.scratch.cluster_map_ptr(),
                self.scratch.text_props_mut_ptr(),
                text_len as u32,
                self.scratch.glyph_indices_ptr(),
                self.scratch.glyph_props_ptr(),
                actual_glyph_count,
                face,
                font_size,
                false,
                rtl,
                analysis as *const _,
                locale_pcw,
                features_arg,
                feature_ranges_arg,
                feature_ranges_count,
                self.scratch.glyph_advances_mut_ptr(),
                self.scratch.glyph_offsets_mut_ptr() as *mut DWRITE_GLYPH_OFFSET,
            )?;
            // SAFETY: cluster map has `text_len + 1` capacity by scratch reservation.
            *self.scratch.cluster_map_mut_ptr().add(text_len) = actual_glyph_count as u16;
        }

        self.emit_complex_segment(codepoints, glyph_len)?;
        Ok(())
    }

    fn emit_simple_segment(&mut self, codepoints: &[Codepoint], cell_width: f32) -> Result<()> {
        let text_len = codepoints.len();
        self.reserve_output(text_len, text_len, text_len + 1, 0)?;
        // SAFETY: output/scratch regions are guaranteed by reserve calls.
        unsafe {
            let cell_ptr = self.output.cells_mut_ptr().add(self.cells_used);
            let adv_ptr = self.output.glyph_advances_mut_ptr().add(self.glyph_used);
            let off_ptr = self.output.glyph_offsets_mut_ptr().add(self.glyph_used);
            let cl_ptr = self.output.cluster_map_mut_ptr().add(self.cluster_used);
            let glyph_idx_ptr = self.scratch.glyph_indices_ptr();
            let codepoints = codepoints.as_ptr();

            for i in 0..=text_len {
                *cl_ptr.add(i) = i as u16;
            }
            for i in 0..text_len {
                *cell_ptr.add(i) = Cell {
                    x: cluster_to_cell_x(*codepoints.add(i)),
                    x_offset: 0,
                    y_offset: 0,
                    glyph_index: *glyph_idx_ptr.add(i) as u32,
                };
                *adv_ptr.add(i) = cell_width;
                *off_ptr.add(i) = GlyphOffset::default();
            }
        }
        self.cluster_used += text_len + 1;
        self.cells_used += text_len;
        self.glyph_used += text_len;
        Ok(())
    }

    fn emit_complex_segment(&mut self, codepoints: &[Codepoint], glyph_len: usize) -> Result<()> {
        let text_len = codepoints.len();
        debug_assert!(text_len > 0);
        self.reserve_output(text_len.max(glyph_len), glyph_len, text_len + 1, 0)?;

        // SAFETY: destination capacity reserved; sources produced by DWrite.
        unsafe {
            let dst_cluster = self.output.cluster_map_mut_ptr().add(self.cluster_used);
            let dst_adv = self.output.glyph_advances_mut_ptr().add(self.glyph_used);
            let dst_off = self.output.glyph_offsets_mut_ptr().add(self.glyph_used);
            let dst_cell = self.output.cells_mut_ptr().add(self.cells_used);
            let src_cluster = self.scratch.cluster_map_ptr();
            let src_adv = self.scratch.glyph_advances_ptr();
            let src_off = self.scratch.glyph_offsets_ptr();
            let src_idx = self.scratch.glyph_indices_ptr();
            let codepoints = codepoints.as_ptr();

            std::ptr::copy_nonoverlapping(src_cluster, dst_cluster, text_len + 1);
            std::ptr::copy_nonoverlapping(src_adv, dst_adv, glyph_len);
            std::ptr::copy_nonoverlapping(src_off, dst_off, glyph_len);

            // SAFETY: loop invariants ensure all reads/writes are in-bounds.
            let mut glyph_cluster = 0usize;
            let mut cluster_glyph_start = *src_cluster as usize;
            let mut cluster_min_x = cluster_to_cell_x(*codepoints);
            for glyph_index in 0..glyph_len {
                while glyph_cluster + 1 <= text_len
                    && *src_cluster.add(glyph_cluster + 1) as usize <= glyph_index
                {
                    glyph_cluster += 1;
                    let next_glyph_start = *src_cluster.add(glyph_cluster) as usize;
                    if next_glyph_start != cluster_glyph_start {
                        cluster_glyph_start = next_glyph_start;
                        cluster_min_x = cluster_to_cell_x(*codepoints.add(glyph_cluster));
                    } else {
                        cluster_min_x =
                            cluster_min_x.min(cluster_to_cell_x(*codepoints.add(glyph_cluster)));
                    }
                }
                let off = *src_off.add(glyph_index);
                debug_assert!(glyph_cluster < text_len);
                *dst_cell.add(glyph_index) = Cell {
                    x: cluster_min_x,
                    x_offset: off.advance_offset.round() as i16,
                    y_offset: off.ascender_offset.round() as i16,
                    glyph_index: *src_idx.add(glyph_index) as u32,
                };
            }
        }

        self.cluster_used += text_len + 1;
        self.cells_used += glyph_len;
        self.glyph_used += glyph_len;
        Ok(())
    }

    fn segment_scripts(
        &mut self,
        source_iface: &IDWriteTextAnalysisSource,
        text_len: u32,
    ) -> Result<()> {
        self.script_runs_used = 0;
        let sink_impl = ScriptAnalysisSink::new(
            // SAFETY: reserve_scratch ensures script-run region exists.
            unsafe { self.scratch.script_runs_mut_ptr() },
            self.scratch.script_runs_cap(),
            &mut self.script_runs_used,
        );
        let sink_iface: IDWriteTextAnalysisSink = sink_impl.into();

        // SAFETY: AnalyzeScript is synchronous and sink/source outlive call.
        unsafe {
            self.analyzer
                .AnalyzeScript(source_iface, 0, text_len, &sink_iface)?;
        }
        if self.script_runs_used == 0 {
            let run = ScriptRun {
                text_position: 0,
                text_length: text_len,
                analysis: DWRITE_SCRIPT_ANALYSIS {
                    script: 0,
                    shapes: DWRITE_SCRIPT_SHAPES(0),
                },
            };
            // SAFETY: reserve_scratch ensures at least one slot.
            unsafe { *self.scratch.script_runs_mut_ptr() = run };
            self.script_runs_used = 1;
        }
        Ok(())
    }

    fn segment_bidi(
        &mut self,
        source_iface: &IDWriteTextAnalysisSource,
        text_len: u32,
    ) -> Result<()> {
        self.bidi_runs_used = 0;
        let sink_impl = BidiAnalysisSink::new(
            // SAFETY: reserve_scratch ensures bidi-run region exists.
            unsafe { self.scratch.bidi_runs_mut_ptr() },
            self.scratch.bidi_runs_cap(),
            &mut self.bidi_runs_used,
        );
        let sink_iface: IDWriteTextAnalysisSink = sink_impl.into();

        // SAFETY: all analysis calls are synchronous; sink/source outlive the calls.
        unsafe {
            self.analyzer
                .AnalyzeBidi(source_iface, 0, text_len, &sink_iface)?;
            self.analyzer
                .AnalyzeNumberSubstitution(source_iface, 0, text_len, &sink_iface)?;
            self.analyzer
                .AnalyzeLineBreakpoints(source_iface, 0, text_len, &sink_iface)?;
        }

        if self.bidi_runs_used == 0 {
            let run = BidiRun {
                text_position: 0,
                text_length: text_len,
                resolved_level: 0,
            };
            // SAFETY: reserve_scratch ensures at least one slot.
            unsafe { *self.scratch.bidi_runs_mut_ptr() = run };
            self.bidi_runs_used = 1;
        }
        Ok(())
    }

    fn reserve_output(
        &mut self,
        cells_add: usize,
        glyph_add: usize,
        cluster_add: usize,
        runs_add: usize,
    ) -> Result<()> {
        let req = OutputCaps {
            runs: self.runs_used + runs_add,
            spans: self.run_spans_used + runs_add,
            cells: self.cells_used + cells_add,
            cluster: self.cluster_used + cluster_add,
            glyph: self.glyph_used + glyph_add,
        };
        let used = OutputUsed {
            runs: self.runs_used,
            spans: self.run_spans_used,
            cells: self.cells_used,
            cluster: self.cluster_used,
            glyph: self.glyph_used,
        };
        self.output.ensure(req, used)
    }

    fn reserve_scratch(&mut self, text_len: usize, glyph_len: usize) -> Result<()> {
        self.scratch.ensure(ScratchCaps {
            script_runs: text_len.max(1),
            bidi_runs: text_len.max(1),
            text: text_len,
            glyph: glyph_len,
        })
    }

    fn set_locale(&mut self, locale: &str) {
        if self.locale_name == locale {
            return;
        }
        self.locale_name.clear();
        self.locale_name.push_str(locale);
        self.locale_utf16.clear();
        self.locale_utf16.extend(locale.encode_utf16());
        self.locale_utf16.push(0);

        // SAFETY: locale pointer stays valid until next `set_locale` call.
        self.number_substitution = unsafe {
            self.factory
                .CreateNumberSubstitution(
                    DWRITE_NUMBER_SUBSTITUTION_METHOD_CONTEXTUAL,
                    PCWSTR(self.locale_utf16.as_ptr()),
                    false,
                )
                .ok()
        };
    }

    fn reset_used_lengths(&mut self) {
        self.runs_used = 0;
        self.run_spans_used = 0;
        self.cells_used = 0;
        self.cluster_used = 0;
        self.glyph_used = 0;
    }

    fn push_run_with_spans(
        &mut self,
        run: TextRun,
        cell_start: u32,
        glyph_start: u32,
        cluster_start: u32,
    ) {
        // SAFETY: caller reserves output capacity before recording spans.
        unsafe {
            *self.output.runs_mut_ptr().add(self.runs_used) = run;
            self.runs_used += 1;
            *self
                .output
                .run_cell_spans_mut_ptr()
                .add(self.run_spans_used) = RunSpan::new(cell_start, self.cells_used as u32);
            *self
                .output
                .run_glyph_spans_mut_ptr()
                .add(self.run_spans_used) = RunSpan::new(glyph_start, self.glyph_used as u32);
            *self
                .output
                .run_cluster_spans_mut_ptr()
                .add(self.run_spans_used) = RunSpan::new(cluster_start, self.cluster_used as u32);
            self.run_spans_used += 1;
        }
    }

    #[allow(dead_code)]
    pub(crate) fn reset_arenas(&mut self, mode: ArenaResetMode) {
        self.output.reset(mode);
        self.scratch.reset(mode);
        self.reset_used_lengths();
        self.script_runs_used = 0;
        self.bidi_runs_used = 0;
    }

    fn current_shape(&self) -> ShapedCells<'_> {
        // SAFETY: `*_used` are bounded by output capacities and written prefixes only.
        unsafe {
            ShapedCells {
                runs: std::slice::from_raw_parts(self.output.runs_ptr(), self.runs_used),
                run_cell_spans: std::slice::from_raw_parts(
                    self.output.run_cell_spans_ptr(),
                    self.run_spans_used,
                ),
                run_glyph_spans: std::slice::from_raw_parts(
                    self.output.run_glyph_spans_ptr(),
                    self.run_spans_used,
                ),
                run_cluster_spans: std::slice::from_raw_parts(
                    self.output.run_cluster_spans_ptr(),
                    self.run_spans_used,
                ),
                cells: std::slice::from_raw_parts(self.output.cells_ptr(), self.cells_used),
                cluster_map: std::slice::from_raw_parts(
                    self.output.cluster_map_ptr(),
                    self.cluster_used,
                ),
                glyph_advances: std::slice::from_raw_parts(
                    self.output.glyph_advances_ptr(),
                    self.glyph_used,
                ),
                glyph_offsets: std::slice::from_raw_parts(
                    self.output.glyph_offsets_ptr(),
                    self.glyph_used,
                ),
            }
        }
    }
}

#[inline]
fn projected_glyphs(text_len: usize) -> usize {
    ((text_len * 3) / 2 + 16).max(16)
}

#[inline]
fn cluster_to_cell_x(codepoint: Codepoint) -> u16 {
    debug_assert!(codepoint.cluster <= u16::MAX as u32);
    codepoint.cluster as u16
}

#[implement(IDWriteTextAnalysisSource)]
struct BorrowedTextAnalysisSource {
    text_ptr: *const u16,
    text_len: u32,
    locale_ptr: *const u16,
    number_substitution: Option<IDWriteNumberSubstitution>,
}

impl BorrowedTextAnalysisSource {
    fn new(
        locale: &[u16],
        text: &[u16],
        number_substitution: Option<IDWriteNumberSubstitution>,
    ) -> Self {
        Self {
            text_ptr: text.as_ptr(),
            text_len: text.len() as u32,
            locale_ptr: locale.as_ptr(),
            number_substitution,
        }
    }
}

#[allow(non_snake_case)]
impl IDWriteTextAnalysisSource_Impl for BorrowedTextAnalysisSource_Impl {
    fn GetTextAtPosition(
        &self,
        textposition: u32,
        textstring: *mut *mut u16,
        textlength: *mut u32,
    ) -> windows::core::Result<()> {
        let pos = textposition.min(self.text_len) as usize;
        // SAFETY: `pos` is clamped to `text_len`.
        unsafe {
            *textstring = self.text_ptr.add(pos) as *mut u16;
            *textlength = self.text_len - pos as u32;
        }
        Ok(())
    }

    fn GetTextBeforePosition(
        &self,
        textposition: u32,
        textstring: *mut *mut u16,
        textlength: *mut u32,
    ) -> windows::core::Result<()> {
        let pos = textposition.min(self.text_len);
        // SAFETY: pointer/length pair references a valid prefix.
        unsafe {
            *textstring = self.text_ptr as *mut u16;
            *textlength = pos;
        }
        Ok(())
    }

    fn GetParagraphReadingDirection(&self) -> DWRITE_READING_DIRECTION {
        DWRITE_READING_DIRECTION_LEFT_TO_RIGHT
    }

    fn GetLocaleName(
        &self,
        textposition: u32,
        textlength: *mut u32,
        localename: *mut *mut u16,
    ) -> windows::core::Result<()> {
        // SAFETY: locale buffer is analyzer-owned and null-terminated.
        unsafe {
            *textlength = self.text_len - textposition.min(self.text_len);
            *localename = self.locale_ptr as *mut u16;
        }
        Ok(())
    }

    fn GetNumberSubstitution(
        &self,
        textposition: u32,
        textlength: *mut u32,
        numbersubstitution: OutRef<IDWriteNumberSubstitution>,
    ) -> windows::core::Result<()> {
        // SAFETY: we expose number substitution for the remaining text span.
        unsafe {
            *textlength = if self.number_substitution.is_some() {
                self.text_len - textposition.min(self.text_len)
            } else {
                0
            };
        }
        let _ = numbersubstitution.write(self.number_substitution.clone());
        Ok(())
    }
}

#[implement(IDWriteTextAnalysisSink)]
/// DWrite script-analysis sink that writes directly into scratch arena memory.
struct ScriptAnalysisSink {
    runs_ptr: UnsafeCell<*mut ScriptRun>,
    runs_cap: usize,
    used_ptr: UnsafeCell<*mut usize>,
}

impl ScriptAnalysisSink {
    fn new(runs: *mut ScriptRun, runs_cap: usize, used: &mut usize) -> Self {
        Self {
            runs_ptr: UnsafeCell::new(runs),
            runs_cap,
            used_ptr: UnsafeCell::new(used as *mut usize),
        }
    }

    #[inline]
    unsafe fn push_script(&self, run: ScriptRun) {
        // SAFETY: pointers are valid for AnalyzeScript call lifetime and writes
        // are bounded by `runs_cap`.
        unsafe {
            let used = &mut **self.used_ptr.get();
            debug_assert!(*used <= self.runs_cap);
            if *used < self.runs_cap {
                (*self.runs_ptr.get()).add(*used).write(run);
                *used += 1;
            }
        }
    }
}

#[allow(non_snake_case)]
impl IDWriteTextAnalysisSink_Impl for ScriptAnalysisSink_Impl {
    fn SetScriptAnalysis(
        &self,
        textposition: u32,
        textlength: u32,
        scriptanalysis: *const DWRITE_SCRIPT_ANALYSIS,
    ) -> windows::core::Result<()> {
        let analysis = if scriptanalysis.is_null() {
            DWRITE_SCRIPT_ANALYSIS {
                script: 0,
                shapes: DWRITE_SCRIPT_SHAPES(0),
            }
        } else {
            // SAFETY: DWrite provides a valid pointer for callback lifetime.
            unsafe { *scriptanalysis }
        };
        // SAFETY: callback is synchronous and buffers outlive callback.
        unsafe {
            self.push_script(ScriptRun {
                text_position: textposition,
                text_length: textlength,
                analysis,
            });
        }
        Ok(())
    }

    fn SetLineBreakpoints(
        &self,
        _textposition: u32,
        _textlength: u32,
        _linebreakpoints: *const DWRITE_LINE_BREAKPOINT,
    ) -> windows::core::Result<()> {
        Ok(())
    }

    fn SetBidiLevel(
        &self,
        _textposition: u32,
        _textlength: u32,
        _explicitlevel: u8,
        _resolvedlevel: u8,
    ) -> windows::core::Result<()> {
        Ok(())
    }

    fn SetNumberSubstitution(
        &self,
        _textposition: u32,
        _textlength: u32,
        _numbersubstitution: Ref<IDWriteNumberSubstitution>,
    ) -> windows::core::Result<()> {
        Ok(())
    }
}

#[implement(IDWriteTextAnalysisSink)]
/// DWrite bidi-analysis sink that writes resolved bidi levels into scratch arena.
struct BidiAnalysisSink {
    runs_ptr: UnsafeCell<*mut BidiRun>,
    runs_cap: usize,
    used_ptr: UnsafeCell<*mut usize>,
}

impl BidiAnalysisSink {
    fn new(runs: *mut BidiRun, runs_cap: usize, used: &mut usize) -> Self {
        Self {
            runs_ptr: UnsafeCell::new(runs),
            runs_cap,
            used_ptr: UnsafeCell::new(used as *mut usize),
        }
    }

    #[inline]
    unsafe fn push_bidi(&self, run: BidiRun) {
        // SAFETY: pointers are valid for analysis call lifetime and writes
        // are bounded by `runs_cap`.
        unsafe {
            let used = &mut **self.used_ptr.get();
            debug_assert!(*used <= self.runs_cap);
            if *used < self.runs_cap {
                (*self.runs_ptr.get()).add(*used).write(run);
                *used += 1;
            }
        }
    }
}

#[allow(non_snake_case)]
impl IDWriteTextAnalysisSink_Impl for BidiAnalysisSink_Impl {
    fn SetScriptAnalysis(
        &self,
        _textposition: u32,
        _textlength: u32,
        _scriptanalysis: *const DWRITE_SCRIPT_ANALYSIS,
    ) -> windows::core::Result<()> {
        Ok(())
    }

    fn SetLineBreakpoints(
        &self,
        _textposition: u32,
        _textlength: u32,
        _linebreakpoints: *const DWRITE_LINE_BREAKPOINT,
    ) -> windows::core::Result<()> {
        Ok(())
    }

    fn SetBidiLevel(
        &self,
        textposition: u32,
        textlength: u32,
        _explicitlevel: u8,
        resolvedlevel: u8,
    ) -> windows::core::Result<()> {
        // SAFETY: callback is synchronous and buffers outlive callback.
        unsafe {
            self.push_bidi(BidiRun {
                text_position: textposition,
                text_length: textlength,
                resolved_level: resolvedlevel,
            });
        }
        Ok(())
    }

    fn SetNumberSubstitution(
        &self,
        _textposition: u32,
        _textlength: u32,
        _numbersubstitution: Ref<IDWriteNumberSubstitution>,
    ) -> windows::core::Result<()> {
        Ok(())
    }
}
