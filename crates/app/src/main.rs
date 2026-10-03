mod actions;
mod profile;
mod profile_registry;
mod shell_detection;
mod tab_strip;
mod types;
mod workspace;

use better_mimalloc_rs::MiMalloc;
use gpui::{
    App, AppContext as _, Bounds, KeyBinding, WindowBackgroundAppearance, WindowBounds,
    WindowOptions, px, size,
};
use gpui_platform::application;
use ui::components::theme::Theme;
use ui::title_bar::title_bar_options;

use crate::actions::{
    CloseActiveTab, CloseWindow, DecreaseFontSize, IncreaseFontSize, NewTab, ResetFontSize,
    SelectNextTab, SelectPreviousTab, SelectTab1, SelectTab2, SelectTab3, SelectTab4, SelectTab5,
    SelectTab6, SelectTab7, SelectTab8, SelectTab9,
};
use crate::profile_registry::ProfileRegistry;
use crate::workspace::Workspace;

const WORKSPACE_KEY_CONTEXT: Option<&str> = Some("Workspace");

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

fn main() {
    application().run(|cx: &mut App| {
        ui::close_prompt::register(cx);

        cx.bind_keys([
            KeyBinding::new("ctrl-t", NewTab, WORKSPACE_KEY_CONTEXT),
            KeyBinding::new("ctrl-w", CloseActiveTab, WORKSPACE_KEY_CONTEXT),
            KeyBinding::new("alt-f4", CloseWindow, WORKSPACE_KEY_CONTEXT),
            KeyBinding::new("ctrl-tab", SelectNextTab, WORKSPACE_KEY_CONTEXT),
            KeyBinding::new("ctrl-shift-tab", SelectPreviousTab, WORKSPACE_KEY_CONTEXT),
            KeyBinding::new("ctrl-1", SelectTab1, WORKSPACE_KEY_CONTEXT),
            KeyBinding::new("ctrl-2", SelectTab2, WORKSPACE_KEY_CONTEXT),
            KeyBinding::new("ctrl-3", SelectTab3, WORKSPACE_KEY_CONTEXT),
            KeyBinding::new("ctrl-4", SelectTab4, WORKSPACE_KEY_CONTEXT),
            KeyBinding::new("ctrl-5", SelectTab5, WORKSPACE_KEY_CONTEXT),
            KeyBinding::new("ctrl-6", SelectTab6, WORKSPACE_KEY_CONTEXT),
            KeyBinding::new("ctrl-7", SelectTab7, WORKSPACE_KEY_CONTEXT),
            KeyBinding::new("ctrl-8", SelectTab8, WORKSPACE_KEY_CONTEXT),
            KeyBinding::new("ctrl-9", SelectTab9, WORKSPACE_KEY_CONTEXT),
            KeyBinding::new("ctrl-=", IncreaseFontSize, WORKSPACE_KEY_CONTEXT),
            KeyBinding::new("ctrl-+", IncreaseFontSize, WORKSPACE_KEY_CONTEXT),
            KeyBinding::new("ctrl--", DecreaseFontSize, WORKSPACE_KEY_CONTEXT),
            KeyBinding::new("ctrl-0", ResetFontSize, WORKSPACE_KEY_CONTEXT),
        ]);

        // Detect available shells and build profile list.
        let (profiles, default_id) = shell_detection::detect_profiles();
        let profiles = cx.new(|_| ProfileRegistry::new(profiles, default_id));

        // Window sizing: 60% x 70% of primary display, centered.
        let display_bounds = cx.primary_display().map_or_else(
            || Bounds::centered(None, size(px(1024.), px(720.)), cx),
            |display| display.bounds(),
        );

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
            let workspace = cx.new(|cx| Workspace::new(profiles, window, cx));
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
