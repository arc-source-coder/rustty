use config::Config;
use config::font::{FontStyle, Variation};

use ghostty::font::Feature;

use std::rc::Rc;

pub struct FontConfig {
    pub font_features: Rc<[Feature]>,

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

impl From<&Config> for FontConfig {
    fn from(config: &Config) -> Self {
        Self {
            font_features: Rc::clone(&config.font_features),
            font_family_regular: config.font_family_regular.clone(),
            font_style_regular: config.font_style_regular.clone(),
            font_family_bold: config.font_family_bold.clone(),
            font_style_bold: config.font_style_bold.clone(),
            font_family_italic: config.font_family_italic.clone(),
            font_style_italic: config.font_style_italic.clone(),
            font_family_bold_italic: config.font_family_bold_italic.clone(),
            font_style_bold_italic: config.font_style_bold_italic.clone(),
            font_variations_regular: Rc::clone(&config.font_variations_regular),
            font_variations_bold: Rc::clone(&config.font_variations_bold),
            font_variations_italic: Rc::clone(&config.font_variations_italic),
            font_variations_bold_italic: Rc::clone(&config.font_variations_bold_italic),
        }
    }
}
