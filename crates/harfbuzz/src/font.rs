use std::ptr::NonNull;

use crate::types::HarfbuzzError;
use windows::Win32::Graphics::DirectWrite::{IDWriteFontFace, IDWriteFontFace5};
use windows::core::Interface as _;

#[repr(C)]
pub struct hb_font_t {
    _private: [u8; 0],
}

/// An owning HarfBuzz font backed by a DirectWrite font face.
///
/// Configuration methods require exclusive access. Once configured, a font may
/// be shared between threads for concurrent shaping because HarfBuzz is built
/// with multithreading enabled.
///
/// The underlying DirectWrite face is retained for the lifetime of the font.
pub struct HbFont {
    pub(crate) handle: NonNull<hb_font_t>,
}

// Safety: Harfbuzz has internal thread synchronization and is compiled without HB_NO_MT
unsafe impl Send for HbFont {}
unsafe impl Sync for HbFont {}

impl HbFont {
    #[inline]
    pub fn from(face: &IDWriteFontFace5) -> Result<Self, HarfbuzzError> {
        let font = unsafe { hb_directwrite_font_create(face.as_raw().cast()) };
        let empty = unsafe { hb_font_get_empty() };

        let allocation_failed = font == empty;

        unsafe { hb_font_destroy(empty.as_mut_unchecked()) };

        if allocation_failed {
            unsafe { hb_font_destroy(font.as_mut_unchecked()) };
            return Err(HarfbuzzError);
        }

        // Harfbuzz guarantees this constructor never returns NULL.
        let handle: NonNull<hb_font_t> = NonNull::new(font).unwrap();

        Ok(Self { handle })
    }

    /// Sets the horizontal and vertical scale used for shaping positions.
    #[inline]
    pub fn set_scale(&mut self, x_scale: i32, y_scale: i32) {
        unsafe { hb_font_set_scale(self.handle.as_mut(), x_scale, y_scale) };
    }
}

impl Drop for HbFont {
    #[inline]
    fn drop(&mut self) {
        unsafe { hb_font_destroy(self.handle.as_mut()) };
    }
}

unsafe extern "C" {
    fn hb_directwrite_font_create(dw_face: *mut IDWriteFontFace) -> *mut hb_font_t;
    fn hb_font_get_empty() -> *mut hb_font_t;
    fn hb_font_destroy(font: &mut hb_font_t);

    fn hb_font_set_scale(font: &mut hb_font_t, x_scale: i32, y_scale: i32);
}
