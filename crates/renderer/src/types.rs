use config::Config;
use ghostty::font::Feature;

/// Terminal grid dimensions in cells.
/// Ghostty: `renderer.GridSize`
#[derive(Clone, Copy, Default, Eq, PartialEq)]
pub struct GridSize {
    pub rows: u16,
    pub columns: u16,
}

pub struct DerivedConfig {
    pub features: Vec<Feature>,
    pub background_opacity: f32,
}

impl From<&Config> for DerivedConfig {
    fn from(config: &Config) -> Self {
        DerivedConfig {
            features: config.font_features.to_vec(),
            background_opacity: config.background_opacity,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FrameOutcome {
    Presented,
    Skipped,
}

/// Vertical target-pixel bounds for a presented region.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DirtyRect {
    pub top: i32,
    pub bottom: i32,
}

/// Tracks frame-local presentation damage and retained rendered bounds by row.
#[derive(Default)]
pub struct DamageTracker {
    cell_height: u32,
    damage: Option<DirtyRect>,
    row_bounds: Vec<Option<DirtyRect>>,
    active: Option<ActiveRow>,
}

struct ActiveRow {
    row: usize,
    bounds: DirtyRect,
}

impl DamageTracker {
    #[inline]
    pub fn begin_frame(&mut self, cell_height: u32) {
        self.cell_height = cell_height;
        self.damage = None;
        self.active = None;
    }

    #[inline]
    pub fn resize(&mut self, rows: usize) {
        self.row_bounds.clear();
        self.row_bounds.resize(rows, None);
    }

    #[inline]
    pub(crate) fn begin_row(&mut self, row: usize) -> &mut DirtyRect {
        assert!(self.active.is_none());
        assert!(row < self.row_bounds.len());

        let top = (row as i32).saturating_mul(self.cell_height as i32);
        let bottom = top.saturating_add(self.cell_height as i32);

        let active = ActiveRow {
            row,
            bounds: DirtyRect { top, bottom },
        };
        &mut self.active.insert(active).bounds
    }

    #[inline]
    pub fn finish_row(&mut self) {
        let active = self.active.take().expect("a row must be active");
        let mut damage = active.bounds;
        if let Some(previous) = self.row_bounds[active.row].replace(active.bounds) {
            damage.include(previous);
        }

        self.include(damage);
    }

    #[inline]
    pub(crate) fn include(&mut self, damage: impl Into<DirtyRect>) {
        let damage = damage.into();
        if damage.top >= damage.bottom {
            return;
        }
        match self.damage.as_mut() {
            Some(current) => current.include(damage),
            None => self.damage = Some(damage),
        }
    }

    #[inline]
    pub const fn rect(&self) -> Option<DirtyRect> {
        self.damage
    }
}

impl DirtyRect {
    #[inline]
    pub(crate) fn include(&mut self, bounds: impl Into<Self>) {
        let bounds = bounds.into();
        if bounds.top >= bounds.bottom {
            return;
        }
        self.top = self.top.min(bounds.top);
        self.bottom = self.bottom.max(bounds.bottom);
    }
}

impl From<&QuadInstance> for DirtyRect {
    #[inline]
    fn from(instance: &QuadInstance) -> Self {
        let top = i32::from(instance.position[1]);
        Self {
            top,
            bottom: top.saturating_add(i32::from(instance.data.size()[1])),
        }
    }
}

#[repr(u16)]
#[derive(Clone, Copy)]
pub enum ShadingType {
    Background = 0,
    Solid = 1,
    GrayscaleText = 2,
    ColorText = 3,
}

/// A type packing dimensions and shading type
/// Each u16 packs one shading bit and a dimension.
/// Bits 0-14 contain the dimension value, allowing a maximum size of 32,767.
/// This is safe since the maximum size of a D3D11 Texture is 16,384.
#[repr(transparent)]
#[derive(Clone, Copy)]
pub struct Data([u16; 2]);

impl Data {
    const SIZE_MASK: u16 = 0x7fff;

    #[inline]
    pub const fn new(mut size: [u16; 2], shading: ShadingType) -> Self {
        size[0] |= (shading as u16 & 1) << 15;
        size[1] |= ((shading as u16 >> 1) & 1) << 15;

        Data(size)
    }

    #[inline]
    pub const fn size(self) -> [u16; 2] {
        [self.0[0] & Self::SIZE_MASK, self.0[1] & Self::SIZE_MASK]
    }
}

// TODO add doc comments
#[repr(C)]
#[derive(Clone, Copy)]
pub struct QuadInstance {
    pub position: [i16; 2],
    /// Packed type containing shading type and dimensions
    pub data: Data,
    pub texcoord: [u16; 2],
    pub color: [u8; 4],
}

const _: () = assert!(size_of::<QuadInstance>() == 16);
const _: () = assert!(align_of::<QuadInstance>() == 2);

impl QuadInstance {
    #[inline]
    pub const fn background_rect(size: [u16; 2]) -> Self {
        Self {
            data: Data::new(size, ShadingType::Background),
            position: [0, 0],
            texcoord: [0, 0],
            color: [0; 4],
        }
    }

    #[inline]
    pub const fn solid_rect(origin: [i16; 2], size: [u16; 2], color: [u8; 4]) -> Self {
        Self {
            data: Data::new(size, ShadingType::Solid),
            position: origin,
            texcoord: [0, 0],
            color,
        }
    }

    #[inline]
    pub const fn glyph_rect(position: [i16; 2], size: [u16; 2], color: [u8; 4]) -> Self {
        Self {
            data: Data::new(size, ShadingType::GrayscaleText),
            position,
            texcoord: [0, 0],
            color,
        }
    }

    #[inline]
    pub const fn color_glyph_rect(origin: [i16; 2], size: [u16; 2]) -> Self {
        Self {
            data: Data::new(size, ShadingType::ColorText),
            position: origin,
            texcoord: [0, 0],
            color: [0, 0, 0, 255],
        }
    }

    #[inline]
    pub const fn set_texcoord(&mut self, x: u16, y: u16) {
        self.texcoord = [x, y];
    }
}
