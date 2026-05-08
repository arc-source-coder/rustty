mod actions;
mod profile;
mod profile_registry;
mod shell_detection;
mod tab_strip;
mod types;
mod workspace;

use better_mimalloc_rs::MiMalloc;
use gpui::{
    App, AppContext, Bounds, KeyBinding, WindowBackgroundAppearance, WindowBounds, WindowOptions,
    px, size,
};
use gpui_platform::application;
use terminal::RenderConfig;
use ui::{components::theme::Theme, title_bar::title_bar_options};

use crate::actions::{CloseActiveTab, NewTab, SelectNextTab, SelectPreviousTab, SelectTab};
use crate::profile_registry::ProfileRegistry;
use crate::workspace::Workspace;
#[cfg(feature = "profiler")]
use tracy_client::Client;

const WORKSPACE_KEY_CONTEXT: Option<&str> = Some("Workspace");

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

fn main() {
    #[cfg(feature = "profiler")]
    let _client = Client::start();

    application().run(|cx: &mut App| {
        cx.bind_keys([
            KeyBinding::new("ctrl-t", NewTab, WORKSPACE_KEY_CONTEXT),
            KeyBinding::new("ctrl-w", CloseActiveTab, WORKSPACE_KEY_CONTEXT),
            KeyBinding::new("ctrl-tab", SelectNextTab, WORKSPACE_KEY_CONTEXT),
            KeyBinding::new("ctrl-shift-tab", SelectPreviousTab, WORKSPACE_KEY_CONTEXT),
            KeyBinding::new("ctrl-1", SelectTab { index: 0 }, WORKSPACE_KEY_CONTEXT),
            KeyBinding::new("ctrl-2", SelectTab { index: 1 }, WORKSPACE_KEY_CONTEXT),
            KeyBinding::new("ctrl-3", SelectTab { index: 2 }, WORKSPACE_KEY_CONTEXT),
            KeyBinding::new("ctrl-4", SelectTab { index: 3 }, WORKSPACE_KEY_CONTEXT),
            KeyBinding::new("ctrl-5", SelectTab { index: 4 }, WORKSPACE_KEY_CONTEXT),
            KeyBinding::new("ctrl-6", SelectTab { index: 5 }, WORKSPACE_KEY_CONTEXT),
            KeyBinding::new("ctrl-7", SelectTab { index: 6 }, WORKSPACE_KEY_CONTEXT),
            KeyBinding::new("ctrl-8", SelectTab { index: 7 }, WORKSPACE_KEY_CONTEXT),
            KeyBinding::new("ctrl-9", SelectTab { index: 8 }, WORKSPACE_KEY_CONTEXT),
        ]);

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
            titlebar: Some(title_bar_options()),
            window_bounds: Some(window_bounds),
            window_background: WindowBackgroundAppearance::Transparent,
            ..Default::default()
        };

        cx.open_window(window_options, |window, cx| {
            Theme::sync_system_appearance(window, cx);
            let workspace = cx.new(|cx| Workspace::new(profiles, render_config, window, cx));
            let focus = workspace.read(cx).active_terminal_focus_handle(cx);
            window.focus(&focus, cx);

            let workspace_handle = workspace.downgrade();
            window.on_window_should_close(cx, move |window, cx| {
                workspace_handle
                    .update(cx, |workspace, cx| {
                        workspace.handle_window_should_close(window, cx)
                    })
                    .unwrap_or(true)
            });

            workspace
        })
        .unwrap();
    });
}
