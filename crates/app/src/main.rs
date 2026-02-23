use gpui::{
    App, Bounds, Context, Entity, Render, Window, WindowBounds, WindowOptions, div, prelude::*, px,
    size,
};
use gpui_platform::application;

use ui::{components::theme::Theme, title_bar::TitleBar};

struct AppView {
    titlebar: Entity<TitleBar>,
}

impl Render for AppView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(gpui::black())
            .child(self.titlebar.clone().into_any_element())
            .child(
                div()
                    .flex_1()
                    .bg(gpui::black())
                    .text_color(gpui::white())
                    .text_lg()
                    .child("Terminal content coming soon..."),
            )
    }
}

fn main() {
    application().run(|cx: &mut App| {
        let display_bounds = cx
            .primary_display()
            .map(|display| display.bounds())
            .unwrap_or_else(|| Bounds::centered(None, size(px(1024.), px(720.)), cx));

        let target_size = size(
            display_bounds.size.width * 0.6,
            display_bounds.size.height * 0.7,
        );
        let window_bounds =
            WindowBounds::Windowed(Bounds::centered_at(display_bounds.center(), target_size));

        let window_options = WindowOptions {
            titlebar: Some(TitleBar::title_bar_options()),
            window_bounds: Some(window_bounds),
            ..Default::default()
        };

        cx.open_window(window_options, |window, cx| {
            Theme::sync_system_appearance(window, cx);
            cx.new(|cx| AppView {
                titlebar: cx.new(|cx| TitleBar::new(cx)),
            })
        })
        .unwrap();
    });
}
