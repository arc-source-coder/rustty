use gpui::{
    App, InteractiveElement, IntoElement, ParentElement, Pixels, RenderOnce,
    StatefulInteractiveElement, Styled, TitlebarOptions, Window, WindowControlArea, div, px, rgba,
    white,
};

use crate::components::{styled::h_flex, theme::ActiveTheme};

pub const TITLE_BAR_HEIGHT_WINDOWED: Pixels = px(40.);
pub const TITLE_BAR_HEIGHT_MAXIMIZED: Pixels = px(36.);
pub const WINDOWS_CAPTION_BUTTON_WIDTH: Pixels = px(46.);

#[cfg(target_os = "windows")]
pub fn windows_symbol_font() -> String {
    use windows::Wdk::System::SystemServices::RtlGetVersion;

    let mut version = unsafe { std::mem::zeroed() };
    let status = unsafe { RtlGetVersion(&mut version) };

    if status.is_ok() && version.dwBuildNumber >= 22000 {
        "Segoe Fluent Icons".to_string()
    } else {
        "Segoe MDL2 Assets".to_string()
    }
}

pub fn title_bar_height(window: &Window) -> Pixels {
    if window.is_maximized() {
        TITLE_BAR_HEIGHT_MAXIMIZED
    } else {
        TITLE_BAR_HEIGHT_WINDOWED
    }
}

pub fn title_bar_options() -> TitlebarOptions {
    TitlebarOptions {
        title: None,
        appears_transparent: true,
        traffic_light_position: None,
    }
}

#[derive(IntoElement)]
pub struct WindowsWindowControls {
    pub button_height: Pixels,
    icon_font: String,
}

impl WindowsWindowControls {
    pub fn new(button_height: Pixels) -> Self {
        Self {
            button_height,
            icon_font: windows_symbol_font(),
        }
    }
}

impl RenderOnce for WindowsWindowControls {
    fn render(self, window: &mut Window, _cx: &mut App) -> impl IntoElement {
        div()
            .id("windows-window-controls")
            .font_family(self.icon_font)
            .flex()
            .flex_row()
            .justify_center()
            .content_stretch()
            .max_h(self.button_height)
            .min_h(self.button_height)
            .child(WindowsCaptionButton::Minimize)
            .child(if window.is_maximized() {
                WindowsCaptionButton::Restore
            } else {
                WindowsCaptionButton::Maximize
            })
            .child(WindowsCaptionButton::Close)
    }
}

#[derive(IntoElement)]
pub enum WindowsCaptionButton {
    Minimize,
    Restore,
    Maximize,
    Close,
}

impl WindowsCaptionButton {
    fn id(&self) -> &'static str {
        match self {
            Self::Minimize => "minimize",
            Self::Restore => "restore",
            Self::Maximize => "maximize",
            Self::Close => "close",
        }
    }

    fn icon(&self) -> &'static str {
        match self {
            Self::Minimize => "\u{e921}",
            Self::Restore => "\u{e923}",
            Self::Maximize => "\u{e922}",
            Self::Close => "\u{e8bb}",
        }
    }

    fn control_area(&self) -> WindowControlArea {
        match self {
            Self::Close => WindowControlArea::Close,
            Self::Maximize | Self::Restore => WindowControlArea::Max,
            Self::Minimize => WindowControlArea::Min,
        }
    }
}

impl RenderOnce for WindowsCaptionButton {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let is_close = matches!(self, Self::Close);
        let theme = cx.theme().colors;

        let (hover_bg, hover_fg, active_bg) = if is_close {
            (rgba(0xe81120e6).into(), white(), rgba(0xe81120cc).into())
        } else {
            (theme.secondary_hover, theme.foreground, theme.muted)
        };

        h_flex()
            .id(self.id())
            .justify_center()
            .content_center()
            .occlude()
            .w(WINDOWS_CAPTION_BUTTON_WIDTH)
            .h_full()
            .text_size(px(10.0))
            .text_color(theme.foreground)
            .hover(|style| style.bg(hover_bg).text_color(hover_fg))
            .active(|style| style.bg(active_bg))
            .window_control_area(self.control_area())
            .child(self.icon())
    }
}
