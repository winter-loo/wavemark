//! Real audio output.
//!
//! Until this existed the transport was decorative: `AppState::tick` nudged a
//! playhead along on a 33 ms timer and not one sample ever reached a sound
//! card. This module owns the `rodio` output device, the playback clock, and
//! therefore the only trustworthy answer to "what time is it?" while audio is
//! running.
//!
//! Three decisions are worth knowing about:
//!
//! * **One `Player` per armed window, not one for the app.** `Player::clear`
//!   sleeps until the old sound drains, which on the UI thread could block for
//!   minutes. Dropping a `Player` is non-blocking, so re-arming just builds a
//!   new one and lets the old one go.
//! * **The source is bounded, not truncated by a timer.** `PcmSource` covers
//!   `[start, end)` and simply runs out, so "play the selection" ends exactly
//!   at the selection instead of ~33 ms past it.
//! * **The playhead reads from the device, not from wall clock.** `get_pos()`
//!   reflects samples actually consumed, so a stalled or resampled stream drags
//!   the cursor with it instead of drifting ahead of the sound.

use std::sync::Arc;
use std::time::Duration;

use rodio::source::SeekError;
use rodio::{ChannelCount, DeviceSinkBuilder, MixerDeviceSink, Player, Sample, SampleRate, Source};

/// The decoded PCM we are currently able to play.
struct Clip {
    /// Shared with [`crate::state::AppState`] — never copied.
    samples: Arc<Vec<f32>>,
    channels: ChannelCount,
    rate: SampleRate,
    /// `samples.len() / channels / rate`, in seconds.
    duration: f64,
}

/// A [`rodio::Source`] over `[start, end)` of the decoded PCM.
///
/// `Sample` is `f32` for as long as rodio's `64bit` feature stays off, which
/// matches `DecodedAudio::samples`.
pub struct PcmSource {
    samples: Arc<Vec<f32>>,
    start: usize,
    /// Index of the next sample to yield.
    pos: usize,
    end: usize,
    channels: ChannelCount,
    rate: SampleRate,
}

impl PcmSource {
    /// Build a source covering `[from, to)` seconds of `clip`.
    fn new(clip: &Clip, from: f64, to: f64) -> Self {
        let ch = clip.channels.get() as usize;
        let total = clip.samples.len();
        // Snap to whole frames. Starting mid-frame would rotate the channels —
        // a stereo clip would come out with left and right swapped.
        let start = (to_sample(from, clip.rate) * ch).min(total);
        let end = (to_sample(to, clip.rate) * ch).clamp(start, total);
        Self {
            samples: clip.samples.clone(),
            start,
            pos: start,
            end,
            channels: clip.channels,
            rate: clip.rate,
        }
    }
}

fn to_sample(t: f64, rate: SampleRate) -> usize {
    if t <= 0.0 {
        0
    } else {
        (t * rate.get() as f64).round() as usize
    }
}

impl Iterator for PcmSource {
    type Item = Sample;

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.end {
            return None;
        }
        let s = self.samples[self.pos];
        self.pos += 1;
        Some(s)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let n = self.end.saturating_sub(self.pos);
        (n, Some(n))
    }
}

impl Source for PcmSource {
    /// `None` means "one constant-format span forever", which is exactly what a
    /// block of PCM is. It also keeps `TrackPosition` counting in absolute
    /// samples instead of restarting per span.
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> ChannelCount {
        self.channels
    }

    fn sample_rate(&self) -> SampleRate {
        self.rate
    }

    fn total_duration(&self) -> Option<Duration> {
        let frames = (self.end - self.start) as f64 / self.channels.get() as f64;
        Some(Duration::from_secs_f64(frames / self.rate.get() as f64))
    }

    /// `pos` is relative to the start of the window, matching what
    /// [`Player::try_seek`] hands down after `TrackPosition` has accounted for
    /// the armed offset.
    fn try_seek(&mut self, pos: Duration) -> Result<(), SeekError> {
        let ch = self.channels.get() as usize;
        let frame = to_sample(pos.as_secs_f64(), self.rate);
        self.pos = (self.start + frame * ch).min(self.end);
        Ok(())
    }
}

/// Owns the output device and drives playback.
pub struct AudioEngine {
    /// The OS stream. Dropping it stops playback, so it outlives every player.
    sink: MixerDeviceSink,
    player: Option<Player>,
    clip: Option<Clip>,
    /// The window the current player was built for, in seconds.
    from: f64,
    to: f64,
    playing: bool,
    volume: f32,
    muted: bool,
}

impl AudioEngine {
    /// Open the default output device. Fails if the machine has none — a headless
    /// box, or a container with no ALSA/Pulse; callers surface the message.
    pub fn open() -> Result<Self, String> {
        let mut sink = DeviceSinkBuilder::open_default_sink().map_err(|e| e.to_string())?;
        // The drop message goes to stderr and reads like a crash. We drop the
        // sink exactly once, at app exit.
        sink.log_on_drop(false);
        Ok(Self {
            sink,
            player: None,
            clip: None,
            from: 0.0,
            to: 0.0,
            playing: false,
            volume: 1.0,
            muted: false,
        })
    }

    /// Sample rate the output device actually runs at — worth showing, because
    /// rodio resamples to it and a mismatch is why things sometimes sound off.
    pub fn output_rate(&self) -> u32 {
        self.sink.config().sample_rate().get()
    }

    /// Make `samples` playable. Takes the `Arc` the decoder already built, so a
    /// two-hour recording is not duplicated just to be audible.
    pub fn load(&mut self, samples: Arc<Vec<f32>>, channels: u16, rate: u32) {
        let channels = ChannelCount::new(channels.max(1)).unwrap_or(ChannelCount::MIN);
        let rate = SampleRate::new(rate.max(1)).unwrap_or(SampleRate::MIN);
        let frames = samples.len() as f64 / channels.get() as f64;
        let duration = frames / rate.get() as f64;
        self.player = None;
        self.playing = false;
        self.clip = Some(Clip {
            samples,
            channels,
            rate,
            duration,
        });
        self.from = 0.0;
        self.to = duration;
    }

    /// Build a player over `[from, to)` and leave it paused at `from`.
    fn arm(&mut self, from: f64, to: f64) {
        let Some(clip) = self.clip.as_ref() else {
            return;
        };
        let from = from.clamp(0.0, clip.duration);
        let to = to.clamp(from, clip.duration);
        let src = PcmSource::new(clip, from, to);
        let player = Player::connect_new(self.sink.mixer());
        player.set_volume(self.gain());
        player.append(src);
        player.pause();
        self.player = Some(player);
        self.from = from;
        self.to = to;
        self.playing = false;
    }

    /// True when the parked player already sits at `from`, so resuming is just
    /// an unpause rather than a rebuild.
    #[must_use]
    pub fn can_resume(&self, from: f64, to: f64) -> bool {
        match &self.player {
            Some(p) if !p.empty() => {
                (from - self.from).abs() <= 5e-4 && (to - self.to).abs() <= 5e-4
            }
            _ => false,
        }
    }

    /// Arm `[from, to)` and start immediately.
    pub fn start(&mut self, from: f64, to: f64) {
        self.arm(from, to);
        if let Some(p) = &self.player {
            p.play();
            self.playing = true;
        }
    }

    pub fn resume(&mut self) {
        if let Some(p) = &self.player {
            p.play();
            self.playing = true;
        }
    }

    pub fn pause(&mut self) {
        if let Some(p) = &self.player {
            p.pause();
        }
        self.playing = false;
    }

    /// Arm `[from, to)` and stay paused — used by stop/rewind and by seeking
    /// outside the armed window.
    pub fn park(&mut self, from: f64, to: f64) {
        self.arm(from, to);
    }

    /// Move the playhead. Seeks inside the armed window when possible; outside
    /// it we have to rebuild the source, and `to` becomes the new end.
    pub fn seek(&mut self, t: f64, to: f64) {
        let resume = self.playing;
        let inside = match &self.player {
            Some(p) => !p.empty() && t >= self.from - 1e-6 && t <= self.to + 1e-6,
            None => false,
        };
        if inside {
            // `try_seek` blocks until the audio thread next services the
            // source — ~5 ms — which is fine for a click or a scrub drag.
            if let Some(p) = &self.player {
                let _ = p.try_seek(Duration::from_secs_f64((t - self.from).max(0.0)));
            }
            return;
        }
        self.arm(t, to);
        if resume {
            if let Some(p) = &self.player {
                p.play();
                self.playing = true;
            }
        }
    }

    /// Where playback currently is, in seconds. This is the device's own
    /// position, so it is authoritative while playing.
    #[must_use]
    pub fn position(&self) -> f64 {
        match &self.player {
            Some(p) => (self.from + p.get_pos().as_secs_f64()).min(self.to),
            None => self.from,
        }
    }

    /// True once the armed window has run out. The caller decides whether that
    /// means stop or loop.
    #[must_use]
    pub fn finished(&self) -> bool {
        self.playing && self.player.as_ref().map_or(true, |p| p.empty())
    }

    pub fn set_volume(&mut self, v: f32) {
        self.volume = v.clamp(0.0, 1.5);
        self.apply_gain();
    }

    pub fn set_muted(&mut self, m: bool) {
        self.muted = m;
        self.apply_gain();
    }

    fn gain(&self) -> f32 {
        if self.muted {
            0.0
        } else {
            self.volume
        }
    }

    fn apply_gain(&self) {
        if let Some(p) = &self.player {
            p.set_volume(self.gain());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 4 frames of stereo at 4 Hz — small enough to reason about by hand.
    fn clip() -> Clip {
        Clip {
            samples: Arc::new((0..8).map(|i| i as f32).collect()),
            channels: ChannelCount::new(2).unwrap(),
            rate: SampleRate::new(4).unwrap(),
            duration: 1.0,
        }
    }

    fn collect(src: &mut PcmSource) -> Vec<f32> {
        src.by_ref().collect()
    }

    #[test]
    fn window_snaps_to_frames_and_stops_at_the_end() {
        let c = clip();
        // 0.25 s = frame 1 = sample 2; 0.75 s = frame 3 = sample 6.
        let mut src = PcmSource::new(&c, 0.25, 0.75);
        assert_eq!(src.start, 2);
        assert_eq!(src.end, 6);
        assert_eq!(src.channels().get(), 2);
        assert_eq!(src.sample_rate().get(), 4);
        assert_eq!(collect(&mut src), vec![2.0, 3.0, 4.0, 5.0]);
    }

    #[test]
    fn total_duration_is_the_window_not_the_file() {
        let c = clip();
        let src = PcmSource::new(&c, 0.25, 0.75);
        // 4 samples = 2 frames at 4 Hz = 0.5 s, not the file's 1.0 s.
        assert_eq!(src.total_duration(), Some(Duration::from_millis(500)));
    }

    #[test]
    fn seek_is_relative_to_the_window_start() {
        let c = clip();
        let mut src = PcmSource::new(&c, 0.25, 0.75);
        // 0.25 s past the *window* start (frame 1 of the window = frame 2 of
        // the file = sample 4). Getting this wrong is the classic bug where
        // seeking after a selection jumps to the wrong place.
        src.try_seek(Duration::from_millis(250)).unwrap();
        assert_eq!(collect(&mut src), vec![4.0, 5.0]);
    }

    #[test]
    fn seek_past_the_end_saturates() {
        let c = clip();
        let mut src = PcmSource::new(&c, 0.0, 1.0);
        src.try_seek(Duration::from_secs(99)).unwrap();
        assert_eq!(src.pos, src.end);
        assert!(src.next().is_none());
    }

    #[test]
    fn mono_clip_works_and_negative_windows_clamp() {
        let c = Clip {
            samples: Arc::new(vec![0.5, -0.5, 0.25]),
            channels: ChannelCount::new(1).unwrap(),
            rate: SampleRate::new(3).unwrap(),
            duration: 1.0,
        };
        let mut src = PcmSource::new(&c, -5.0, 0.5);
        assert_eq!(src.start, 0);
        assert_eq!(collect(&mut src), vec![0.5, -0.5]);
    }

    /// End-to-end: open the real output device, play, and watch the clock move.
    ///
    /// Skipped unless `WAVEMARK_AUDIO_TEST=1`, because CI has no sound card and
    /// a test that needs one should not be the thing that fails the build.
    #[test]
    fn engine_plays_through_the_device() {
        if std::env::var("WAVEMARK_AUDIO_TEST").as_deref() != Ok("1") {
            eprintln!("skipped: set WAVEMARK_AUDIO_TEST=1 to exercise real playback");
            return;
        }
        let rate = 48_000u32;
        let samples: Vec<f32> = (0..rate as usize)
            .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / rate as f32).sin() * 0.25)
            .collect();
        let mut eng = AudioEngine::open().expect("open default sink");

        eng.load(Arc::new(samples), 1, rate);
        eng.start(0.0, 1.0);

        // Poll for up to ~5 s: the device is the clock, so read it rather than
        // sleeping for a fixed second and hoping.
        let mut last = 0.0f64;
        for _ in 0..100 {
            std::thread::sleep(Duration::from_millis(50));
            let pos = eng.position();
            assert!(
                pos >= last - 1e-6,
                "position went backwards: {last} -> {pos}"
            );
            last = pos;
            if eng.finished() {
                break;
            }
        }
        eprintln!("reached {last:.3}s, finished={}", eng.finished());
        assert!(last > 0.2, "playback never advanced (stuck at {last})");
        assert!(eng.finished(), "one second of audio never finished");
        assert!(
            (last - 1.0).abs() < 0.05,
            "ran to {last}s, expected the whole 1 s file"
        );

        // Now the case that actually matters for a marking tool: play just a
        // window of the file. It must stop at 0.75 s and not run on to 1.0 s.
        eng.start(0.5, 0.75);
        for _ in 0..100 {
            std::thread::sleep(Duration::from_millis(50));
            if eng.finished() {
                break;
            }
        }
        let pos = eng.position();
        eprintln!("window ended at {pos:.3}s");
        assert!(eng.finished(), "windowed playback never finished");
        assert!(
            (pos - 0.75).abs() < 0.05,
            "window ran to {pos}s, expected to stop at 0.75"
        );
    }
}
