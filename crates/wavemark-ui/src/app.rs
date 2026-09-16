//! The wavemark editor view.
//!
//! Layout, top to bottom:
//!
//! ```text
//! ┌─ title bar ──── app name · file ───────────── [─][□][✕] ─┐
//! ├─ toolbar ────── grouped icon buttons ─────────────────────┤
//! │  time ruler                                               │
//! │  big waveform canvas        (drag = select a range)       │
//! │  overview bar  (whole file + viewport, click = jump)      │
//! ├─ annotation panel ─── list · compose ─────────────────────┤
//! └─ status bar ──── format · selection · message ────────────┘
//! ```
//!
//! The waveform is drawn as a row of thin `div`s whose heights come from the
//! peak data, so it is genuinely GPU-composited by GPUI rather than rasterised
//! into a bitmap.

use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::base::input::InputState;
use gpui_kit::component::button::*;
use gpui_kit::component::input::Input;
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use gpui_kit::component::*;
use gpui_kit::prelude::{FluentBuilder as _, StatefulInteractiveElement as _};
use gpui_kit::*;

use crate::state::AppState;
use wavemark_core::silence::SilenceOptions;

/// Shorten `s` to at most `max` chars, dropping the middle so the tail — which
/// holds the extension — survives: `/very/long/path…/clip.wav`.
fn ellipsize(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub(1); // room for the ellipsis
    let tail = (keep / 3).clamp(4, keep);
    let head = keep - tail;
    let head: String = s.chars().take(head).collect();
    let tail: String = s.chars().skip(n - tail).collect();
    format!("{head}…{tail}")
}

/// A timestamp for the ruler, at a precision that suits the current zoom.
fn stamp(t: f64, span: f64) -> String {
    if span < 2.0 {
        format!("{t:.3}")
    } else if span < 60.0 {
        format!("{t:.2}")
    } else {
        let m = (t / 60.0).floor() as u32;
        format!("{}:{:04.1}", m, t - m as f64 * 60.0)
    }
}

/// Vertical rule used to group toolbar buttons.
fn divider(color: Hsla) -> impl IntoElement {
    div().w(px(1.0)).h(px(18.0)).bg(color)
}

/// A compact icon-only toolbar button. Shortcuts live in the tooltip rather
/// than in a permanent hint line, which keeps the chrome quiet.
fn tool(id: &'static str, icon: IconName, tip: &str) -> Button {
    Button::new(id)
        .icon(icon)
        .compact()
        .ghost()
        .tooltip(tip.to_string())
}

/// The same button, outlined — used for toggles that are currently on, since
/// gpui-component's `Button` has no `selected` state of its own.
fn tool_on(id: &'static str, icon: IconName, tip: &str) -> Button {
    Button::new(id)
        .icon(icon)
        .compact()
        .outline()
        .tooltip(tip.to_string())
}

pub struct Editor {
    pub state: AppState,
    /// Created lazily — `InputState::new` needs a `Window`.
    input: Option<Entity<InputState>>,
    /// Volume slider. Also lazily created, and driven by rodio rather than by
    /// the waveform, so it has to be a real widget and not a label.
    volume: Option<Entity<SliderState>>,
    /// True while the user is dragging the playhead along the ruler.
    scrubbing: bool,
    /// Guards the playback ticker so we only ever run one.
    timer_running: bool,
}

impl Editor {
    pub fn new() -> Self {
        Self {
            state: AppState::default(),
            input: None,
            volume: None,
            scrubbing: false,
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
    ///
    /// The timer no longer *advances* the playhead — `poll_playback` reads the
    /// output device's own position, so the cursor and the sound can't drift
    /// apart. All this does is ask ~30×/second.
    fn ensure_timer(&mut self, cx: &mut Context<Self>) {
        if self.timer_running {
            return;
        }
        self.timer_running = true;
        let period = Duration::from_millis(33);
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(period).await;
            let playing = this.update(cx, |this, cx| {
                this.state.poll_playback();
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

    /// Global keyboard shortcuts.
    ///
    /// Two rules keep this from fighting the text input:
    ///
    /// 1. Anything with a modifier is always ours (undo, redo).
    /// 2. Bare letter keys are only ours when the annotation composer is not
    ///    focused — otherwise typing "kind" would jump around and delete things.
    fn on_key_down(&mut self, e: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let m = e.keystroke.modifiers;
        let key = e.keystroke.key.as_str();
        let accel = m.platform || m.control;

        if accel && key == "z" {
            let what = if m.shift { "redo" } else { "undo" };
            let ok = if m.shift {
                self.state.redo()
            } else {
                self.state.undo()
            };
            self.state.status = if ok {
                format!(
                    "{what} — {}/{} undo left",
                    self.state.can_undo(),
                    self.state.can_redo()
                )
            } else {
                format!("nothing to {what}")
            };
            cx.notify();
            return;
        }
        if m.control || m.alt || m.platform || m.function {
            return;
        }

        // Don't steal keys while the user is writing a note.
        if let Some(input) = &self.input {
            if input.read(cx).focus_handle(cx).is_focused(window) {
                return;
            }
        }

        match key {
            "space" => {
                if self.state.playing {
                    self.state.pause();
                } else {
                    self.state.play();
                    self.ensure_timer(cx);
                }
            }
            "escape" => self.state.stop(),
            "l" => self.state.toggle_loop(),
            "m" => self.state.toggle_mute(),
            "home" => self.state.seek(0.0),
            "end" => self.state.seek(self.state.duration),
            "left" => {
                let step = if m.shift { 1.0 } else { 0.05 };
                self.state.seek(self.state.playhead - step);
            }
            "right" => {
                let step = if m.shift { 1.0 } else { 0.05 };
                self.state.seek(self.state.playhead + step);
            }
            "+" | "=" => self.state.zoom_at(0.5, 1.5),
            "-" => self.state.zoom_at(0.5, 1.0 / 1.5),
            "j" => {
                if !self.state.step_annotation(1) {
                    self.state.status = "no annotations".into();
                }
            }
            "k" => {
                if !self.state.step_annotation(-1) {
                    self.state.status = "no annotations".into();
                }
            }
            "a" => {
                self.add_annotation(window, cx);
                return; // add_annotation already notifies
            }
            "e" => self.export(cx),
            "delete" | "backspace" => match self.state.remove_annotation_at_selection() {
                Ok(()) => self.state.status = "annotation deleted".into(),
                Err(err) => self.state.status = format!("{err}"),
            },
            _ => return,
        }
        cx.notify();
    }

    // ---- pieces -------------------------------------------------------------

    /// Custom title bar. Owns window dragging, double-click-to-zoom and (via
    /// `TitleBar`) the minimize / maximize / close controls — none of which
    /// exist when the window manager hands us a bare client-decorated surface.
    ///
    /// The current file lives here rather than in the toolbar: it is the one
    /// string that grows without bound, and the title bar has room to truncate
    /// it without shoving controls off screen.
    fn titlebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let file = self
            .state
            .audio_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "no file".to_string());

        TitleBar::new().child(
            div()
                .h_flex()
                .flex_1()
                .items_center()
                .gap_2()
                .min_w_0()
                .child(
                    Icon::new(IconName::AudioWaveform)
                        .small()
                        .text_color(cx.theme().muted_foreground),
                )
                .child(div().text_sm().font_semibold().child("wavemark"))
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .overflow_hidden()
                        .child(ellipsize(&file, 64)),
                ),
        )
    }

    /// Grouped icon buttons. Groups are separated by a rule so the eye can
    /// find transport vs. editing vs. view without reading labels.
    fn toolbar(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let line = cx.theme().border;
        let playing = self.state.playing;
        let looping = self.state.looping;
        let muted = self.state.muted;
        let can_undo = self.state.can_undo();
        let can_redo = self.state.can_redo();
        let has_audio = self.state.has_audio();

        let transport = if playing {
            tool("play-pause", IconName::Pause, "Pause (Space)")
        } else {
            tool("play-pause", IconName::Play, "Play (Space)")
        };

        // Loop is a toggle with no `selected` styling available, so it flips
        // between ghost and outlined instead.
        let loop_btn = if looping {
            tool_on("loop", IconName::Repeat, "Loop on — click to turn off (L)")
        } else {
            tool("loop", IconName::Repeat, "Loop the selection (L)")
        };

        let mute_btn = if muted {
            tool_on("mute", IconName::VolumeX, "Unmute (M)")
        } else {
            tool("mute", IconName::Volume2, "Mute (M)")
        };

        div()
            .h_flex()
            .gap_1()
            .px_2()
            .py_1()
            .w_full()
            .items_center()
            .flex_shrink_0()
            .border_b_1()
            .border_color(line)
            // file
            .child(
                tool("open", IconName::FolderOpen, "Open…").on_click(cx.listener(
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
                )),
            )
            .child(divider(line))
            // transport
            .child(
                tool("rewind", IconName::Rewind, "Back to start (Home)")
                    .disabled(!has_audio)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.state.stop();
                        cx.notify();
                    })),
            )
            .child(transport.on_click(cx.listener(|this, _, _, cx| {
                if this.state.playing {
                    this.state.pause();
                } else {
                    this.state.play();
                    this.ensure_timer(cx);
                }
                cx.notify();
            })))
            .child(
                tool("stop", IconName::Square, "Stop (Esc)")
                    .disabled(!has_audio)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.state.stop();
                        cx.notify();
                    })),
            )
            .child(
                loop_btn
                    .disabled(!has_audio)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.state.toggle_loop();
                        cx.notify();
                    })),
            )
            .child(divider(line))
            // editing
            .child(
                tool("undo", IconName::Undo2, "Undo (Ctrl+Z)")
                    .disabled(!can_undo)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if !this.state.undo() {
                            this.state.status = "nothing to undo".into();
                        }
                        cx.notify();
                    })),
            )
            .child(
                tool("redo", IconName::Redo2, "Redo (Ctrl+Shift+Z)")
                    .disabled(!can_redo)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if !this.state.redo() {
                            this.state.status = "nothing to redo".into();
                        }
                        cx.notify();
                    })),
            )
            .child(divider(line))
            // view
            .child(
                tool("zoom-in", IconName::ZoomIn, "Zoom in (+)")
                    .disabled(!has_audio)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.state.zoom_at(0.5, 2.0);
                        cx.notify();
                    })),
            )
            .child(
                tool("zoom-out", IconName::ZoomOut, "Zoom out (−)")
                    .disabled(!has_audio)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.state.zoom_at(0.5, 0.5);
                        cx.notify();
                    })),
            )
            .child(
                tool("fit", IconName::Maximize2, "Fit whole file")
                    .disabled(!has_audio)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.state.view_start = 0.0;
                        this.state.view_end = this.state.duration;
                        cx.notify();
                    })),
            )
            .child(divider(line))
            // marking
            .child(
                tool("silence", IconName::AudioLines, "Mark silence")
                    .disabled(!has_audio)
                    .on_click(cx.listener(|this, _, _, cx| {
                        let opts = SilenceOptions::default();
                        match this.state.add_silence_marks(&opts) {
                            Ok(0) => this.state.status = "no silence found".into(),
                            Ok(n) => {
                                this.state.status = format!("added {n} auto:silence annotation(s)")
                            }
                            Err(e) => this.state.status = format!("{e}"),
                        }
                        cx.notify();
                    })),
            )
            .child(divider(line))
            // export
            .child(
                tool("export", IconName::Download, "Export selection (E)")
                    .disabled(!has_audio)
                    .on_click(cx.listener(|this, _, _, cx| this.export(cx))),
            )
            // Keep the groups left-aligned as the window grows.
            .child(div().flex_1())
            // Output level. Parked at the far right so it reads as a playback
            // control rather than an editing one, and so it never competes with
            // the groups above for space.
            .child(
                mute_btn
                    .disabled(!has_audio)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.state.toggle_mute();
                        cx.notify();
                    })),
            )
            .child(match &self.volume {
                Some(v) => div().w(px(72.0)).px_1().child(Slider::new(v)),
                None => div().w(px(72.0)),
            })
    }

    /// Tick marks and timestamps for the visible window. Without this the
    /// waveform is a shape with no scale on it.
    ///
    /// It doubles as the scrub strip: dragging here moves the playhead, which
    /// is the one transport gesture that has nowhere else to live — the big
    /// canvas is owned by range selection.
    fn ruler(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let span = (self.state.view_end - self.state.view_start).max(f64::EPSILON);
        let ticks = 8usize;

        let mut row = div()
            .relative()
            .w_full()
            .h(px(18.0))
            .flex_shrink_0()
            .border_b_1()
            .border_color(cx.theme().border)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &MouseDownEvent, window, cx| {
                    let f = this.frac_of(window, e.position.x);
                    this.scrubbing = true;
                    this.state.seek(this.state.time_at_frac(f));
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, window, cx| {
                if this.scrubbing {
                    let f = this.frac_of(window, e.position.x);
                    this.state.seek(this.state.time_at_frac(f));
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| {
                    this.scrubbing = false;
                    cx.notify();
                }),
            );

        for i in 0..=ticks {
            let f = i as f32 / ticks as f32;
            row = row.child(
                div().absolute().top_0().left(relative(f)).child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(stamp(self.state.time_at_frac(f as f64), span)),
                ),
            );
        }
        row
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
                    // A drag here selects a range, so it wins over any scrub
                    // that began on the ruler and wandered down.
                    this.scrubbing = false;
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
            .flex_shrink_0()
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

    /// Annotation list + composer.
    fn panel(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self.state.selection;
        let anns: Vec<_> = self
            .state
            .session
            .as_ref()
            .map(|s| {
                s.annotations
                    .iter()
                    .map(|a| (a.id.clone(), a.range, a.text.clone(), a.tags.clone()))
                    .collect()
            })
            .unwrap_or_default();

        let mut list = div()
            // An id is what makes the div stateful, which is what allows it to
            // own a scroll offset.
            .id("annotation-list")
            .v_flex()
            .gap_1()
            .flex_1()
            // Scroll rather than clip: with a few dozen annotations the old
            // fixed-height list silently hid everything past the fold.
            .overflow_y_scroll()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("{} annotations", anns.len())),
            );

        for (i, (id, range, text, tags)) in anns.into_iter().enumerate() {
            let go_id = id.clone();
            let del_id = id.clone();
            let is_current = selected
                .map(|s| (s.start - range.start).abs() < 1e-6 && (s.end - range.end).abs() < 1e-6)
                .unwrap_or(false);
            // Auto-generated marks carry no text, so fall back to their tag —
            // otherwise they render as unexplained blank rows.
            let label = if text.is_empty() {
                tags.join(", ")
            } else {
                text
            };

            list = list.child(
                div()
                    .h_flex()
                    .gap_2()
                    .items_center()
                    .py_0p5()
                    .px_1()
                    .rounded(cx.theme().radius)
                    .when(is_current, |this| this.bg(cx.theme().accent.opacity(0.15)))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("{:.3}–{:.3}", range.start, range.end)),
                    )
                    .child(div().flex_1().min_w_0().text_sm().child(label))
                    .child(
                        Button::new(("go", i))
                            .icon(IconName::Crosshair)
                            .compact()
                            .ghost()
                            .tooltip("Go to")
                            .on_click(cx.listener(move |this, _, _, cx| {
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
                            })),
                    )
                    .child(
                        Button::new(("del", i))
                            .icon(IconName::Trash)
                            .compact()
                            .ghost()
                            .tooltip("Delete")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                _ = this.state.remove_annotation(&del_id);
                                cx.notify();
                            })),
                    ),
            );
        }

        let compose = if let Some(state) = self.input.clone() {
            div()
                .w(px(300.0))
                .v_flex()
                .gap_2()
                .child(Input::new(&state))
                .child(
                    Button::new("add")
                        .primary()
                        .label("Add annotation")
                        .tooltip("Save a note over the selected range (A)")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.add_annotation(window, cx)),
                        ),
                )
        } else {
            div().w(px(300.0))
        };

        div()
            .h_flex()
            .gap_3()
            .p_2()
            .w_full()
            .h(px(170.0))
            .flex_shrink_0()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(list)
            .child(compose)
    }

    /// Bottom status bar: what the file is, what is selected, what just
    /// happened. Kept out of the toolbar so a long path can never push the
    /// buttons off the window.
    fn status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;

        let format = self
            .state
            .audio
            .as_ref()
            .map(|a| {
                let ch = match a.channels {
                    1 => "mono".to_string(),
                    2 => "stereo".to_string(),
                    n => format!("{n}ch"),
                };
                format!("{} Hz · {} · {:.2} s", a.sample_rate, ch, a.duration_sec)
            })
            .unwrap_or_else(|| "no file".to_string());

        let sel = self
            .state
            .selection
            .map(|s| format!("sel {:.3} – {:.3} ({:.3}s)", s.start, s.end, s.duration()))
            .unwrap_or_else(|| "no selection".to_string());

        // Where we are, and where the sound is going. The device rate matters
        // because rodio resamples to it: if it differs from the file rate, that
        // is the honest explanation for anything that sounds slightly off.
        let head = match self.state.output_rate() {
            Some(rate) => format!("▶ {:.2} s · out {} Hz", self.state.playhead, rate),
            None => format!("▶ {:.2} s", self.state.playhead),
        };

        div()
            .h_flex()
            .gap_3()
            .px_2()
            .py_1()
            .w_full()
            .items_center()
            .flex_shrink_0()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(div().text_xs().text_color(muted).child(format))
            .child(divider(cx.theme().border))
            .child(div().text_xs().text_color(muted).child(sel))
            .child(divider(cx.theme().border))
            .child(div().text_xs().text_color(muted).child(head))
            .child(divider(cx.theme().border))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_xs()
                    .text_color(muted)
                    .child(ellipsize(&self.state.status, 90)),
            )
    }
}

impl Render for Editor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // InputState needs a Window, so build it on first render.
        if self.input.is_none() {
            self.input = Some(cx.new(|cx| InputState::new(window, cx)));
        }
        if self.volume.is_none() {
            let v = self.state.volume;
            let slider = cx.new(|_| {
                SliderState::new()
                    .min(0.0)
                    .max(1.5)
                    .step(0.01)
                    .default_value(v)
            });
            // The slider owns the number; we push it into rodio. `Change` fires
            // during the drag, `Release` once at the end — both mean the same
            // thing to us, so there is no need to debounce.
            cx.subscribe(&slider, |this, _, event: &SliderEvent, cx| {
                let value = match event {
                    SliderEvent::Change(v) | SliderEvent::Release(v) => v.end(),
                };
                this.state.set_volume(value);
                cx.notify();
            })
            .detach();
            self.volume = Some(slider);
        }
        div()
            .v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_key_down(cx.listener(Self::on_key_down))
            // Safety net: a scrub that ends outside the ruler would otherwise
            // leave the playhead glued to the cursor.
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.scrubbing = false),
            )
            .child(self.titlebar(cx))
            .child(self.toolbar(window, cx))
            .child(self.ruler(cx))
            .child(self.waveform(window, cx))
            .child(self.overview(window, cx))
            .child(self.panel(window, cx))
            .child(self.status_bar(cx))
    }
}
