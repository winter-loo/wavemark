//! Pure editor logic — no GUI framework types here.
//!
//! Keeping all of this free of gpui means it is unit-testable and portable. The
//! GUI layer ([`crate::app`]) only reads from this state and forwards events.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use wavemark_core::audio::{decode, export_range, DecodedAudio};
use wavemark_core::silence::{detect_silence, SilenceOptions};
use wavemark_core::{Annotation, Peaks, Session, TimeRange};

/// Peak resolution stored for the whole file. 100 buckets/sec is plenty of
/// detail; both the main canvas and the overview bar down-sample from this.
const PEAKS_PER_SECOND: usize = 100;

/// How many bars the main waveform canvas renders. Sized to stay smooth while
/// the playhead is animating.
pub const MAIN_BARS: usize = 600;
/// How many bars the bottom overview bar renders.
pub const OVERVIEW_BARS: usize = 400;

/// Depth of the undo stack. Sessions are small (a few KB of JSON), so 50
/// snapshots is nothing — and generous undo is what makes a marking tool
/// pleasant to use for an hour at a time.
pub const MAX_UNDO: usize = 50;

pub struct AppState {
    pub audio: Option<DecodedAudio>,
    pub peaks: Option<Peaks>,
    pub audio_path: Option<PathBuf>,
    pub session_path: Option<PathBuf>,
    pub session: Option<Session>,

    /// Visible time window, in seconds.
    pub view_start: f64,
    pub view_end: f64,
    pub duration: f64,

    pub selection: Option<TimeRange>,
    /// Anchor while the user is dragging out a new selection.
    pub drag_from: Option<f64>,

    pub playhead: f64,
    pub playing: bool,

    /// Snapshots of `session` before each mutation. Undo/redo only ever tracks
    /// the *session* — never audio samples, which would be absurd.
    undo_stack: Vec<Session>,
    redo_stack: Vec<Session>,

    pub status: String,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            audio: None,
            peaks: None,
            audio_path: None,
            session_path: None,
            session: None,
            view_start: 0.0,
            view_end: 0.0,
            duration: 0.0,
            selection: None,
            drag_from: None,
            playhead: 0.0,
            playing: false,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            status: "open an audio file to begin".into(),
        }
    }
}

impl AppState {
    // ---- loading ------------------------------------------------------------

    /// Decode an audio file, build peaks, and load (or create) its sidecar
    /// session. This is what makes the GUI and CLI share state.
    pub fn open(&mut self, path: &Path) -> Result<()> {
        let audio = decode(path).with_context(|| format!("decoding {}", path.display()))?;
        let peaks = audio.peaks((audio.duration_sec * PEAKS_PER_SECOND as f64).ceil() as usize);
        self.duration = audio.duration_sec;
        self.audio = Some(audio);
        self.peaks = Some(peaks);
        self.audio_path = Some(path.to_path_buf());
        self.session_path = Session::resolve_sidecar(path);
        self.view_start = 0.0;
        self.view_end = self.duration;
        self.playhead = 0.0;
        self.selection = None;
        self.undo_stack.clear();
        self.redo_stack.clear();

        // Load the sidecar if it exists, otherwise start a fresh session. The
        // sidecar is how annotations reach the CLI.
        self.session = match &self.session_path {
            Some(p) if p.is_file() => Session::load(p).ok(),
            _ => None,
        };
        if self.session.is_none() {
            let d = self.duration;
            let (sr, ch) = self
                .audio
                .as_ref()
                .map(|a| (a.sample_rate, a.channels))
                .unwrap_or((0, 0));
            self.session = Some(Session::new(path.to_string_lossy().to_string(), d, sr, ch));
        }
        self.status = format!("loaded {}", path.display());
        Ok(())
    }

    pub fn has_audio(&self) -> bool {
        self.audio.is_some()
    }

    // ---- waveform -----------------------------------------------------------

    /// Bar heights in `0..=1` covering `MAIN_BARS` bars across `[view_start,
    /// view_end]`. Returns an empty vec before a file is loaded.
    ///
    /// The GUI turns these into thin vertical bars; a symmetric waveform is
    /// produced by centring each bar, so height is `max - min` of the window.
    pub fn main_bars(&self) -> Vec<f32> {
        self.bars(MAIN_BARS, self.view_start, self.view_end)
    }

    /// Same idea for the bottom overview bar — always the whole file.
    pub fn overview_bars(&self) -> Vec<f32> {
        self.bars(OVERVIEW_BARS, 0.0, self.duration)
    }

    /// Down-sample the stored peaks into `n` bars covering `[from, to]`.
    fn bars(&self, n: usize, from: f64, to: f64) -> Vec<f32> {
        let Some(peaks) = &self.peaks else {
            return Vec::new();
        };
        let total = peaks.buckets.len();
        if total == 0 || n == 0 || (to - from) <= 0.0 {
            return vec![0.0; n];
        }
        let full = self.duration.max(f64::EPSILON);
        let first = ((from / full) * total as f64).floor() as usize;
        let last = (((to / full) * total as f64).ceil() as usize)
            .max(first + 1)
            .min(total);

        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let b0 = first + ((last - first) * i) / n;
            let b1 = (first + ((last - first) * (i + 1)) / n)
                .max(b0 + 1)
                .min(total);
            let mut lo = f32::INFINITY;
            let mut hi = f32::NEG_INFINITY;
            for b in b0..b1.min(total) {
                let p = &peaks.buckets[b];
                if p.min < lo {
                    lo = p.min;
                }
                if p.max > hi {
                    hi = p.max;
                }
            }
            if !lo.is_finite() || !hi.is_finite() {
                out.push(0.0);
            } else {
                out.push((hi - lo).clamp(0.0, 1.0));
            }
        }
        out
    }

    // ---- viewport / zoom ----------------------------------------------------

    /// Map a 0..1 horizontal position on the main canvas to a time in seconds.
    #[must_use]
    pub fn time_at_frac(&self, frac: f64) -> f64 {
        let span = self.view_end - self.view_start;
        (self.view_start + frac * span).clamp(0.0, self.duration.max(0.0))
    }

    /// Inverse of [`Self::time_at_frac`].
    #[must_use]
    pub fn frac_at_time(&self, t: f64) -> f64 {
        let span = self.view_end - self.view_start;
        if span <= 0.0 {
            0.0
        } else {
            ((t - self.view_start) / span).clamp(0.0, 1.0)
        }
    }

    /// Zoom the view around `anchor_frac`, keeping that point stationary.
    pub fn zoom_at(&mut self, anchor_frac: f64, factor: f64) {
        if !self.has_audio() {
            return;
        }
        let anchor_t = self.time_at_frac(anchor_frac);
        let span = (self.view_end - self.view_start) / factor;
        let min_span = 0.02; // don't zoom in past 20ms of audio
        let span = span.clamp(min_span, self.duration.max(min_span));
        let mut start = anchor_t - anchor_frac * span;
        let mut end = start + span;
        if start < 0.0 {
            start = 0.0;
            end = span;
        }
        if end > self.duration {
            end = self.duration;
            start = (end - span).max(0.0);
        }
        self.view_start = start;
        self.view_end = end;
    }

    /// Centre the view on `t` (used when jumping from the overview bar or from
    /// an annotation in the list).
    pub fn focus(&mut self, t: f64) {
        let span = self.view_end - self.view_start;
        let start = (t - span / 2.0).clamp(0.0, (self.duration - span).max(0.0));
        self.view_start = start;
        self.view_end = start + span;
    }

    // ---- selection ----------------------------------------------------------

    pub fn begin_drag(&mut self, t: f64) {
        self.drag_from = Some(t);
        self.selection = Some(TimeRange::new(t, t));
    }

    pub fn update_drag(&mut self, t: f64) {
        if let Some(from) = self.drag_from {
            self.selection = Some(TimeRange::new(from, t));
        }
    }

    pub fn end_drag(&mut self, t: f64) {
        if let Some(from) = self.drag_from.take() {
            let r = TimeRange::new(from, t);
            // Ignore accidental clicks under 20ms — treat as "clear selection".
            self.selection = if r.duration() < 0.02 { None } else { Some(r) };
        }
    }

    /// Select and reveal an existing annotation.
    pub fn select_annotation(&mut self, a: &Annotation) {
        self.selection = Some(a.range);
        self.focus(a.range.start);
    }

    // ---- annotations --------------------------------------------------------

    /// Commit the draft as an annotation over the current selection.
    pub fn add_annotation(&mut self, text: String, tags: Vec<String>) -> Result<()> {
        let sel = self
            .selection
            .ok_or_else(|| anyhow!("select a range first"))?;
        self.push_undo();
        let session = self
            .session
            .as_mut()
            .ok_or_else(|| anyhow!("no session loaded"))?;
        let a = Annotation::new(sel, text).with_tags(tags);
        session.annotations.push(a);
        self.save_session()?;
        Ok(())
    }

    pub fn remove_annotation(&mut self, id: &str) -> Result<()> {
        self.push_undo();
        let session = self
            .session
            .as_mut()
            .ok_or_else(|| anyhow!("no session loaded"))?;
        session.remove_annotation(id);
        self.save_session()?;
        Ok(())
    }

    /// Remove whichever annotation covers the current selection. Used by the
    /// Delete key, which has no idea what ids are.
    pub fn remove_annotation_at_selection(&mut self) -> Result<()> {
        let sel = self.selection.ok_or_else(|| anyhow!("no selection"))?;
        let id = self
            .session
            .as_ref()
            .and_then(|s| {
                s.annotations
                    .iter()
                    .find(|a| a.range.overlaps(&sel))
                    .map(|a| a.id.clone())
            })
            .ok_or_else(|| anyhow!("no annotation covers the selection"))?;
        self.remove_annotation(&id)
    }

    /// Jump to the next (`delta > 0`) or previous (`delta < 0`) annotation,
    /// wrapping around at the ends. Returns false when there's nothing to do.
    pub fn step_annotation(&mut self, delta: isize) -> bool {
        let Some(session) = self.session.as_ref() else {
            return false;
        };
        if session.annotations.is_empty() || delta == 0 {
            return false;
        }
        let n = session.annotations.len() as isize;
        let cur = self.selection.map(|s| s.start).unwrap_or(self.playhead);

        // `position` / `rposition` assume sorted order, which merge_patches
        // and silence --write both maintain.
        let idx = if delta > 0 {
            session
                .annotations
                .iter()
                .position(|a| a.range.start > cur + 1e-6)
                .map(|i| i as isize)
                .unwrap_or(0)
        } else {
            session
                .annotations
                .iter()
                .rposition(|a| a.range.start < cur - 1e-6)
                .map(|i| i as isize)
                .unwrap_or(n - 1)
        };
        let idx = ((idx % n) + n) % n;

        let a = session.annotations[idx as usize].clone();
        self.select_annotation(&a);
        self.playhead = a.range.start;
        true
    }

    // ---- undo / redo --------------------------------------------------------

    /// Snapshot the session before a mutation. Any new edit clears redo, which
    /// is the behaviour everyone expects from every other editor.
    fn push_undo(&mut self) {
        if let Some(session) = &self.session {
            self.undo_stack.push(session.clone());
            if self.undo_stack.len() > MAX_UNDO {
                self.undo_stack.remove(0);
            }
            self.redo_stack.clear();
        }
    }

    #[must_use]
    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    #[must_use]
    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    /// Roll back one mutation. Restores the snapshot and persists, so the CLI
    /// sees the undone state immediately.
    pub fn undo(&mut self) -> bool {
        let Some(prev) = self.undo_stack.pop() else {
            return false;
        };
        if let Some(cur) = self.session.take() {
            self.redo_stack.push(cur);
        }
        self.session = Some(prev);
        let _ = self.save_session();
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo_stack.pop() else {
            return false;
        };
        if let Some(cur) = self.session.take() {
            self.undo_stack.push(cur);
        }
        self.session = Some(next);
        let _ = self.save_session();
        true
    }

    // ---- session ------------------------------------------------------------

    /// Persist to the sidecar `.wavemark.json`. This is the hand-off point to
    /// the CLI: `wavemark annotations` reads exactly this file.
    pub fn save_session(&mut self) -> Result<()> {
        let (path, session) = match (self.session_path.as_ref(), self.session.as_mut()) {
            (Some(p), Some(s)) => (p, s),
            _ => return Err(anyhow!("no session to save")),
        };
        if let Some(sel) = self.selection {
            session.last_selection = Some(sel);
        }
        session
            .save(path)
            .with_context(|| format!("saving {}", path.display()))
    }

    // ---- playback -----------------------------------------------------------

    pub fn play(&mut self) {
        if self.has_audio() {
            self.playing = true;
        }
    }

    pub fn pause(&mut self) {
        self.playing = false;
    }

    pub fn stop(&mut self) {
        self.playing = false;
        self.playhead = 0.0;
    }

    pub fn seek(&mut self, t: f64) {
        self.playhead = t.clamp(0.0, self.duration);
    }

    /// Advance the playhead by `dt` seconds; stops at the end of the file (or
    /// at the end of the selection when looping a selection).
    pub fn tick(&mut self, dt: f64) {
        if !self.playing {
            return;
        }
        self.playhead += dt;
        let limit = self.selection.map(|s| s.end).unwrap_or(self.duration);
        if self.playhead >= limit {
            self.playhead = self.selection.map(|s| s.start).unwrap_or(0.0);
        }
    }

    /// Keep the playhead in view while playing.
    pub fn follow_playhead(&mut self) {
        if self.playhead < self.view_start || self.playhead > self.view_end {
            self.focus(self.playhead);
        }
    }

    /// Run silence detection and add each run as an `auto:silence` annotation.
    ///
    /// Cheap enough to run interactively: it walks the ~100/second peak buckets
    /// we already computed for the waveform, not the raw PCM.
    pub fn add_silence_marks(&mut self, opts: &SilenceOptions) -> Result<usize> {
        let peaks = self
            .peaks
            .clone()
            .ok_or_else(|| anyhow!("no audio loaded"))?;
        let spans = detect_silence(&peaks, opts);
        if spans.is_empty() {
            return Ok(0);
        }
        self.push_undo();
        let session = self
            .session
            .as_mut()
            .ok_or_else(|| anyhow!("no session loaded"))?;
        for s in &spans {
            session.add_annotation(
                Annotation::new(TimeRange::new(s.start, s.end), String::new())
                    .with_tags(vec!["auto:silence".to_string()]),
            );
        }
        session.sort_annotations();
        let n = spans.len();
        self.save_session()?;
        Ok(n)
    }

    // ---- export -------------------------------------------------------------

    /// Export the current selection to `out` as WAV.
    pub fn export_selection(&self, out: &Path) -> Result<()> {
        let sel = self
            .selection
            .ok_or_else(|| anyhow!("select a range first"))?;
        let audio = self.audio.as_ref().ok_or_else(|| anyhow!("no audio"))?;
        export_range(
            &audio.samples,
            audio.sample_rate,
            audio.channels,
            sel,
            out,
            5.0,
        )?;
        Ok(())
    }
}
