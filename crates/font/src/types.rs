#[cfg(target_os = "windows")]
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_AXIS_TAG, DWRITE_FONT_AXIS_TAG_ITALIC, DWRITE_FONT_AXIS_TAG_SLANT,
    DWRITE_FONT_AXIS_TAG_WEIGHT, DWRITE_FONT_AXIS_VALUE, DWRITE_FONT_FEATURE,
    DWRITE_FONT_FEATURE_TAG, DWRITE_FONT_FEATURE_TAG_CONTEXTUAL_ALTERNATES,
    DWRITE_FONT_FEATURE_TAG_CONTEXTUAL_LIGATURES, DWRITE_FONT_FEATURE_TAG_STANDARD_LIGATURES,
    DWRITE_FONT_WEIGHT_BOLD, DWRITE_SCRIPT_ANALYSIS,
};

use crate::cache::glyph_cache::GlyphAtlasKind;

/// Ghostty-compatible presentation preference.
///
/// Ghostty reference: `font.Presentation` in `font/main.zig`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Presentation {
    Text,
    Emoji,
}

impl Presentation {
    /// Map a presentation to the atlas kind it renders into.
    ///
    /// Ghostty reference: `SharedGrid.renderGlyph` —
    ///   `.text => &self.atlas_grayscale, .emoji => &self.atlas_color`
    #[inline]
    pub const fn atlas_kind(self) -> GlyphAtlasKind {
        match self {
            Presentation::Text => GlyphAtlasKind::Grayscale,
            Presentation::Emoji => GlyphAtlasKind::Color,
        }
    }
}

/// Dense font face identifier — the Rustty equivalent of Ghostty's
/// `Collection.Index`.
///
/// Ghostty packs `(Style: u3, idx: u13)` into a `u16`, supporting up to
/// 8,192 fonts per style. We replicate the exact same layout so that
/// `GlyphKey` can pack into a single `u64`.
///
/// A terminal typically uses <10 font faces total (regular, bold, italic,
/// bold-italic, plus a handful of fallback fonts). The collection owns
/// dense `FontIndex` assignment.
///
/// ## Bit layout (same as Ghostty)
///
///   `[15:13]` style variant (3 bits, matches `Style` discriminant)
///   `[12:0]`  face index   (13 bits, up to 8,192 faces per style)
///
/// Ghostty reference:
///   `zig/ghostty/src/font/Collection.zig` — `Index`
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Default)]
#[repr(transparent)]
pub struct FontIndex(u16);

impl FontIndex {
    /// Maximum number of distinct faces per style variant.
    pub const MAX_FACES_PER_STYLE: u16 = (1 << 13) - 1; // 8191

    /// Construct from a style variant and a dense face index.
    ///
    /// Panics (debug) if `idx` exceeds 13 bits.
    #[inline]
    pub const fn new(style: Style, idx: u16) -> Self {
        debug_assert!(idx <= Self::MAX_FACES_PER_STYLE);
        Self((style as u16) << 13 | (idx & Self::MAX_FACES_PER_STYLE))
    }

    /// The raw `u16` value — suitable for packing into a `GlyphKey`.
    #[inline]
    pub const fn to_u16(self) -> u16 {
        self.0
    }

    /// Extract the style variant.
    #[inline]
    pub const fn style(self) -> Style {
        // SAFETY: the top 3 bits are always a valid Style
        // discriminant (0–3), because `new` only ever stores values
        // produced by `Style as u16`.
        unsafe { std::mem::transmute((self.0 >> 13) as u8) }
    }

    /// Extract the dense face index.
    #[inline]
    pub const fn index(self) -> u16 {
        self.0 & Self::MAX_FACES_PER_STYLE
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
#[repr(u8)]
pub enum Style {
    Normal,
    Bold,
    Italic,
    BoldItalic,
}

impl Style {
    pub const COUNT: usize = 4;
    pub const ALL: [Self; Self::COUNT] = [Self::Normal, Self::Bold, Self::Italic, Self::BoldItalic];

    pub fn is_bold(self) -> bool {
        matches!(self, Self::Bold | Self::BoldItalic)
    }

    pub fn is_italic(self) -> bool {
        matches!(self, Self::Italic | Self::BoldItalic)
    }
}

#[cfg(target_os = "windows")]
#[derive(Clone, Debug, Default)]
pub struct FontAxisSpec {
    /// Variable-axis values forwarded to DirectWrite fallback/shaping.
    ///
    /// WT reference:
    /// `opensrc/repos/microsoft/terminal/src/renderer/atlas/AtlasEngine.api.cpp`
    /// (`fontAxisValues` + `MapCharacters` axis path).
    pub values: Vec<DWRITE_FONT_AXIS_VALUE>,
}

#[cfg(target_os = "windows")]
impl FontAxisSpec {
    /// Parse variable-axis settings from user strings like:
    /// `wght=700`, `ital=1`, `slnt=-12`, `opsz=13.5`.
    ///
    /// WT compatibility notes:
    /// - If any axis setting is present, pre-seed `wght`/`ital`/`slnt`
    ///   with `-1` sentinel values (same as WT `UpdateFont`).
    /// - Invalid tags or values are ignored.
    pub fn parse(entries: &[&str]) -> Self {
        let mut values = Vec::with_capacity(entries.len() + 3);
        let mut saw_non_empty = false;
        for raw in entries {
            let item = raw.trim();
            if item.is_empty() {
                continue;
            }
            if !saw_non_empty {
                saw_non_empty = true;
                values.push(DWRITE_FONT_AXIS_VALUE {
                    axisTag: DWRITE_FONT_AXIS_TAG_WEIGHT,
                    value: -1.0,
                });
                values.push(DWRITE_FONT_AXIS_VALUE {
                    axisTag: DWRITE_FONT_AXIS_TAG_ITALIC,
                    value: -1.0,
                });
                values.push(DWRITE_FONT_AXIS_VALUE {
                    axisTag: DWRITE_FONT_AXIS_TAG_SLANT,
                    value: -1.0,
                });
            }

            let Some((raw_tag, raw_value)) = item.split_once('=') else {
                continue;
            };
            let Some(tag) = parse_axis_tag(raw_tag.trim()) else {
                continue;
            };
            let Ok(value) = raw_value.trim().parse::<f32>() else {
                continue;
            };
            ensure_or_set_axis(&mut values, tag, value);
        }
        if !saw_non_empty {
            return Self::default();
        }

        Self { values }
    }

    pub fn resolve_with_variant_defaults_into(
        &self,
        variant: Style,
        default_weight: u16,
        out: &mut Vec<DWRITE_FONT_AXIS_VALUE>,
    ) {
        out.clear();
        out.extend(self.values.iter().copied());
        apply_variant_axis_defaults(out, variant, default_weight);
    }
}

#[cfg(target_os = "windows")]
fn apply_variant_axis_defaults(
    values: &mut Vec<DWRITE_FONT_AXIS_VALUE>,
    variant: Style,
    default_weight: u16,
) {
    // WT-compatible defaults.
    let existing_weight = axis_value(values, DWRITE_FONT_AXIS_TAG_WEIGHT);
    ensure_or_set_axis(
        values,
        DWRITE_FONT_AXIS_TAG_WEIGHT,
        if variant.is_bold() {
            DWRITE_FONT_WEIGHT_BOLD.0 as f32
        } else if let Some(existing) = existing_weight.filter(|v| *v >= 0.0) {
            existing
        } else {
            default_weight as f32
        },
    );
    ensure_or_set_axis(
        values,
        DWRITE_FONT_AXIS_TAG_ITALIC,
        if variant.is_italic() {
            1.0
        } else {
            axis_value(values, DWRITE_FONT_AXIS_TAG_ITALIC)
                .filter(|v| *v >= 0.0)
                .unwrap_or(0.0)
        },
    );
    ensure_or_set_axis(
        values,
        DWRITE_FONT_AXIS_TAG_SLANT,
        if variant.is_italic() {
            -12.0
        } else {
            axis_value(values, DWRITE_FONT_AXIS_TAG_SLANT)
                .filter(|v| *v >= 0.0)
                .unwrap_or(0.0)
        },
    );
}

#[cfg(target_os = "windows")]
fn axis_value(values: &[DWRITE_FONT_AXIS_VALUE], tag: DWRITE_FONT_AXIS_TAG) -> Option<f32> {
    values.iter().find(|v| v.axisTag == tag).map(|v| v.value)
}

#[cfg(target_os = "windows")]
fn ensure_or_set_axis(
    values: &mut Vec<DWRITE_FONT_AXIS_VALUE>,
    tag: DWRITE_FONT_AXIS_TAG,
    value: f32,
) {
    if let Some(found) = values.iter_mut().find(|v| v.axisTag == tag) {
        found.value = value;
        return;
    }
    values.push(DWRITE_FONT_AXIS_VALUE {
        axisTag: tag,
        value,
    });
}

#[cfg(target_os = "windows")]
#[inline]
fn parse_axis_tag(tag: &str) -> Option<DWRITE_FONT_AXIS_TAG> {
    Some(DWRITE_FONT_AXIS_TAG(parse_tag4(tag)?))
}

#[cfg(target_os = "windows")]
fn parse_tag4(tag: &str) -> Option<u32> {
    let bytes = tag.as_bytes();
    if bytes.len() != 4 || !bytes.is_ascii() {
        return None;
    }
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

#[cfg(target_os = "windows")]
#[derive(Clone, Debug, Default)]
pub struct FontFeatureSpec {
    /// OpenType feature toggles/values used by `GetGlyphs` and
    /// `GetGlyphPlacements` via `DWRITE_TYPOGRAPHIC_FEATURES`.
    ///
    /// WT reference:
    /// `opensrc/repos/microsoft/terminal/src/renderer/atlas/AtlasEngine.api.cpp`
    /// (`fontFeatures` building and DWrite feature comments).
    pub features: Vec<DWRITE_FONT_FEATURE>,
}

#[cfg(target_os = "windows")]
impl FontFeatureSpec {
    /// Parse OpenType feature entries from strings like:
    /// `liga`, `+liga`, `-liga`, `calt=0`, `ss01=1`.
    ///
    /// WT compatibility notes:
    /// - If any feature settings are present, pre-seed defaults
    ///   for `liga`/`clig`/`calt` as enabled.
    /// - Invalid tags or values are ignored.
    pub fn parse(entries: &[&str]) -> Self {
        let mut features = Vec::with_capacity(entries.len() + 3);
        let mut saw_non_empty = false;
        for raw in entries {
            let item = raw.trim();
            if item.is_empty() {
                continue;
            }
            if !saw_non_empty {
                saw_non_empty = true;
                features.push(DWRITE_FONT_FEATURE {
                    nameTag: DWRITE_FONT_FEATURE_TAG_STANDARD_LIGATURES,
                    parameter: 1,
                });
                features.push(DWRITE_FONT_FEATURE {
                    nameTag: DWRITE_FONT_FEATURE_TAG_CONTEXTUAL_LIGATURES,
                    parameter: 1,
                });
                features.push(DWRITE_FONT_FEATURE {
                    nameTag: DWRITE_FONT_FEATURE_TAG_CONTEXTUAL_ALTERNATES,
                    parameter: 1,
                });
            }

            if let Some(stripped) = item.strip_prefix('+') {
                if let Some(tag) = parse_feature_tag(stripped.trim()) {
                    ensure_or_set_feature(&mut features, tag, 1);
                }
                continue;
            }
            if let Some(stripped) = item.strip_prefix('-') {
                if let Some(tag) = parse_feature_tag(stripped.trim()) {
                    ensure_or_set_feature(&mut features, tag, 0);
                }
                continue;
            }
            if let Some((raw_tag, raw_value)) = item.split_once('=') {
                let Some(tag) = parse_feature_tag(raw_tag.trim()) else {
                    continue;
                };
                let Ok(value) = raw_value.trim().parse::<f32>() else {
                    continue;
                };
                let parameter = value.round().max(0.0) as u32;
                ensure_or_set_feature(&mut features, tag, parameter);
                continue;
            }
            if let Some(tag) = parse_feature_tag(item.trim()) {
                ensure_or_set_feature(&mut features, tag, 1);
            }
        }
        if !saw_non_empty {
            return Self::default();
        }

        Self { features }
    }
}

#[cfg(target_os = "windows")]
#[inline]
fn parse_feature_tag(tag: &str) -> Option<DWRITE_FONT_FEATURE_TAG> {
    Some(DWRITE_FONT_FEATURE_TAG(parse_tag4(tag)?))
}

#[cfg(target_os = "windows")]
fn ensure_or_set_feature(
    features: &mut Vec<DWRITE_FONT_FEATURE>,
    tag: DWRITE_FONT_FEATURE_TAG,
    parameter: u32,
) {
    if let Some(found) = features.iter_mut().find(|f| f.nameTag == tag) {
        found.parameter = parameter;
        return;
    }
    features.push(DWRITE_FONT_FEATURE {
        nameTag: tag,
        parameter,
    });
}

#[derive(Clone, Debug)]
pub struct ShapeOptions {
    /// Locale name for script analysis and fallback mapping.
    pub locale: String,
    /// Font em-size passed to DirectWrite placement.
    pub font_size: f32,
    /// Cell width used by the WT simple-text fast path.
    pub cell_width: f32,
    /// Terminal style variant to map to normal/bold/italic/bold-italic.
    pub variant: Style,
    #[cfg(target_os = "windows")]
    /// OpenType feature list (WT-style feature plumbing).
    pub features: FontFeatureSpec,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
pub struct TextRun {
    /// Position-independent run hash for shaped-run cache keys.
    ///
    /// Ghostty reference:
    /// `zig/ghostty/src/font/shaper/run.zig` (`TextRun.hash`).
    pub hash: u64,
    /// Start column offset of this run in the row.
    ///
    /// Ghostty reference:
    /// `zig/ghostty/src/font/shaper/run.zig` (`TextRun.offset`).
    pub offset: u16,
    /// Number of terminal cells covered by this run.
    ///
    /// Ghostty reference:
    /// `zig/ghostty/src/font/shaper/run.zig` (`TextRun.cells`).
    pub cells: u16,
    /// Dense font face identity for the shaped segment.
    ///
    /// Ghostty reference:
    /// `zig/ghostty/src/font/shaper/run.zig` (`TextRun.font_index`).
    pub font_index: FontIndex,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
pub struct Cell {
    /// Cell-local X position relative to `TextRun.offset`.
    ///
    /// Ghostty reference:
    /// `zig/ghostty/src/font/shape.zig` (`Cell.x`).
    pub x: u16,
    /// Additional X offset applied at render time.
    ///
    /// Ghostty reference:
    /// `zig/ghostty/src/font/shape.zig` (`Cell.x_offset`).
    pub x_offset: i16,
    /// Additional Y offset applied at render time.
    ///
    /// Ghostty reference:
    /// `zig/ghostty/src/font/shape.zig` (`Cell.y_offset`).
    pub y_offset: i16,
    /// Glyph id/index in the mapped font face.
    ///
    /// Ghostty reference:
    /// `zig/ghostty/src/font/shape.zig` (`Cell.glyph_index`).
    pub glyph_index: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GlyphOffset {
    /// Additional horizontal shift (DirectWrite `advanceOffset`).
    ///
    /// WT reference:
    /// `opensrc/repos/microsoft/terminal/src/renderer/atlas/AtlasEngine.cpp`
    /// (`_api.glyphOffsets` from `GetGlyphPlacements`).
    pub advance_offset: f32,
    /// Additional vertical shift (DirectWrite `ascenderOffset`).
    ///
    /// WT reference:
    /// `opensrc/repos/microsoft/terminal/src/renderer/atlas/AtlasEngine.cpp`
    /// (`_api.glyphOffsets` from `GetGlyphPlacements`).
    pub ascender_offset: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct ScriptRun {
    /// UTF-16 start position in the source run.
    ///
    /// WT equivalent:
    /// `TextAnalysisSinkResult.textPosition` from
    /// `opensrc/repos/microsoft/terminal/src/renderer/atlas/common.h`.
    pub text_position: u32,
    /// UTF-16 length of this script segment.
    ///
    /// WT equivalent:
    /// `TextAnalysisSinkResult.textLength` from
    /// `opensrc/repos/microsoft/terminal/src/renderer/atlas/common.h`.
    pub text_length: u32,
    #[cfg(target_os = "windows")]
    /// DirectWrite script metadata produced by `AnalyzeScript`.
    ///
    /// WT equivalent:
    /// `TextAnalysisSinkResult.analysis` from
    /// `opensrc/repos/microsoft/terminal/src/renderer/atlas/common.h`.
    pub analysis: DWRITE_SCRIPT_ANALYSIS,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct BidiRun {
    /// UTF-16 start position for this bidi-level span.
    ///
    /// WT equivalent:
    /// `SetBidiLevel(textPosition, textLength, ...)` callback arguments in
    /// `opensrc/repos/microsoft/terminal/src/renderer/atlas/DWriteTextAnalysis.h`.
    pub text_position: u32,
    /// UTF-16 length for this bidi-level span.
    pub text_length: u32,
    /// Resolved Unicode bidi embedding level.
    pub resolved_level: u8,
}

pub struct ShapedCells<'a> {
    /// Mapped sub-runs produced by fallback/script segmentation.
    ///
    /// Ghostty parallel:
    /// run iterator emits run boundaries before shaping.
    pub runs: &'a [TextRun],
    /// Per-run `[start, end)` range in `cells`.
    pub run_cell_spans: &'a [RunSpan],
    /// Per-run `[start, end)` range in `glyph_advances` and `glyph_offsets`.
    pub run_glyph_spans: &'a [RunSpan],
    /// Per-run `[start, end)` range in `cluster_map`.
    pub run_cluster_spans: &'a [RunSpan],
    /// Final per-cell render payload.
    ///
    /// Ghostty equivalent:
    /// `[]font.shape.Cell` returned from shaper `shape(run)` and cached in
    /// `zig/ghostty/src/font/shaper/Cache.zig`.
    pub cells: &'a [Cell],
    /// UTF-16 cluster to glyph-start map, with sentinel at `text_len`.
    ///
    /// WT reference:
    /// `_api.clusterMap` in
    /// `opensrc/repos/microsoft/terminal/src/renderer/atlas/AtlasEngine.cpp`.
    pub cluster_map: &'a [u16],
    /// Per-glyph advances from `GetGlyphPlacements`.
    pub glyph_advances: &'a [f32],
    /// Per-glyph offsets from `GetGlyphPlacements`.
    pub glyph_offsets: &'a [GlyphOffset],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
pub struct RunSpan(pub u64);

impl RunSpan {
    /// Packs `[start, end)` into one `u64` (`start` high 32, `end` low 32).
    #[inline]
    pub const fn new(start: u32, end: u32) -> Self {
        Self(((start as u64) << 32) | (end as u64))
    }

    #[inline]
    pub const fn start(self) -> u32 {
        (self.0 >> 32) as u32
    }

    #[inline]
    pub const fn end(self) -> u32 {
        self.0 as u32
    }
}

#[cfg(all(test, target_os = "windows"))]
mod windows_tests {
    use super::*;

    #[test]
    fn feature_parse_supports_defaults_and_overrides() {
        let spec = FontFeatureSpec::parse(&["-liga", "ss01=1", "clig=0"]);
        let liga = spec
            .features
            .iter()
            .find(|f| f.nameTag == DWRITE_FONT_FEATURE_TAG_STANDARD_LIGATURES)
            .unwrap();
        let clig = spec
            .features
            .iter()
            .find(|f| f.nameTag == DWRITE_FONT_FEATURE_TAG_CONTEXTUAL_LIGATURES)
            .unwrap();
        let calt = spec
            .features
            .iter()
            .find(|f| f.nameTag == DWRITE_FONT_FEATURE_TAG_CONTEXTUAL_ALTERNATES)
            .unwrap();
        assert_eq!(liga.parameter, 0);
        assert_eq!(clig.parameter, 0);
        assert_eq!(calt.parameter, 1);
        assert!(
            spec.features
                .iter()
                .any(|f| { f.nameTag.0 == u32::from_le_bytes(*b"ss01") && f.parameter == 1 })
        );
    }

    #[test]
    fn axis_parse_and_variant_defaults_match_wt_style() {
        let spec = FontAxisSpec::parse(&["opsz=13.5"]);
        let mut resolved = Vec::new();
        spec.resolve_with_variant_defaults_into(Style::Italic, 450, &mut resolved);

        let wght = resolved
            .iter()
            .find(|v| v.axisTag == DWRITE_FONT_AXIS_TAG_WEIGHT)
            .unwrap();
        let ital = resolved
            .iter()
            .find(|v| v.axisTag == DWRITE_FONT_AXIS_TAG_ITALIC)
            .unwrap();
        let slnt = resolved
            .iter()
            .find(|v| v.axisTag == DWRITE_FONT_AXIS_TAG_SLANT)
            .unwrap();
        let opsz = resolved
            .iter()
            .find(|v| v.axisTag.0 == u32::from_le_bytes(*b"opsz"))
            .unwrap();
        assert_eq!(wght.value, 450.0);
        assert_eq!(ital.value, 1.0);
        assert_eq!(slnt.value, -12.0);
        assert_eq!(opsz.value, 13.5);
    }
}
