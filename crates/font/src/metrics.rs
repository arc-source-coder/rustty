use ghostty::sprite::SpriteMetrics;

/// Pixel metrics that define terminal grid geometry and glyph placement.
///
/// These values are derived from the primary font face at its configured size
/// and DPI. Integer fields describe pixel-aligned grid and sprite geometry.
///
/// Vertical cell positions use screen coordinates (`+Y` downward) unless a
/// field explicitly states otherwise.
#[derive(Clone, Debug, Default)]
pub struct FontMetrics {
    /// Width of one terminal cell in pixels.
    pub cell_width: u32,
    /// Height of one terminal cell in pixels.
    pub cell_height: u32,

    /// Distance in pixels from the bottom of the cell to the text baseline.
    ///
    /// This is the only cell-relative vertical position measured upward from
    /// the bottom. Decoration positions are measured downward from the top.
    pub cell_baseline: u32,

    /// Distance from the top of the cell to the top of the underline stroke.
    pub underline_position: u32,

    /// Underline stroke thickness in pixels.
    pub underline_thickness: u32,

    /// Distance from the top of the cell to the top of the strikethrough stroke.
    pub strikethrough_position: u32,

    /// Strikethrough stroke thickness in pixels.
    pub strikethrough_thickness: u32,

    /// Distance from the top of the cell to the top of the overline stroke.
    ///
    /// This may be negative when the overline extends above the cell.
    pub overline_position: i32,

    /// Overline stroke thickness in pixels.
    pub overline_thickness: u32,

    /// Base stroke thickness used box-drawing and related sprite glyphs.
    pub box_thickness: u32,

    /// Stroke thickness used to rasterize bar, underline, and hollow cursors.
    ///
    /// This defaults to one pixel because it is renderer configuration rather
    /// than a metric supplied by the font.
    pub cursor_thickness: u32,

    /// Height available to cursor sprites in pixels.
    ///
    /// This initially equals `cell_height`, but is stored separately so cursor
    /// height can be adjusted independently from the terminal grid.
    pub cursor_height: u32,

    /// Target height used when constraining icons that may occupy multiple cells.
    pub icon_height: f64,

    /// Target height used when constraining an icon to one cell.
    ///
    /// This is generally smaller than `icon_height`, which keeps wide patched
    /// icons visually balanced when they must fit within a single cell.
    pub icon_height_single: f64,

    /// Unrounded advance width of the primary face's terminal cell, in pixels.
    pub face_width: f64,
    /// Unrounded typographic line height of the primary face, in pixels.
    ///
    /// This equals ascent - descent + line_gap.
    pub face_height: f64,

    /// Offset from the bottom of the cell to the bottom of the unrounded face.
    ///
    /// This records the difference introduced when the face is centered
    /// inside the rounded, and potentially adjusted, cell height.
    pub face_y: f64,
}

impl FontMetrics {
    /// Derives pixel-aligned grid metrics from unrounded face metrics.
    pub fn calculate(face: &FaceMetrics) -> Self {
        let face_width = face.cell_width;
        let face_height = face.line_height();

        let cell_width = face_width.round();
        let cell_height = face_height.round();

        let half_line_gap = face.line_gap / 2.0;
        let face_baseline = half_line_gap - face.descent;
        let cell_baseline = (face_baseline - (cell_height - face_height) / 2.0).round();

        let face_y = cell_baseline - face_baseline;
        let top_to_baseline = cell_height - cell_baseline;

        let underline_thickness = f64::max(face.underline_thickness.ceil(), 1.0);
        let strikethrough_thickness = f64::max(face.strikethrough_thickness.ceil(), 1.0);
        let underline_position = (top_to_baseline - face.underline_position).round();
        let strikethrough_position = (top_to_baseline - face.strikethrough_position).round();

        let icon_height = face_height;
        let icon_height_single = (2.0 * face.cap_height() + face_height) / 3.0;

        Self {
            cell_width: cell_width as u32,
            cell_height: cell_height as u32,
            cell_baseline: cell_baseline as u32,
            underline_position: underline_position as u32,
            underline_thickness: underline_thickness as u32,
            strikethrough_position: strikethrough_position as u32,
            strikethrough_thickness: strikethrough_thickness as u32,
            overline_position: 0,
            overline_thickness: underline_thickness as u32,
            box_thickness: underline_thickness as u32,
            cursor_thickness: 1,
            cursor_height: cell_height as u32,
            icon_height,
            icon_height_single,
            face_width,
            face_height,
            face_y,
        }
    }

    #[inline]
    pub fn sprite_metrics(&self) -> SpriteMetrics {
        SpriteMetrics {
            cell_width: self.cell_width,
            cell_height: self.cell_height,
            cell_baseline: self.cell_baseline,

            underline_position: self.underline_position,
            underline_thickness: self.underline_thickness,

            strikethrough_position: self.strikethrough_position,
            strikethrough_thickness: self.strikethrough_thickness,

            overline_position: self.overline_position,
            overline_thickness: self.overline_thickness,

            box_thickness: self.box_thickness,
            cursor_thickness: self.cursor_thickness,
            cursor_height: self.cursor_height,

            icon_height: self.icon_height,
            icon_height_single: self.icon_height_single,
            face_width: self.face_width,
            face_height: self.face_height,
            face_y: self.face_y,
        }
    }
}

/// Unrounded face measurements in physical pixels at the loaded size and DPI.
/// Vertical positions are relative to the baseline, with positive Y upward.
pub struct FaceMetrics {
    /// Maximum advance width among the face's printable ASCII glyphs.
    pub cell_width: f64,

    /// Typographic ascent above the baseline in pixels.
    pub ascent: f64,
    /// Signed typographic descent in pixels, normally negative.
    pub descent: f64,
    /// Additional line spacing in pixels.
    pub line_gap: f64,

    /// Authored underline position relative to the baseline.
    pub underline_position: f64,
    /// Underline stroke thickness in pixels.
    pub underline_thickness: f64,

    /// Authored strikethrough position relative to the baseline.
    pub strikethrough_position: f64,
    /// Strikethrough stroke thickness in pixels.
    pub strikethrough_thickness: f64,

    /// Authored capital-letter height in pixels; zero means unavailable.
    pub cap_height: f64,
}

impl FaceMetrics {
    /// Returns the unrounded typographic line height.
    #[inline]
    pub fn line_height(&self) -> f64 {
        self.ascent - self.descent + self.line_gap
    }

    /// Returns the authored cap height or estimates it as 75% of the ascent.
    #[inline]
    pub fn cap_height(&self) -> f64 {
        if self.cap_height > 0.0 {
            self.cap_height
        } else {
            0.75 * self.ascent
        }
    }
}
