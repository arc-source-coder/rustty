use std::mem::MaybeUninit;
use std::ptr::NonNull;

use crate::types::{ClusterLevel, ContentType, Direction, HarfbuzzError};
use crate::types::{hb_bool_t, hb_codepoint_t};

#[repr(C)]
pub struct hb_buffer_t {
    _private: [u8; 0],
}

/// An owning HarfBuzz shaping buffer.
pub struct HbBuffer {
    pub(crate) handle: NonNull<hb_buffer_t>,
}

// TODO: Doc comments
impl HbBuffer {
    #[inline]
    pub fn new() -> Result<Self, HarfbuzzError> {
        let ptr = unsafe { hb_buffer_create() };

        if unsafe { hb_buffer_allocation_successful(ptr) } == 0 {
            unsafe { hb_buffer_destroy(ptr.as_mut_unchecked()) };
            return Err(HarfbuzzError);
        }

        // Harfbuzz guarantees that `hb_buffer_create` never returns NULL.
        // If `hb_buffer_allocation_successful` succeeded, then the buffer is valid.
        let handle = NonNull::new(ptr).unwrap();

        Ok(Self { handle })
    }

    #[inline]
    pub fn reserve(&mut self, capacity: u32) -> Result<(), HarfbuzzError> {
        let result = unsafe { hb_buffer_pre_allocate(self.handle.as_mut(), capacity) };
        if result == 0 {
            return Err(HarfbuzzError);
        }
        Ok(())
    }

    /// Resets the buffer to the same state as a newly created buffer.
    #[inline]
    pub fn clear(&mut self) {
        unsafe { hb_buffer_reset(self.handle.as_mut()) };
    }

    #[inline]
    pub fn len(&self) -> u32 {
        unsafe { hb_buffer_get_length(self.handle.as_ref()) }
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[inline]
    pub fn add_codepoints(&mut self, codepoints: &[u32]) {
        let (ptr, len) = (codepoints.as_ptr(), codepoints.len() as i32);
        unsafe { hb_buffer_add_codepoints(self.handle.as_mut(), ptr, len, 0, -1) };
    }

    #[inline]
    pub fn get_glyph_infos(&self) -> &[GlyphInfo] {
        let mut length: u32 = 0;
        let ptr = unsafe { hb_buffer_get_glyph_infos(self.handle, &mut length) };

        if ptr.is_null() || length == 0 {
            return &[];
        }

        unsafe { std::slice::from_raw_parts(ptr, length as usize) }
    }

    #[inline]
    pub fn get_glyph_positions(&self) -> &[GlyphPosition] {
        let mut length: u32 = 0;
        let ptr = unsafe { hb_buffer_get_glyph_positions(self.handle, &mut length) };

        if ptr.is_null() || length == 0 {
            return &[];
        }

        unsafe { std::slice::from_raw_parts(ptr, length as usize) }
    }

    #[inline]
    pub fn set_content_type(&mut self, content_type: ContentType) {
        unsafe { hb_buffer_set_content_type(self.handle.as_mut(), content_type as i32) };
    }

    #[inline]
    pub fn set_cluster_level(&mut self, level: ClusterLevel) {
        unsafe { hb_buffer_set_cluster_level(self.handle.as_mut(), level as i32) };
    }

    #[inline]
    pub fn set_direction(&mut self, direction: Direction) {
        unsafe { hb_buffer_set_direction(self.handle.as_mut(), direction as i32) };
    }

    #[inline]
    pub fn guess_segment_properties(&mut self) {
        unsafe { hb_buffer_guess_segment_properties(self.handle.as_mut()) };
    }
}

impl Drop for HbBuffer {
    #[inline]
    fn drop(&mut self) {
        unsafe { hb_buffer_destroy(self.handle.as_mut()) };
    }
}
/// Information connecting one buffer item to its input text.
///
/// Before shaping, an item represents a Unicode code point. After shaping, it
/// represents a glyph and identifies the input cluster that produced it.
///
/// Rust mirror of `hb_glyph_info_t`
#[repr(C)]
pub struct GlyphInfo {
    // TODO: Doc comments
    pub codepoint: u32,

    _mask: MaybeUninit<u32>,

    pub cluster: u32,

    _var1: MaybeUninit<u32>,
    _var2: MaybeUninit<u32>,
}

/// Rust mirror of `hb_glyph_position_t`
#[repr(C)]
pub struct GlyphPosition {
    // TODO doc comments
    pub x_advance: i32,
    pub y_advance: i32,
    pub x_offset: i32,
    pub y_offset: i32,

    _var: MaybeUninit<u32>,
}

unsafe extern "C" {
    fn hb_buffer_create() -> *mut hb_buffer_t;
    fn hb_buffer_allocation_successful(buffer: *mut hb_buffer_t) -> hb_bool_t;
    fn hb_buffer_destroy(buffer: &mut hb_buffer_t);
    fn hb_buffer_pre_allocate(buffer: &mut hb_buffer_t, size: u32) -> hb_bool_t;
    fn hb_buffer_reset(buffer: &mut hb_buffer_t);
    fn hb_buffer_get_length(buffer: &hb_buffer_t) -> u32;
    fn hb_buffer_add_codepoints(
        buffer: &mut hb_buffer_t,
        text: *const hb_codepoint_t,
        text_length: i32,
        item_offset: u32,
        item_length: i32,
    );

    fn hb_buffer_set_content_type(buffer: &mut hb_buffer_t, content_type: i32);
    fn hb_buffer_set_cluster_level(buffer: &mut hb_buffer_t, level: i32);
    fn hb_buffer_set_direction(buffer: &mut hb_buffer_t, direction: i32);
    fn hb_buffer_guess_segment_properties(buffer: &mut hb_buffer_t);

    fn hb_buffer_get_glyph_infos(buffer: NonNull<hb_buffer_t>, length: &mut u32) -> *mut GlyphInfo;
    fn hb_buffer_get_glyph_positions(
        buffer: NonNull<hb_buffer_t>,
        length: &mut u32,
    ) -> *mut GlyphPosition;
}
