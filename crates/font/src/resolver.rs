use std::sync::Arc;

#[cfg(target_os = "windows")]
use crate::backend::dwrite::discovery::DirectWrite as Discovery;

use crate::collection::Collection;
use crate::types::{FontIndex, FontStyle, Presentation, PresentationMode};
use unicode_properties::{EmojiStatus, UnicodeEmoji as _};

pub struct CodepointResolver {
    pub collection: Collection,
    pub styles: [bool; 4],
    pub discovery: Arc<Discovery>,
}

impl CodepointResolver {
    pub fn get_index(
        &mut self,
        cp: u32,
        style: FontStyle,
        presentation: Option<Presentation>,
    ) -> Option<FontIndex> {
        let style_enabled = self.styles[style as usize];

        // If a style is disabled, fallback to regular.
        if !style_enabled && style != FontStyle::Regular {
            return self.get_index(cp, FontStyle::Regular, presentation);
        }

        if ghostty::sprite::has_codepoint(cp) {
            return Some(FontIndex::SPRITE);
        }

        let p_mode: PresentationMode = match presentation {
            Some(p) => PresentationMode::Explicit(p),
            None => 'p_mode: {
                let Some(ch) = char::from_u32(cp) else {
                    break 'p_mode PresentationMode::Default(Presentation::Text);
                };
                // Ghostty reference: `CodepointResolver.getIndex` uses UCD
                // `is_emoji_presentation` when no explicit VS15/VS16 is present.
                let emoji_presentation = matches!(
                    ch.emoji_status(),
                    EmojiStatus::EmojiPresentation
                        | EmojiStatus::EmojiPresentationAndModifierBase
                        | EmojiStatus::EmojiPresentationAndEmojiComponent
                        | EmojiStatus::EmojiPresentationAndModifierAndEmojiComponent
                );
                match emoji_presentation {
                    true => PresentationMode::Default(Presentation::Emoji),
                    false => PresentationMode::Default(Presentation::Text),
                }
            }
        };

        if let Some(idx) = self.collection.get_index(cp, style, p_mode) {
            return Some(idx);
        }

        if style != FontStyle::Regular {
            // Resolve regular fallback fully, including system font discovery.
            return self.get_index(cp, FontStyle::Regular, presentation);
        }

        // Ghostty compatibility: only perform fallback discovery from regular
        // style resolution to avoid pulling in styled fallback faces.
        match self.discovery.discover_fallback(cp) {
            Ok(Some(entry)) => {
                if let Ok(idx) = self.collection.add(entry, FontStyle::Regular) {
                    return Some(idx);
                }
            }
            Err(e) => log::warn!("fallback font discovery failed for U+{cp:04X}: {e}"),
            Ok(None) => {}
        }

        // Fallback to regular with any presentation.
        let any_mode = PresentationMode::Any;
        if let Some(idx) = self.collection.get_index(cp, FontStyle::Regular, any_mode) {
            return Some(idx);
        }

        None
    }
}
