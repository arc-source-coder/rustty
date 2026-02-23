use gpui::{App, Global, Hsla, Window, WindowAppearance, rgba};

/// Hardcoded color palette for light and dark modes.
/// Values mirror the gpui-component Default Light/Dark theme.
#[derive(Debug, Clone, Copy)]
pub struct ThemeColors {
    pub background: Hsla,
    pub foreground: Hsla,
    pub border: Hsla,
    pub muted: Hsla,
    pub muted_foreground: Hsla,
    pub secondary_hover: Hsla,
    pub title_bar: Hsla,
    pub title_bar_border: Hsla,
}

impl ThemeColors {
    pub fn light() -> Self {
        Self {
            background: rgba(0xffffffff).into(),
            foreground: rgba(0x0a0a0aff).into(),
            border: rgba(0xe5e5e5ff).into(),
            muted: rgba(0xf5f5f5ff).into(),
            muted_foreground: rgba(0x737373ff).into(),
            secondary_hover: rgba(0xf0f0f0ff).into(),
            title_bar: rgba(0xf8f8f8ff).into(),
            title_bar_border: rgba(0xe5e5e5ff).into(),
        }
    }

    pub fn dark() -> Self {
        Self {
            background: rgba(0x0a0a0aff).into(),
            foreground: rgba(0xfafafaff).into(),
            border: rgba(0x262626ff).into(),
            muted: rgba(0x262626ff).into(),
            muted_foreground: rgba(0x737373ff).into(),
            secondary_hover: rgba(0x1a1a1aff).into(),
            title_bar: rgba(0x171717ff).into(),
            title_bar_border: rgba(0x262626ff).into(),
        }
    }
}

/// The active application theme, stored as a GPUI global.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub colors: ThemeColors,
    pub is_dark: bool,
}

impl Global for Theme {}

impl Theme {
    pub fn light() -> Self {
        Self {
            colors: ThemeColors::light(),
            is_dark: false,
        }
    }

    pub fn dark() -> Self {
        Self {
            colors: ThemeColors::dark(),
            is_dark: true,
        }
    }

    /// Sync the theme global to the current window appearance.
    pub fn sync_system_appearance(window: &mut Window, cx: &mut App) {
        let is_dark = matches!(
            window.appearance(),
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        );
        let theme = if is_dark { Self::dark() } else { Self::light() };
        cx.set_global(theme);
    }

    /// Returns the current global theme, falling back to light if uninitialised.
    pub fn get(cx: &App) -> Theme {
        cx.try_global::<Theme>()
            .copied()
            .unwrap_or_else(Self::light)
    }
}

/// Extension trait to access the active theme from any context.
pub trait ActiveTheme {
    fn theme(&self) -> Theme;
}

impl ActiveTheme for App {
    fn theme(&self) -> Theme {
        Theme::get(self)
    }
}
