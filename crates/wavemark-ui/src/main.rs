//! wavemark GUI — a GPU-accelerated, minimal audio editor built with
//! [gpui-kit](https://github.com/longbridge/gpui-kit).
//!
//! Run it with no args to open a window, then open an audio file. Everything you
//! mark is written to a `<audio>.wavemark.json` sidecar, which the `wavemark`
//! CLI reads so an AI agent can consume it.

mod app;
mod state;

use gpui_kit::component::{ActiveTheme, Root};
use gpui_kit::*;

use crate::app::Editor;

fn main() {
    gpui_kit::application().run(move |cx| {
        // Required before using any GPUI component feature.
        gpui_kit::init(cx);

        cx.spawn(async move |cx| {
            cx.open_window(WindowOptions::default(), |window, cx| {
                let view = cx.new(|_| Editor::new());
                // The first level of a window must be a Root.
                cx.new(|cx| Root::new(view, window, cx).bg(cx.theme().background))
            })
            .expect("Failed to open window");
        })
        .detach();
    });
}
