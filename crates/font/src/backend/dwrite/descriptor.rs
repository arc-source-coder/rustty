use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_AXIS_TAG, DWRITE_FONT_AXIS_TAG_ITALIC, DWRITE_FONT_AXIS_TAG_SLANT,
    DWRITE_FONT_AXIS_TAG_WEIGHT, DWRITE_FONT_AXIS_VALUE,
};

use crate::types::FontDescriptor;

// TODO: Doc comments
pub struct Descriptor {
    pub axes: Vec<DWRITE_FONT_AXIS_VALUE>,
}

impl From<&FontDescriptor> for Descriptor {
    #[inline]
    fn from(descriptor: &FontDescriptor) -> Self {
        let required_len = descriptor.variations.len() + 3;
        let mut axes: Vec<DWRITE_FONT_AXIS_VALUE> = Vec::with_capacity(required_len);

        // Derive conventional style axes only when no exact face name was requested.
        if descriptor.style.is_none() {
            let bold_value = if descriptor.bold { 700.0 } else { 400.0 };
            axes.set_axis(DWRITE_FONT_AXIS_TAG_WEIGHT, bold_value);

            let italic_value = if descriptor.italic { 1.0 } else { 0.0 };
            axes.set_axis(DWRITE_FONT_AXIS_TAG_ITALIC, italic_value);
            axes.set_axis(DWRITE_FONT_AXIS_TAG_SLANT, 0.0);
        }

        for variation in descriptor.variations.iter() {
            axes.set_axis(
                DWRITE_FONT_AXIS_TAG(variation.tag.packed()),
                variation.value.get() as f32,
            );
        }

        Self { axes }
    }
}

trait SetAxis {
    fn set_axis(&mut self, tag: DWRITE_FONT_AXIS_TAG, value: f32);
}

impl SetAxis for Vec<DWRITE_FONT_AXIS_VALUE> {
    #[inline]
    fn set_axis(&mut self, tag: DWRITE_FONT_AXIS_TAG, value: f32) {
        match self.iter_mut().find(|axis| axis.axisTag == tag) {
            Some(axis) => axis.value = value,
            None => self.push(DWRITE_FONT_AXIS_VALUE {
                axisTag: tag,
                value,
            }),
        }
    }
}
