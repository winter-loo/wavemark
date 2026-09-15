//! The wavemark editor view.
//!
//! Layout, top to bottom:
//!
//! ```text
//! ┌─ header ─── open file · transport · zoom ─────────────────┐
//! │  big waveform canvas  (drag = select a range)             │
//! │  overview bar  (whole file + viewport window, click = jump)│
//! └─ annotation panel ─── list · compose · export ────────────┘
//! ```
//!
//! The waveform is drawn as a row of thin `div`s whose heights come from the
//! peak data, so it is genuinely GPU-composited by GPUI rather than rasterised
//! into a bitmap.

use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::base::input::InputState;
use gpui_kit::component::button::*;
use gpui_kit::component::input::Input;
use gpui_kit::component::*;
use gpui_kit::*;

use crate::state::AppState;

pub struct Editor {
    pub state: AppState,
    /// Created lazily — `InputState::new` needs a `Window`.
    input: Option<Entity<InputState>>,
    /// Guards the playback ticker so we only ever run one.
    timer_running: bool,
}

impl Editor {
    pub fn new() -> Self {
        Self {
            state: AppState::default(),
            input: None,
            timer_running: false,
        }
    }

    /// Horizontal fraction (0..1) of a mouse position over a full-width canvas.
    fn frac_of(&self, window: &Window, x: Pixels) -> f64 {
        let w: f32 = window.viewport_size().width.into();
        let x: f32 = x.into();
        if w <= 0.0 {
            0.0
        } else {
            (x / w).clamp(0.0, 1.0) as f64
        }
    }

    fn open(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        match self.state.open(&path) {
            Ok(()) => self.state.status = format!("loaded {}", path.display()),
            Err(e) => self.state.status = format!("open failed: {e}"),
        }
        cx.notify();
    }

    fn add_annotation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = match &self.input {
            Some(i) => i.read(cx).value().to_string(),
            None => String::new(),
        };
        let text = text.trim().to_string();
        if text.is_empty() {
            self.state.status = "type some text first".into();
            cx.notify();
            return;
        }
        match self.state.add_annotation(text, Vec::new()) {
            Ok(()) => {
                self.state.status =
                    "annotation saved — now visible to `wavemark annotations`".into();
                if let Some(i) = &self.input {
                    i.update(cx, |s, cx| s.set_value("", window, cx));
                }
            }
            Err(e) => self.state.status = format!("could not save: {e}"),
        }
        cx.notify();
    }

    fn export(&mut self, cx: &mut Context<Self>) {
        let out = match self.state.audio_path.as_ref() {
            Some(p) => {
                let sel = self
                    .state
                    .selection
                    .unwrap_or_else(|| wavemark_core::TimeRange::new(0.0, self.state.duration));
                PathBuf::from(format!(
                    "{}-{:0.3}-{:0.3}.wav",
                    p.file_stem().and_then(|s| s.to_str()).unwrap_or("segment"),
                    sel.start,
                    sel.end
                ))
            }
            None => PathBuf::from("segment.wav"),
        };
        match self.state.export_selection(&out) {
            Ok(()) => self.state.status = format!("exported {}", out.display()),
            Err(e) => self.state.status = format!("export failed: {e}"),
        }
        cx.notify();
    }

    /// Drive the playhead while playing.
    fn ensure_timer(&mut self, cx: &mut Context<Self>) {
        if self.timer_running {
            return;
        }
        self.timer_running = true;
        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(33))
                .await;
            let playing = this.update(cx, |this, cx| {
                this.state.tick(0.033);
                this.state.follow_playhead();
                cx.notify();
                this.state.playing
            });
            match playing {
                Ok(false) | Err(_) => break,
                _ => {}
            }
        })
        .detach();
    }

    // ---- pieces -------------------------------------------------------------

    fn header(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let name = self
            .state
            .audio_path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "no file".into());
        let status = self.state.status.clone();
        let sel = self
            .state
            .selection
            .map(|s| format!("{:.3} – {:.3} s ({:.3}s)", s.start, s.end, s.duration()))
            .unwrap_or_else(|| "no selection".into());

        div()
            .h_flex()
            .gap_2()
            .p_2()
            .w_full()
            .items_center()
            .child(format!("{name} · {status}"))
            .child(div().flex_1())
            .child(format!("sel: {sel}"))
            .child(Button::new("open").label("Open…").on_click(cx.listener(
                |_this, _, _window, cx| {
                    let rx = cx.prompt_for_paths(PathPromptOptions {
                        files: true,
                        directories: false,
                        multiple: false,
                        prompt: None,
                    });
                    cx.spawn(async move |this, cx| {
                        if let Ok(Ok(Some(mut paths))) = rx.await {
                            if let Some(p) = paths.pop() {
                                _ = this.update(cx, |this, cx| this.open(p, cx));
                            }
                        }
                    })
                    .detach();
                },
            )))
            .child(
                Button::new("play")
                    .primary()
                    .label("Play")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.state.play();
                        this.ensure_timer(cx);
                        cx.notify();
                    })),
            )
            .child(
                Button::new("pause")
                    .label("Pause")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.state.pause();
                        cx.notify();
                    })),
            )
            .child(
                Button::new("stop")
                    .label("Stop")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.state.stop();
                        cx.notify();
                    })),
            )
            .child(
                Button::new("zoomin")
                    .label("Zoom +")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.state.zoom_at(0.5, 2.0);
                        cx.notify();
                    })),
            )
            .child(Button::new("zoomout").label("Zoom −").on_click(cx.listener(
                |this, _, _, cx| {
                    this.state.zoom_at(0.5, 0.5);
                    cx.notify();
                },
            )))
            .child(
                Button::new("fit")
                    .label("Fit")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.state.view_start = 0.0;
                        this.state.view_end = this.state.duration;
                        cx.notify();
                    })),
            )
    }

    /// The big waveform. Drag horizontally to select a range.
    fn waveform(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let bars = self.state.main_bars();
        let height = 160.0f32;

        let mut canvas = div()
            .id("waveform")
            .flex_1()
            .w_full()
            .min_h(px(height))
            .relative()
            .overflow_hidden()
            .bg(rgb(0x0b0f14))
            .h_flex()
            .items_center()
            .justify_center()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &MouseDownEvent, window, cx| {
                    let f = this.frac_of(window, e.position.x);
                    this.state.begin_drag(this.state.time_at_frac(f));
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, window, cx| {
                if this.state.drag_from.is_some() {
                    let f = this.frac_of(window, e.position.x);
                    this.state.update_drag(this.state.time_at_frac(f));
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, e: &MouseUpEvent, window, cx| {
                    let f = this.frac_of(window, e.position.x);
                    this.state.end_drag(this.state.time_at_frac(f));
                    cx.notify();
                }),
            );

        // The waveform itself: one bar per bucket, each sharing the width.
        for h in bars {
            let h_px = (h * height).max(1.0);
            canvas = canvas.child(div().flex_1().h(px(h_px)).bg(rgb(0x38bdf8)));
        }

        // Selection overlay.
        if let Some(sel) = self.state.selection {
            let a = self.state.frac_at_time(sel.start) as f32;
            let b = self.state.frac_at_time(sel.end) as f32;
            canvas = canvas.child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(relative(a))
                    .w(relative((b - a).max(0.001)))
                    .bg(rgba(0x22d3ee55)),
            );
        }

        // Annotation markers along the top edge.
        let anns: Vec<f32> = self
            .state
            .session
            .as_ref()
            .map(|s| {
                s.annotations
                    .iter()
                    .map(|a| self.state.frac_at_time(a.range.start) as f32)
                    .collect()
            })
            .unwrap_or_default();
        for (i, f) in anns.into_iter().enumerate() {
            canvas = canvas.child(
                div()
                    .id(("ann", i))
                    .absolute()
                    .top_0()
                    .h(px(10.0))
                    .w(px(2.0))
                    .left(relative(f))
                    .bg(rgb(0xf59e0b)),
            );
        }

        // Playhead.
        let pf = self.state.frac_at_time(self.state.playhead) as f32;
        canvas.child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(relative(pf))
                .w(px(1.0))
                .bg(rgb(0xef4444)),
        )
    }

    /// The bottom overview bar: the whole file at a glance, with the main
    /// canvas' viewport drawn as a window on top.
    fn overview(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let bars = self.state.overview_bars();
        let height = 56.0f32;
        let a = if self.state.duration > 0.0 {
            (self.state.view_start / self.state.duration) as f32
        } else {
            0.0
        };
        let b = if self.state.duration > 0.0 {
            (self.state.view_end / self.state.duration) as f32
        } else {
            1.0
        };
        let pf = if self.state.duration > 0.0 {
            (self.state.playhead / self.state.duration) as f32
        } else {
            0.0
        };

        let mut bar = div()
            .id("overview")
            .w_full()
            .h(px(height))
            .relative()
            .overflow_hidden()
            .bg(rgb(0x0f151c))
            .h_flex()
            .items_center()
            .justify_center()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &MouseDownEvent, window, cx| {
                    let f = this.frac_of(window, e.position.x);
                    let t = f * this.state.duration;
                    this.state.focus(t);
                    this.state.seek(t);
                    cx.notify();
                }),
            );

        for h in bars {
            let h_px = (h * height).max(1.0);
            bar = bar.child(div().flex_1().h(px(h_px)).bg(rgb(0x64748b)));
        }

        // Viewport window + playhead.
        bar.child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(relative(a))
                .w(relative((b - a).max(0.001)))
                .border_1()
                .border_color(rgb(0x22d3ee)),
        )
        .child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(relative(pf))
                .w(px(1.0))
                .bg(rgb(0xef4444)),
        )
    }

    /// Annotation list + composer + export.
    fn panel(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let anns: Vec<_> = self
            .state
            .session
            .as_ref()
            .map(|s| {
                s.annotations
                    .iter()
                    .map(|a| (a.id.clone(), a.range, a.text.clone()))
                    .collect()
            })
            .unwrap_or_default();

        let mut list = div()
            .v_flex()
            .gap_1()
            .flex_1()
            .overflow_hidden()
            .child(format!("{} annotations", anns.len()));

        for (i, (id, range, text)) in anns.into_iter().enumerate() {
            let go_id = id.clone();
            let del_id = id.clone();
            list = list.child(
                div()
                    .h_flex()
                    .gap_2()
                    .items_center()
                    .child(format!("{:.3}–{:.3}s", range.start, range.end))
                    .child(div().flex_1().child(text))
                    .child(Button::new(("go", i)).label("Go to").on_click(cx.listener(
                        move |this, _, _, cx| {
                            if let Some(a) = this
                                .state
                                .session
                                .as_ref()
                                .and_then(|s| s.annotation(&go_id))
                                .cloned()
                            {
                                this.state.select_annotation(&a);
                            }
                            cx.notify();
                        },
                    )))
                    .child(
                        Button::new(("del", i))
                            .label("Delete")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                _ = this.state.remove_annotation(&del_id);
                                cx.notify();
                            })),
                    ),
            );
        }

        let compose = if let Some(state) = self.input.clone() {
            div()
                .w(px(320.0))
                .v_flex()
                .gap_2()
                .child(Input::new(&state))
        } else {
            div().w(px(320.0))
        };

        div()
            .h_flex()
            .gap_2()
            .p_2()
            .w_full()
            .min_h(px(140.0))
            .child(list)
            .child(
                compose.child(
                    Button::new("add")
                        .primary()
                        .label("Add annotation")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.add_annotation(window, cx)),
                        ),
                ),
            )
            .child(
                Button::new("export")
                    .label("Export selection")
                    .on_click(cx.listener(|this, _, _, cx| this.export(cx))),
            )
    }
}

impl Render for Editor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // InputState needs a Window, so build it on first render.
        if self.input.is_none() {
            self.input = Some(cx.new(|cx| InputState::new(window, cx)));
        }
        div()
            .v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.header(window, cx))
            .child(self.waveform(window, cx))
            .child(self.overview(window, cx))
            .child(self.panel(window, cx))
    }
}
