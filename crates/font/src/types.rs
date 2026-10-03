use config::font::Variation;
use ghostty::font::Feature;

use harfbuzz::HarfbuzzError;
use thiserror::Error;
use utils::asserts::unreachable;
use utils::floats::NotNan;

use crate::shared_grid::SharedGrid;

use std::hash::{Hash, Hasher};
use std::rc::Rc;

#[derive(Clone, Copy, Debug, Error)]
pub enum FontError {
    #[error("invalid font index for collection")]
    InvalidIndex,

    #[error("invalid unicode codepoint")]
    InvalidCodepoint,

    #[error("font face has not been initialized")]
    FaceNotInitialized,

    #[error("font has invalid metrics")]
    InvalidMetrics,

    #[error("the platform font backend failed")]
    Backend,

    #[error("harfbuzz failed")]
    Harfbuzz,
}

impl From<HarfbuzzError> for FontError {
    #[inline]
    fn from(_: HarfbuzzError) -> Self {
        FontError::Harfbuzz
    }
}

impl From<windows::core::Error> for FontError {
    #[inline]
    fn from(_: windows::core::Error) -> Self {
        FontError::Backend
    }
}

/// A font matching request. An exact style name suppresses the conventional
/// bold/italic axis defaults; explicit variations override any derived axes.
#[derive(Eq, PartialEq)]
pub struct FontDescriptor {
    pub family: Rc<str>,
    /// Exact face/style name within the family, when specified.
    pub style: Option<Rc<str>>,
    /// Requested point size. DirectWrite discovery does not use this field;
    /// the collection's `FontSize` determines the loaded face size.
    pub size: NotNan<f32>,
    pub bold: bool,
    pub italic: bool,
    pub variations: Rc<[Variation]>,
}

impl Hash for FontDescriptor {
    fn hash<H: Hasher>(&self, hasher: &mut H) {
        self.family.hash(hasher);
        if let Some(style) = &self.style {
            style.hash(hasher);
        }
        hasher.write_u32(self.size.get().to_bits());
        self.bold.hash(hasher);
        self.italic.hash(hasher);

        self.variations.len().hash(hasher);
        for variation in self.variations.iter() {
            variation.hash(hasher);
        }
    }
}

/// Presentation preference.
/// Ghostty reference: `font.Presentation` in `font/main.zig`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Presentation {
    Text,
    Emoji,
}

/// How strictly font resolution should match text or emoji presentation.
#[derive(Clone, Copy, Eq, PartialEq)]
pub enum PresentationMode {
    /// Require matching presentation for configured and fallback faces.
    Explicit(Presentation),
    /// Require matching presentation only for fallback faces, allowing
    /// explicitly configured fonts to override the Unicode default.
    Default(Presentation),
    /// Require glyph coverage without checking presentation.
    Any,
}

#[derive(Eq, PartialEq, Default, Hash, Clone, Copy)]
pub struct FontSize {
    pub points: NotNan<f32>,
    pub x_dpi: u16,
    pub y_dpi: u16,
}

impl FontSize {
    pub fn pixels(self) -> f32 {
        // 1 point = 1/72 inch
        (self.points.get() * f32::from(self.y_dpi)) / 72.0
    }
}

pub enum SpecialFont {
    Sprite,
}

/// Collection-local font face identifier, or the reserved sprite identifier.
///
/// The collection assigns dense real-face indices 0 through 8,190 per style.
/// Index 8,191 is reserved for sprites, which do not refer to a loaded face.
/// Indices from different collections are not interchangeable.
///
/// A terminal typically uses <10 font faces total (regular, bold, italic,
/// bold-italic, plus a handful of fallback fonts).
///
/// ## Bit layout
///
///   `[15:13]` style variant (3 bits, matches `FontStyle` discriminant)
///   `[12:0]`  face index or sprite sentinel (13 bits)
///
/// Ghostty reference: `zig/ghostty/src/font/Collection.zig` — `Collection.Index`
#[derive(Clone, Copy, Eq, PartialEq, Hash, Default)]
#[repr(transparent)]
pub struct FontIndex(u16);

impl FontIndex {
    const INDEX_MASK: u16 = (1 << 13) - 1; // 8191
    const SPRITE_INDEX: u16 = Self::INDEX_MASK;

    pub const DEFAULT: FontIndex = Self::new(FontStyle::Regular, 0);
    pub const SPRITE: Self = Self(((FontStyle::Regular as u16) << 13) | Self::SPRITE_INDEX);

    /// Inclusive maximum real-face index per style variant (8,191 faces).
    pub const MAX_FACES_PER_STYLE: u16 = Self::SPRITE_INDEX - 1; // 8190

    /// Construct from a style variant and a dense face index.
    #[inline]
    pub const fn new(style: FontStyle, idx: u16) -> Self {
        assert!(idx <= Self::MAX_FACES_PER_STYLE);
        Self(((style as u16) << 13) | idx)
    }

    /// The raw `u16` value — suitable for packing into a `GlyphKey`.
    #[inline]
    pub const fn to_u16(self) -> u16 {
        self.0
    }

    #[inline]
    pub const fn special(self) -> Option<SpecialFont> {
        if self.index() == Self::SPRITE_INDEX {
            return Some(SpecialFont::Sprite);
        }
        None
    }

    /// Extract the style variant.
    #[inline]
    pub const fn style(self) -> FontStyle {
        match self.0 >> 13 {
            0 => FontStyle::Regular,
            1 => FontStyle::Bold,
            2 => FontStyle::Italic,
            3 => FontStyle::BoldItalic,
            // Safety: `new` only ever stores style bits produced by
            // `FontStyle as u16`, so this branch is unreachable.
            _ => unreachable(),
        }
    }

    /// Extract the dense face index or reserved sprite sentinel.
    #[inline]
    pub const fn index(self) -> u16 {
        self.0 & Self::INDEX_MASK
    }
}

/// Terminal bold/italic combinations, distinct from named font face styles.
/// The discriminants index collection buckets and are packed into `FontIndex`.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Hash, Eq, PartialEq)]
pub enum FontStyle {
    Regular = 0,
    Bold = 1,
    Italic = 2,
    BoldItalic = 3,
}

impl FontStyle {
    pub const ALL: [Self; 4] = [Self::Regular, Self::Bold, Self::Italic, Self::BoldItalic];
}

pub struct ShapeOptions<'a> {
    /// OpenType feature list
    pub features: &'a [Feature],
}

/// Ghostty reference: `zig/ghostty/src/font/shaper/run.zig` (`TextRun`).
pub struct TextRun<'a> {
    /// Position-independent run hash for shaped-run cache keys.
    pub hash: u64,
    // TODO doc comment
    pub grid: &'a SharedGrid,
    /// Start column offset of this run in the row.
    pub offset: u16,
    /// Number of terminal cells covered by this run.
    pub cells: u16,
    /// Dense font face identity for the shaped segment.
    pub font_index: FontIndex,
}

/// Ghostty reference: `zig/ghostty/src/font/shape.zig` (`Cell`).
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct Cell {
    /// Terminal column offset relative to the run start (`TextRun.offset`).
    pub x: u16,
    /// Additional horizontal offset in pixels, positive rightward.
    pub x_offset: i16,
    /// Additional vertical offset in pixels to be applied at render time.
    /// This value is in font/shaper space. Positive Y means move up.
    pub y_offset: i16,
    /// Glyph ID in the mapped font face, or a sprite identifier for sprite runs.
    pub glyph_index: u32,
}
