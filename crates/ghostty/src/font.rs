use std::rc::Rc;

#[repr(C)]
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct Feature {
    pub tag: [u8; 4],
    pub value: u32,
}

impl Feature {
    // Parse features from a contiguous string of features
    #[inline]
    pub fn parse(input: &str) -> Rc<[Feature]> {
        let mut out_len = 0;
        let out_ptr =
            unsafe { ghostty_font_parse_features(input.as_ptr(), input.len(), &mut out_len) };
        if out_ptr.is_null() || out_len == 0 {
            return Rc::from([]);
        }
        let features = unsafe { std::slice::from_raw_parts(out_ptr, out_len) };
        let result = Rc::from(features);
        unsafe { ghostty_font_bytes_free(out_ptr, out_len) };

        result
    }
}

unsafe extern "C" {
    fn ghostty_font_parse_features(
        bytes: *const u8,
        len: usize,
        out_len: &mut usize,
    ) -> *const Feature;
    fn ghostty_font_bytes_free(bytes: *const Feature, len: usize);
}
