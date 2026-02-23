use gpui::{
    AnyElement, App, Context, Decorations, InteractiveElement, IntoElement, MouseButton,
    ParentElement, Pixels, Render, RenderOnce, StatefulInteractiveElement, Styled, TitlebarOptions,
    Window, WindowControlArea, div, prelude::FluentBuilder, px, rgba, svg, white,
};

use crate::components::{styled::h_flex, theme::ActiveTheme};

pub const TITLE_BAR_HEIGHT: Pixels = px(34.);

/// The traffic light button offset used on macOS.
const TRAFFIC_LIGHT_PADDING: Pixels = px(80.);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlatformStyle {
    Mac,
    Linux,
    Windows,
}

impl PlatformStyle {
    fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Mac
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else {
            Self::Linux
        }
    }
}

pub struct TitleBar {
    children: Vec<AnyElement>,
    platform_style: PlatformStyle,
    should_move: bool,
}

impl TitleBar {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            children: Vec::new(),
            platform_style: PlatformStyle::current(),
            should_move: false,
        }
    }

    pub fn title_bar_options() -> TitlebarOptions {
        TitlebarOptions {
            title: None,
            appears_transparent: true,
            traffic_light_position: Some(gpui::point(px(9.0), px(9.0))),
        }
    }
}

impl Render for TitleBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let decorations = window.window_decorations();
        let children = std::mem::take(&mut self.children);

        let left_pad = if !window.is_fullscreen() && self.platform_style == PlatformStyle::Mac {
            TRAFFIC_LIGHT_PADDING
        } else {
            px(12.)
        };

        h_flex()
            .id("title-bar")
            .w_full()
            .h(TITLE_BAR_HEIGHT)
            .pl(left_pad)
            .occlude()
            .window_control_area(WindowControlArea::Drag)
            .bg(theme.title_bar)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, _| {
                    this.should_move = true;
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| {
                    this.should_move = false;
                }),
            )
            .on_mouse_down_out(cx.listener(|this, _, _, _| {
                this.should_move = false;
            }))
            .on_mouse_move(cx.listener(|this, _, window, _| {
                if this.should_move {
                    this.should_move = false;
                    window.start_window_move();
                }
            }))
            .when(self.platform_style == PlatformStyle::Mac, |el| {
                el.on_click(|event, window, _| {
                    if event.click_count() == 2 {
                        window.titlebar_double_click();
                    }
                })
            })
            .when(self.platform_style == PlatformStyle::Linux, |el| {
                el.on_click(|event, window, _| {
                    if event.click_count() == 2 {
                        window.zoom_window();
                    }
                })
            })
            .map(|el| match decorations {
                Decorations::Server => el,
                Decorations::Client { tiling, .. } => el
                    .when(!(tiling.top || tiling.right), |el| el.rounded_tr(px(8.)))
                    .when(!(tiling.top || tiling.left), |el| el.rounded_tl(px(8.)))
                    // Slight negative margin to avoid a transparent gap at rounded corners.
                    .mt(px(-1.))
                    .border(px(1.))
                    .border_color(theme.title_bar),
            })
            .content_stretch()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .overflow_x_hidden()
                    .w_full()
                    .children(children),
            )
            .when(!window.is_fullscreen(), |el| match self.platform_style {
                PlatformStyle::Mac => el,
                PlatformStyle::Linux => {
                    if matches!(decorations, Decorations::Client { .. }) {
                        el.child(LinuxWindowControls)
                            .on_mouse_down(MouseButton::Right, |ev, window, _| {
                                window.show_window_menu(ev.position)
                            })
                    } else {
                        el
                    }
                }
                PlatformStyle::Windows => el.child(WindowsWindowControls {
                    button_height: TITLE_BAR_HEIGHT,
                }),
            })
    }
}

// --- Windows window controls ---

#[derive(IntoElement)]
struct WindowsWindowControls {
    button_height: Pixels,
}

impl RenderOnce for WindowsWindowControls {
    fn render(self, window: &mut Window, _cx: &mut App) -> impl IntoElement {
        div()
            .id("windows-window-controls")
            .font_family("Segoe Fluent Icons")
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
enum WindowsCaptionButton {
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
            .w(px(36.))
            .h_full()
            .text_size(px(10.0))
            .text_color(theme.foreground)
            .hover(|style| style.bg(hover_bg).text_color(hover_fg))
            .active(|style| style.bg(active_bg))
            .window_control_area(self.control_area())
            .child(self.icon())
    }
}

// --- Linux window controls ---

#[derive(IntoElement)]
struct LinuxWindowControls;

impl RenderOnce for LinuxWindowControls {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme().colors;

        h_flex()
            .id("linux-window-controls")
            .px_3()
            .gap_2()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(LinuxControlButton::new(
                "wc-minimize",
                LinuxControlType::Minimize,
                theme.secondary_hover,
                theme.foreground,
            ))
            .child(LinuxControlButton::new(
                "wc-maximize",
                if window.is_maximized() {
                    LinuxControlType::Restore
                } else {
                    LinuxControlType::Maximize
                },
                theme.secondary_hover,
                theme.foreground,
            ))
            .child(LinuxControlButton::new(
                "wc-close",
                LinuxControlType::Close,
                theme.secondary_hover,
                theme.foreground,
            ))
    }
}

#[derive(Debug, Clone, Copy)]
enum LinuxControlType {
    Minimize,
    Restore,
    Maximize,
    Close,
}

impl LinuxControlType {
    fn svg_path(self) -> &'static str {
        match self {
            Self::Minimize => "icons/window-minimize.svg",
            Self::Restore => "icons/window-restore.svg",
            Self::Maximize => "icons/window-maximize.svg",
            Self::Close => "icons/window-close.svg",
        }
    }
}

#[derive(IntoElement)]
struct LinuxControlButton {
    id: gpui::ElementId,
    control_type: LinuxControlType,
    hover_bg: gpui::Hsla,
    icon_color: gpui::Hsla,
}

impl LinuxControlButton {
    fn new(
        id: impl Into<gpui::ElementId>,
        control_type: LinuxControlType,
        hover_bg: gpui::Hsla,
        icon_color: gpui::Hsla,
    ) -> Self {
        Self {
            id: id.into(),
            control_type,
            hover_bg,
            icon_color,
        }
    }
}

impl RenderOnce for LinuxControlButton {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let control_type = self.control_type;
        let icon = svg()
            .size_4()
            .flex_none()
            .path(control_type.svg_path())
            .text_color(self.icon_color);

        h_flex()
            .id(self.id)
            .cursor_pointer()
            .justify_center()
            .content_center()
            .rounded_md()
            .w_5()
            .h_5()
            .hover(|s| s.bg(self.hover_bg))
            .child(icon)
            .on_mouse_move(|_, _, cx| cx.stop_propagation())
            .on_click(move |_, window, cx| {
                cx.stop_propagation();
                match control_type {
                    LinuxControlType::Minimize => window.minimize_window(),
                    LinuxControlType::Restore | LinuxControlType::Maximize => window.zoom_window(),
                    LinuxControlType::Close => window.remove_window(),
                }
            })
    }
}
