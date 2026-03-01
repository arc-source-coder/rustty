mod profile;
mod profile_registry;
mod shell_detection;
mod types;
mod workspace;

use gpui::{App, AppContext, Bounds, Context, WindowBounds, WindowOptions, px, size};
use gpui_platform::application;
use terminal::RenderConfig;
use ui::{components::theme::Theme, title_bar::TitleBar};

use crate::profile_registry::ProfileRegistry;
use crate::workspace::Workspace;

fn main() {
    application().run(|cx: &mut App| {
        // Detect available shells and build profile list.
        let (profiles, default_id) = shell_detection::detect_profiles();
        let profiles = cx.new(|_| ProfileRegistry::new(profiles, default_id));

        // Shared render config (v0: defaults only, no config file).
        let render_config = cx.new(|_| RenderConfig::default());

        // Window sizing: 60% x 70% of primary display, centered.
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
            cx.new(|cx| Workspace::new(profiles, render_config, window, cx))
        })
        .unwrap();
    });
}
