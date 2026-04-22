use bytemuck::{Pod, Zeroable};

const _: () = {
    assert!(std::mem::size_of::<QuadInstance>() == 20);
    assert!(std::mem::align_of::<QuadInstance>() == 4);
};

#[derive(Default)]
pub(crate) struct RenderBatch {
    pub clear_color: [f32; 4],
    pub instance_count: usize,
    pub dirty_rects: Vec<DirtyRect>,
    pub grid_cols: u16,
    pub grid_rows: u16,
    pub cell_size: [f32; 2],
    pub bg_generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DirtyRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl DirtyRect {
    pub(crate) fn include_vertical_span_of(&mut self, instance: &QuadInstance) {
        let height = i32::from(instance.size[1]);
        if height == 0 {
            return;
        }

        let top = i32::from(instance.pos[1]);
        self.top = self.top.min(top);
        self.bottom = self.bottom.max(top + height);
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct QuadInstance {
    pub shading_type: u16,
    pub rendition_scale: [u8; 2],
    pub pos: [i16; 2],
    pub size: [u16; 2],
    pub texcoord: [u16; 2],
    pub color: u32,
}

impl QuadInstance {
    pub(crate) const SHADING_BACKGROUND: u16 = 0;
    pub(crate) const SHADING_TEXT_GRAYSCALE: u16 = 1;
    pub(crate) const SHADING_TEXT_COLOR: u16 = 2;
    pub(crate) const SHADING_SOLID_LINE: u16 = 8;

    pub(crate) fn background_rect(size: [f32; 2]) -> Self {
        Self {
            shading_type: Self::SHADING_BACKGROUND,
            rendition_scale: [1, 1],
            pos: [0, 0],
            size: [pack_px(size[0]), pack_px(size[1])],
            texcoord: [0, 0],
            color: 0,
        }
    }

    pub(crate) fn solid_rect(origin: [f32; 2], size: [f32; 2], color: u32) -> Self {
        Self {
            shading_type: Self::SHADING_SOLID_LINE,
            rendition_scale: [1, 1],
            pos: [pack_pos(origin[0]), pack_pos(origin[1])],
            size: [pack_px(size[0]), pack_px(size[1])],
            texcoord: [0, 0],
            color,
        }
    }

    pub(crate) fn glyph_rect(origin: [f32; 2], size: [f32; 2], color: u32) -> Self {
        Self {
            shading_type: Self::SHADING_TEXT_GRAYSCALE,
            rendition_scale: [1, 1],
            pos: [pack_pos(origin[0]), pack_pos(origin[1])],
            size: [pack_px(size[0]), pack_px(size[1])],
            texcoord: [0, 0],
            color,
        }
    }

    pub(crate) fn color_glyph_rect(origin: [f32; 2], size: [f32; 2]) -> Self {
        Self {
            shading_type: Self::SHADING_TEXT_COLOR,
            rendition_scale: [1, 1],
            pos: [pack_pos(origin[0]), pack_pos(origin[1])],
            size: [pack_px(size[0]), pack_px(size[1])],
            texcoord: [0, 0],
            color: u32::from_le_bytes([0, 0, 0, 255]),
        }
    }

    pub(crate) fn set_texcoord(&mut self, x: u16, y: u16) {
        self.texcoord = [x, y];
    }
}

fn pack_px(v: f32) -> u16 {
    if !v.is_finite() {
        return 0;
    }
    let rounded = v.round().clamp(0.0, u16::MAX as f32);
    rounded as u16
}

fn pack_pos(v: f32) -> i16 {
    if !v.is_finite() {
        return 0;
    }
    let rounded = v.round().clamp(i16::MIN as f32, i16::MAX as f32);
    rounded as i16
}
