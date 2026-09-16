//! wavemark GUI — a GPU-accelerated, minimal audio editor built with
//! [gpui-kit](https://github.com/longbridge/gpui-kit).
//!
//! Run it with no args to open a window, then open an audio file. Everything you
//! mark is written to a `<audio>.wavemark.json` sidecar, which the `wavemark`
//! CLI reads so an AI agent can consume it.

mod app;
mod audio;
mod state;

use gpui_kit::component::{ActiveTheme, Root, TitleBar};
use gpui_kit::*;

use crate::app::Editor;

/// `app_id` becomes the X11 `WM_CLASS` / Wayland app id. Without it the desktop
/// shell files the window under a generic class, so it gets no taskbar grouping
/// and no icon association.
const APP_ID: &str = "wavemark";
const WINDOW_TITLE: &str = "wavemark";

fn main() {
    gpui_kit::application().run(move |cx| {
        // Required before using any GPUI component feature.
        gpui_kit::init(cx);

        let bounds = Bounds::centered(None, size(px(1180.0), px(760.0)), cx);

        cx.spawn(async move |cx| {
            // `TitleBar::window_options()` is the base every window rendering a
            // `TitleBar` must use — it hands dragging and double-click-to-zoom
            // to the title bar itself. We additionally opt into *client-side*
            // decorations, which is what makes the window controllable at all:
            // under Wayland gpui hands you a bare surface and expects the app to
            // draw its own move / zoom / close affordances.
            cx.open_window(
                WindowOptions {
                    titlebar: Some(TitlebarOptions {
                        title: Some(WINDOW_TITLE.into()),
                        ..TitleBar::title_bar_options()
                    }),
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    // Below this the toolbar wraps and the annotation panel
                    // collapses, neither of which the user can recover from.
                    window_min_size: Some(size(px(760.0), px(560.0))),
                    app_id: Some(APP_ID.to_string()),
                    window_decorations: Some(WindowDecorations::Client),
                    ..TitleBar::window_options()
                },
                |window, cx| {
                    let view = cx.new(|_| Editor::new());
                    // The first level of a window must be a Root.
                    cx.new(|cx| Root::new(view, window, cx).bg(cx.theme().background))
                },
            )
            .expect("Failed to open window");
        })
        .detach();
    });
}
