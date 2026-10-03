use std::ffi::c_uint;

/// An operation reported failure through the HarfBuzz C API.
///
/// HarfBuzz generally reports only success or failure, so this
/// error does not contain a more detailed diagnostic.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HarfbuzzError;

impl std::error::Error for HarfbuzzError {}

impl std::fmt::Display for HarfbuzzError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("harfbuzz failed")
    }
}

pub type hb_bool_t = i32;
pub type hb_codepoint_t = u32;

/// The kind of data currently stored in a HarfBuzz buffer.
#[repr(i32)]
#[derive(Clone, Copy)]
pub enum ContentType {
    /// The buffer is empty.
    Invalid = 0,
    /// The buffer contains Unicode code points awaiting shaping.
    Unicode = 1,
    /// The buffer contains shaped glyph identifiers.
    Glyphs = 2,
}

/// Controls how HarfBuzz groups input characters into output clusters.
#[repr(i32)]
#[derive(Clone, Copy)]
pub enum ClusterLevel {
    MonotoneGraphemes = 0,
    MonotoneCharacters = 1,
    Characters = 2,
    Graphemes = 3,
}

/// The direction in which text is shaped and laid out.
#[repr(i32)]
#[derive(Clone, Copy)]
pub enum Direction {
    /// No direction has been specified.
    Invalid = 0,

    /// Horizontal text progressing from left to right.
    Ltr = 4,

    /// Horizontal text progressing from right to left.
    Rtl = 5,

    /// Vertical text progressing from top to bottom.
    Ttb = 6,

    /// Vertical text progressing from bottom to top.
    Btt = 7,
}

pub type hb_tag_t = u32;

const HB_FEATURE_GLOBAL_START: c_uint = 0;
const HB_FEATURE_GLOBAL_END: c_uint = u32::MAX;

/// An OpenType feature applied while shaping.
///
/// Rust mirror of `hb_feature_t`
#[repr(C)]
pub struct HbFeature {
    /// Big-endian encoded tag
    tag: hb_tag_t,
    value: u32,
    start: c_uint,
    end: c_uint,
}

impl HbFeature {
    #[inline]
    pub const fn new(tag: [u8; 4], value: u32) -> Self {
        Self {
            tag: u32::from_be_bytes(tag),
            value,
            start: HB_FEATURE_GLOBAL_START,
            end: HB_FEATURE_GLOBAL_END,
        }
    }
}
