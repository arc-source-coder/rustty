pub mod font;

mod temp;
pub use temp::SpawnConfig;

use std::rc::Rc;

use crate::font::{FontStyle, Variation};
use ghostty::font::Feature;

/// Minimal terminal config. Currently only contains rendering-specific fields.
/// This should be expanded when introducing the full config system backed by Ghostty.
pub struct Config {
    pub font_size: f32,
    pub font_features: Rc<[Feature]>,
    /// Terminal background opacity in the inclusive range 0.0..=1.0.
    pub background_opacity: f32,

    pub font_family_regular: Vec<Rc<str>>,
    pub font_style_regular: FontStyle,

    pub font_family_bold: Vec<Rc<str>>,
    pub font_style_bold: FontStyle,

    pub font_family_italic: Vec<Rc<str>>,
    pub font_style_italic: FontStyle,

    pub font_family_bold_italic: Vec<Rc<str>>,
    pub font_style_bold_italic: FontStyle,

    pub font_variations_regular: Rc<[Variation]>,
    pub font_variations_bold: Rc<[Variation]>,
    pub font_variations_italic: Rc<[Variation]>,
    pub font_variations_bold_italic: Rc<[Variation]>,
}

impl Default for Config {
    fn default() -> Self {
        let font_family: Rc<str> = Rc::from("Cascadia Code");
        Self {
            font_size: 11.0,
            font_features: Feature::parse("+liga"),
            background_opacity: 1.0,
            font_family_regular: vec![Rc::clone(&font_family)],
            font_style_regular: FontStyle::Default,
            font_family_bold: vec![Rc::clone(&font_family)],
            font_style_bold: FontStyle::Default,
            font_family_italic: vec![Rc::clone(&font_family)],
            font_style_italic: FontStyle::Default,
            font_family_bold_italic: vec![font_family],
            font_style_bold_italic: FontStyle::Default,
            font_variations_regular: Rc::new([]),
            font_variations_bold: Rc::new([]),
            font_variations_italic: Rc::new([]),
            font_variations_bold_italic: Rc::new([]),
        }
    }
}
