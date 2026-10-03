use anyhow::Result;
use harfbuzz::{ClusterLevel, ContentType, Direction, HarfbuzzError, HbBuffer, HbFeature};

use crate::shaper::run_iterator::RunIteratorHook;
use crate::types::{Cell, FontError, ShapeOptions, TextRun};
use utils::asserts::assert;

/// Harfbuzz shaper with DirectWrite font face integration.
/// Ghostty reference: `font/shaper/harfbuzz.zig`
pub struct Shaper {
    // TODO: Doc comments
    pub(crate) buffer: HbBuffer,
    pub(crate) cells: Vec<Cell>,
    pub(crate) codepoints: Vec<u32>,
    pub(crate) clusters: Vec<u32>,
    pub(crate) features: Vec<HbFeature>,
}

impl Shaper {
    pub fn new(shape_options: ShapeOptions) -> Result<Self, HarfbuzzError> {
        let hb_features: Vec<HbFeature> = shape_options
            .features
            .iter()
            .map(|feature| HbFeature::new(feature.tag, feature.value))
            .collect();

        Ok(Self {
            buffer: HbBuffer::new()?,
            cells: Vec::new(),
            codepoints: Vec::new(),
            clusters: Vec::new(),
            features: hb_features,
        })
    }

    /// Shape the current run using codepoints collected during the most recent run iteration.
    ///
    /// Ghostty: `Shaper.shape(run) -> []const Cell`
    pub fn shape(&mut self, run: &TextRun) -> Result<&[Cell], FontError> {
        if run.font_index.special().is_none() {
            run.grid
                .with_face(run.font_index, |face| -> Result<(), HarfbuzzError> {
                    let hb_font = face.hb_font.as_ref().unwrap();
                    harfbuzz::shape(hb_font, &mut self.buffer, self.features.as_slice())?;
                    Ok(())
                })??;
        }

        if self.buffer.is_empty() {
            return Ok(&[]);
        }

        let infos = self.buffer.get_glyph_infos();
        let positions = self.buffer.get_glyph_positions();

        assert(infos.len() == positions.len());

        let mut run_cluster_offset = 0;
        let mut run_offset_x = 0;
        let mut run_offset_y = 0;

        let mut cell_cluster_offset: u32 = 0;
        let mut cell_offset_x = 0;

        self.cells.clear();

        for (info, position) in infos.iter().zip(positions.iter()) {
            // This is used to index into the codepoints array
            // and get the original cluster
            let index = info.cluster as usize;
            assert(index < self.clusters.len());
            // The cluster is the cell X position.
            let cluster: u32 = self.clusters[index];

            if cluster != cell_cluster_offset {
                let is_after_glyph_from_current_or_next_clusters = cluster <= run_cluster_offset;
                let is_first_codepoint_in_cluster =
                    index == 0 || self.clusters[index - 1] != cluster;

                if is_first_codepoint_in_cluster && !is_after_glyph_from_current_or_next_clusters {
                    cell_cluster_offset = cluster;
                    cell_offset_x = run_offset_x;
                }
            }

            // Round Harfbuzz's 26.6 fixed point units to the nearest whole value.
            let x_offset = run_offset_x - cell_offset_x + ((position.x_offset + 0b100_000) >> 6);
            let y_offset = run_offset_y + ((position.y_offset + 0b100_000) >> 6);

            self.cells.push(Cell {
                x: cell_cluster_offset as u16,
                x_offset: x_offset as i16,
                y_offset: y_offset as i16,
                glyph_index: info.codepoint,
            });

            // Apply the advances and move the pen.
            run_offset_x += (position.x_advance + 0b100_000) >> 6;
            run_offset_y += (position.y_advance + 0b100_000) >> 6;
            run_cluster_offset = run_cluster_offset.max(cluster);
        }

        Ok(self.cells.as_slice())
    }
}

impl RunIteratorHook for Shaper {
    fn prepare(&mut self) {
        self.buffer.clear();
        self.buffer.set_content_type(ContentType::Unicode);
        self.buffer.set_cluster_level(ClusterLevel::Characters);
        // Force LTR direction
        self.buffer.set_direction(Direction::Ltr);

        self.clusters.clear();
        self.codepoints.clear();
    }

    fn add_codepoint(&mut self, codepoint: u32, cluster: u32) {
        self.codepoints.push(codepoint);
        self.clusters.push(cluster);
    }

    fn finalize(&mut self) {
        self.buffer.add_codepoints(self.codepoints.as_slice());
        self.buffer.guess_segment_properties();
    }
}
