//! Silence detection over pre-computed peaks.
//!
//! The overview waveform already shows you *where* the sound is. This module
//! turns that picture into data: it walks the RMS buckets and reports the runs
//! that fall below a threshold.
//!
//! Runs on `Peaks` rather than raw samples on purpose — a 40-minute file has
//! ~240k buckets, not ~100M samples, so this is effectively free and can run
//! on every repaint if the GUI ever wants to.

use crate::peaks::Peaks;

/// A detected run of audio, in seconds. Half-open: `[start, end)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Span {
    pub start: f64,
    pub end: f64,
}

impl Span {
    #[must_use]
    pub fn duration(&self) -> f64 {
        (self.end - self.start).max(0.0)
    }
}

/// Detection parameters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SilenceOptions {
    /// Anything at or below this RMS level counts as silence.
    pub threshold_dbfs: f32,
    /// Runs shorter than this are discarded.
    pub min_len_sec: f64,
    /// Shrink each reported run by this much on both sides, so the reported
    /// span sits clearly *inside* the silence rather than butting up against
    /// the edge of a word.
    pub pad_sec: f64,
}

impl Default for SilenceOptions {
    fn default() -> Self {
        Self {
            threshold_dbfs: -50.0,
            min_len_sec: 0.35,
            pad_sec: 0.05,
        }
    }
}

/// Convert a linear RMS amplitude to dBFS. Zero (digital silence) becomes
/// `-inf`, which [`is_silent`] handles explicitly.
#[inline]
fn to_dbfs(rms: f32) -> f32 {
    if rms <= 0.0 {
        f32::NEG_INFINITY
    } else {
        20.0 * rms.log10()
    }
}

/// True when `db` is at or below the threshold. `-inf` (true digital silence)
/// counts as silent — otherwise the "is it silent?" check on an all-zero buffer
/// would never match, because every comparison against `-inf` is false-ish in
/// the direction we care about.
#[inline]
fn is_silent(db: f32, threshold: f32) -> bool {
    !db.is_finite() || db <= threshold
}

/// Collect contiguous runs of buckets matching `pred`, as time spans.
/// Runs shorter than `min_len_sec` are dropped before padding.
fn runs(peaks: &Peaks, pred: impl Fn(f32) -> bool, opts: &SilenceOptions) -> Vec<Span> {
    if peaks.buckets.is_empty() || peaks.sample_rate == 0 {
        return Vec::new();
    }
    let per_bucket = peaks.seconds_per_bucket();
    if per_bucket <= 0.0 {
        return Vec::new();
    }
    let total = peaks.duration();

    let mut out = Vec::new();
    let mut run_start: Option<usize> = None;

    for (i, b) in peaks.buckets.iter().enumerate() {
        match (run_start, pred(to_dbfs(b.rms))) {
            (None, true) => run_start = Some(i),
            (Some(s), false) => {
                push_run(&mut out, s, i, per_bucket, total, opts);
                run_start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = run_start {
        push_run(&mut out, s, peaks.buckets.len(), per_bucket, total, opts);
    }
    out
}

fn push_run(
    out: &mut Vec<Span>,
    first: usize,
    last: usize,
    per_bucket: f64,
    total: f64,
    opts: &SilenceOptions,
) {
    let raw_start = first as f64 * per_bucket;
    let raw_end = last as f64 * per_bucket;
    if raw_end - raw_start < opts.min_len_sec {
        return;
    }
    let start = (raw_start + opts.pad_sec).min(total);
    let end = (raw_end - opts.pad_sec).max(start);
    if end - start <= 0.0 {
        return;
    }
    out.push(Span { start, end });
}

/// Runs of near-silence, e.g. gaps between words or dead air.
#[must_use]
pub fn detect_silence(peaks: &Peaks, opts: &SilenceOptions) -> Vec<Span> {
    runs(peaks, |db| is_silent(db, opts.threshold_dbfs), opts)
}

/// Runs of *sound* — the inverse. Useful for "give me every utterance"
/// without having to subtract silence spans yourself.
#[must_use]
pub fn detect_sound(peaks: &Peaks, opts: &SilenceOptions) -> Vec<Span> {
    runs(peaks, |db| !is_silent(db, opts.threshold_dbfs), opts)
}

/// Fold a list of spans into annotation-shaped suggestions.
///
/// Returned spans are tagged `auto:silence` / `auto:sound` by the caller so a
/// human reading the session can tell machine guesses from real notes.
#[must_use]
pub fn to_ranges(spans: &[Span]) -> Vec<crate::TimeRange> {
    spans
        .iter()
        .map(|s| crate::TimeRange::new(s.start, s.end))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peaks::Peaks;

    fn peaks_from(rms: &[f32]) -> Peaks {
        Peaks {
            buckets: rms
                .iter()
                .map(|r| crate::peaks::Peak {
                    min: -*r,
                    max: *r,
                    rms: *r,
                })
                .collect(),
            // 0.1 s per bucket, so 10 buckets == 1 second.
            samples_per_bucket: 1_000,
            sample_rate: 10_000,
            channels: 1,
        }
    }

    fn opts() -> SilenceOptions {
        SilenceOptions {
            threshold_dbfs: -50.0,
            min_len_sec: 0.0,
            pad_sec: 0.0,
        }
    }

    const LOUD: f32 = 0.5; // ~-6 dBFS
    const QUIET: f32 = 0.0001; // ~-80 dBFS

    #[test]
    fn finds_the_gap_between_two_words() {
        // 1s loud, 1s quiet, 1s loud @ 10 buckets/sec.
        let mut v = vec![LOUD; 10];
        v.extend(vec![QUIET; 10]);
        v.extend(vec![LOUD; 10]);
        let p = peaks_from(&v);

        let gaps = detect_silence(&p, &opts());
        assert_eq!(gaps.len(), 1, "{gaps:?}");
        assert!((gaps[0].start - 1.0).abs() < 1e-6);
        assert!((gaps[0].end - 2.0).abs() < 1e-6);

        let sounds = detect_sound(&p, &opts());
        assert_eq!(sounds.len(), 2, "{sounds:?}");
    }

    #[test]
    fn min_length_filters_short_runs() {
        let mut v = vec![LOUD; 10];
        v.extend(vec![QUIET; 5]); // 0.5s
        v.extend(vec![LOUD; 10]);
        let p = peaks_from(&v);

        let lenient = SilenceOptions {
            min_len_sec: 0.0,
            ..opts()
        };
        assert_eq!(detect_silence(&p, &lenient).len(), 1);

        let strict = SilenceOptions {
            min_len_sec: 1.0,
            ..opts()
        };
        assert_eq!(detect_silence(&p, &strict).len(), 0);
    }

    #[test]
    fn padding_shrinks_inside_the_run() {
        let mut v = vec![LOUD; 10];
        v.extend(vec![QUIET; 10]);
        v.extend(vec![LOUD; 10]);
        let p = peaks_from(&v);

        let padded = SilenceOptions {
            pad_sec: 0.2,
            ..opts()
        };
        let gaps = detect_silence(&p, &padded);
        assert!((gaps[0].start - 1.2).abs() < 1e-6);
        assert!((gaps[0].end - 1.8).abs() < 1e-6);
    }

    #[test]
    fn digital_silence_counts_as_silence() {
        // rms 0.0 -> -inf dBFS, which must still be treated as silent.
        let p = peaks_from(&[0.0; 10]);
        assert_eq!(detect_silence(&p, &opts()).len(), 1);
        assert_eq!(detect_sound(&p, &opts()).len(), 0);
    }

    #[test]
    fn padding_never_inverts_a_run() {
        // 0.3s gap, padded by 0.2 each side -> would invert; must be dropped.
        let mut v = vec![LOUD; 10];
        v.extend(vec![QUIET; 3]);
        v.extend(vec![LOUD; 10]);
        let p = peaks_from(&v);
        let o = SilenceOptions {
            pad_sec: 0.2,
            min_len_sec: 0.0,
            ..opts()
        };
        assert!(detect_silence(&p, &o).is_empty());
    }

    #[test]
    fn empty_peaks_yield_nothing() {
        let p = peaks_from(&[]);
        assert!(detect_silence(&p, &opts()).is_empty());
    }
}
