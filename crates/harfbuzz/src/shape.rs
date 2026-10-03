use std::ffi::c_char;
use std::ptr::NonNull;

use crate::buffer::{HbBuffer, hb_buffer_t};
use crate::font::{HbFont, hb_font_t};
use crate::types::{HbFeature, hb_bool_t};

use crate::types::HarfbuzzError;

const SHAPER_LIST: [*const c_char; 2] = [c"ot".as_ptr(), std::ptr::null()];

/// Shapes the Unicode contents of `buffer` with `font` using the OpenType shaper.
///
/// The buffer is transformed in place from Unicode code points into glyph
/// information and positions. `features` are applied in slice order, with
/// later overlapping settings taking precedence.
///
/// # Errors
///
/// Returns an error if the OpenType shaper cannot shape the buffer.
#[inline]
pub fn shape(
    font: &HbFont,
    buffer: &mut HbBuffer,
    features: &[HbFeature],
) -> Result<(), HarfbuzzError> {
    let result = unsafe {
        hb_shape_full(
            font.handle,
            buffer.handle.as_mut(),
            features.as_ptr(),
            features.len() as u32,
            SHAPER_LIST.as_ptr(),
        )
    };
    if result == 0 {
        return Err(HarfbuzzError);
    }
    Ok(())
}

unsafe extern "C" {
    fn hb_shape_full(
        font: NonNull<hb_font_t>,
        buffer: &mut hb_buffer_t,
        features: *const HbFeature,
        num_features: u32,
        shaper_list: *const *const c_char,
    ) -> hb_bool_t;
}
