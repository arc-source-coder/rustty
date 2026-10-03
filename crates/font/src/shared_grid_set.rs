use std::collections::hash_map::Entry;
use std::rc::Rc;
use std::sync::{Arc, Weak};

use anyhow::{Result, anyhow};
use rustc_hash::FxHashMap;

#[cfg(target_os = "windows")]
use crate::backend::dwrite::discovery::DirectWrite as Discovery;

use crate::collection::Collection;
use crate::config::FontConfig;
use crate::resolver::CodepointResolver;
use crate::shared_grid::SharedGrid;

use crate::types::{FontDescriptor, FontSize, FontStyle};
use utils::floats::NotNan;

#[derive(Eq, PartialEq, Hash)]
pub struct Key {
    descriptors: Box<[FontDescriptor]>,
    style_offsets: [usize; 4],
    styles: [bool; 4],
    font_size: FontSize,
}

impl Key {
    pub fn new(config: &FontConfig, font_size: FontSize) -> Self {
        let mut descriptors = Vec::new();

        let regular_style = config.font_style_regular.name_value();
        let bold_style = config.font_style_bold.name_value();
        let italic_style = config.font_style_italic.name_value();
        let bold_italic_style = config.font_style_bold_italic.name_value();

        for family in &config.font_family_regular {
            descriptors.push(FontDescriptor {
                family: Rc::clone(family),
                size: font_size.points,
                style: regular_style.clone(),
                bold: false,
                italic: false,
                variations: Rc::clone(&config.font_variations_regular),
            });
        }
        for family in &config.font_family_bold {
            descriptors.push(FontDescriptor {
                family: Rc::clone(family),
                size: font_size.points,
                style: bold_style.clone(),
                bold: bold_style.is_none(),
                italic: false,
                variations: Rc::clone(&config.font_variations_bold),
            });
        }
        for family in &config.font_family_italic {
            descriptors.push(FontDescriptor {
                family: Rc::clone(family),
                size: font_size.points,
                style: italic_style.clone(),
                bold: false,
                italic: italic_style.is_none(),
                variations: Rc::clone(&config.font_variations_italic),
            });
        }
        for family in &config.font_family_bold_italic {
            descriptors.push(FontDescriptor {
                family: Rc::clone(family),
                size: font_size.points,
                style: bold_italic_style.clone(),
                bold: bold_italic_style.is_none(),
                italic: bold_italic_style.is_none(),
                variations: Rc::clone(&config.font_variations_bold_italic),
            });
        }

        let regular_offset = config.font_family_regular.len();
        let bold_offset = regular_offset + config.font_family_bold.len();
        let italic_offset = bold_offset + config.font_family_italic.len();
        let bold_italic_offset = italic_offset + config.font_family_bold_italic.len();

        Self {
            descriptors: descriptors.into_boxed_slice(),
            style_offsets: [regular_offset, bold_offset, italic_offset, bold_italic_offset],
            styles: [
                true,
                config.font_style_bold.is_enabled(),
                config.font_style_italic.is_enabled(),
                config.font_style_bold_italic.is_enabled(),
            ],
            font_size,
        }
    }

    fn descriptors_for_style(&self, style: FontStyle) -> &[FontDescriptor] {
        let idx = style as usize;
        let start = if idx == 0 { 0 } else { self.style_offsets[idx - 1] };
        let end = self.style_offsets[idx];
        &self.descriptors[start..end]
    }
}

/// Ghostty-style shared-grid registry keyed by derived font configuration.
/// Reuses live font grids with matching ordered font descriptors, enabled styles,
/// point size, and DPI. Shaping features are renderer-local and not part of the key.
pub struct SharedGridSet {
    grids: FxHashMap<Key, Weak<SharedGrid>>,
    discovery: Arc<Discovery>,
}

impl SharedGridSet {
    pub fn new() -> Result<Self> {
        Ok(Self { grids: FxHashMap::default(), discovery: Arc::new(Discovery::new()?) })
    }

    /// Return a live grid for this configuration and size, or discover its fonts
    /// and create one. Expired weak entries are pruned before lookup.
    pub fn grid_ref(&mut self, config: &FontConfig, size: FontSize) -> Result<Arc<SharedGrid>> {
        let key = Key::new(config, size);

        self.grids.retain(|_, v| v.strong_count() > 0);
        match self.grids.entry(key) {
            Entry::Occupied(existing) => Ok(existing.get().upgrade().unwrap()),
            Entry::Vacant(slot) => {
                let mut collection = Collection::new(size);

                for style in FontStyle::ALL {
                    for descriptor in slot.key().descriptors_for_style(style) {
                        if let Some(entry) = self.discovery.discover(descriptor)? {
                            collection.add(entry, style)?;
                        }
                    }
                }

                // Add Segoe UI Emoji to the collection for emoji fallback on Windows.
                #[cfg(target_os = "windows")]
                {
                    let descriptor = FontDescriptor {
                        family: Rc::from("Segoe UI Emoji"),
                        style: None,
                        bold: false,
                        italic: false,
                        size: NotNan::new(0.0).ok_or_else(|| anyhow!("Unexpected NaN value"))?,
                        variations: Rc::new([]),
                    };
                    let mut entry = self
                        .discovery
                        .discover(&descriptor)?
                        .ok_or_else(|| anyhow!("Failed to find Segoe UI Emoji"))?;
                    entry.fallback = true;
                    collection.add(entry, FontStyle::Regular)?;
                }

                let resolver = CodepointResolver {
                    collection,
                    styles: slot.key().styles,
                    discovery: Arc::clone(&self.discovery),
                };
                let grid = Arc::new(SharedGrid::new(resolver)?);
                slot.insert(Arc::downgrade(&grid));
                Ok(grid)
            }
        }
    }
}
