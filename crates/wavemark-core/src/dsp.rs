//! Everyday audio operations: normalize, gain, trim, split, concatenate.
//!
//! Every function here is a pure transformation over interleaved `f32`
//! samples — no I/O, no allocation surprises beyond the returned buffer. That
//! keeps them unit-testable and lets both the CLI and the GUI call the same
//! code.
//!
//! Nothing here touches the source file. Callers write results to a *new* path.

/// Largest absolute sample value in the buffer.
#[must_use]
pub fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0_f32, |m, s| m.max(s.abs()))
}

/// Peak level in dBFS. Silence is `-inf`; callers should handle that.
#[must_use]
pub fn peak_dbfs(samples: &[f32]) -> f32 {
    let p = peak(samples);
    if p <= 0.0 {
        f32::NEG_INFINITY
    } else {
        20.0 * p.log10()
    }
}

/// RMS level in dBFS.
#[must_use]
pub fn rms_dbfs(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return f32::NEG_INFINITY;
    }
    let sum: f32 = samples.iter().map(|s| s * s).sum();
    let rms = (sum / samples.len() as f32).sqrt();
    if rms <= 0.0 {
        f32::NEG_INFINITY
    } else {
        20.0 * rms.log10()
    }
}

/// Convert a dB delta to a linear gain multiplier.
#[must_use]
pub fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// Convert a linear gain multiplier to a dB delta.
#[must_use]
pub fn gain_to_db(gain: f32) -> f32 {
    20.0 * gain.abs().log10()
}

/// Scale the buffer in place by `db` decibels.
///
/// Returns `true` if the result would clip (any sample leaving `[-1, 1]`).
/// Nothing is clamped — clipping is reported, not hidden, because an agent
/// needs to know its gain was too aggressive.
pub fn apply_gain(samples: &mut [f32], db: f32) -> bool {
    let g = db_to_gain(db);
    let mut clipped = false;
    for s in samples.iter_mut() {
        *s *= g;
        if s.abs() > 1.0 {
            clipped = true;
        }
    }
    clipped
}

/// Peak-normalize to `target_dbfs` (typically `-1.0` to `-0.1`).
///
/// Returns the gain actually applied, in dB. Digital silence is left alone —
/// normalizing silence would otherwise multiply by infinity.
pub fn normalize(samples: &mut [f32], target_dbfs: f32) -> f32 {
    let p = peak(samples);
    if p <= 0.0 || !p.is_finite() {
        return 0.0;
    }
    let current = 20.0 * p.log10();
    let delta = target_dbfs - current;
    let g = db_to_gain(delta);
    for s in samples.iter_mut() {
        *s *= g;
    }
    delta
}

/// Split the buffer into `n` equal parts. Trailing samples that don't divide
/// evenly stay in the last part.
#[must_use]
pub fn split_into(samples: &[f32], n: usize) -> Vec<&[f32]> {
    if n == 0 || samples.is_empty() {
        return Vec::new();
    }
    let chunk = (samples.len() / n).max(1);
    let mut out = Vec::with_capacity(n);
    let mut i = 0;
    for k in 0..n {
        if i >= samples.len() {
            break;
        }
        // The final part absorbs the remainder so no samples are dropped.
        let end = if k == n - 1 {
            samples.len()
        } else {
            (i + chunk).min(samples.len())
        };
        out.push(&samples[i..end]);
        i = end;
    }
    out
}

/// Join buffers. Callers are responsible for sample-rate agreement — this just
/// concatenates bytes.
#[must_use]
pub fn concat(parts: &[&[f32]]) -> Vec<f32> {
    let total: usize = parts.iter().map(|p| p.len()).sum();
    let mut out = Vec::with_capacity(total);
    for p in parts {
        out.extend_from_slice(p);
    }
    out
}

/// Split points (in sample frame indices) at each time in `cuts`.
///
/// `cuts` need not be sorted; out-of-range and duplicate cuts are dropped. Used
/// by "split at every annotation boundary".
#[must_use]
pub fn cut_points(cuts: &[f64], sample_rate: u32, total_frames: usize) -> Vec<usize> {
    let mut out: Vec<usize> = cuts
        .iter()
        .filter(|t| t.is_finite() && **t > 0.0)
        .map(|t| ((*t * sample_rate as f64).round() as i64).max(0) as usize)
        .filter(|i| *i > 0 && *i < total_frames)
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Slice out `[start, end)` in frames and split at `cuts`, returning segments.
#[must_use]
pub fn slice_and_split(
    samples: &[f32],
    channels: usize,
    start_frame: usize,
    end_frame: usize,
    cuts: &[usize],
) -> Vec<Vec<f32>> {
    let channels = channels.max(1);
    let start = (start_frame * channels).min(samples.len());
    let end = (end_frame * channels).min(samples.len());
    let body = &samples[start..end];

    if cuts.is_empty() {
        return vec![body.to_vec()];
    }

    let mut out = Vec::with_capacity(cuts.len() + 1);
    let mut prev = 0usize;
    for c in cuts {
        let at = (*c * channels).min(body.len());
        if at > prev {
            out.push(body[prev..at].to_vec());
            prev = at;
        }
    }
    if prev < body.len() {
        out.push(body[prev..].to_vec());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gain_round_trips() {
        for db in [-12.0, -6.0, 0.0, 3.0, 6.0] {
            assert!((gain_to_db(db_to_gain(db)) - db).abs() < 1e-4, "db={db}");
        }
    }

    #[test]
    fn normalize_hits_target() {
        let mut s = vec![0.1_f32, -0.2, 0.05];
        let applied = normalize(&mut s, -1.0);
        assert!(applied > 0.0, "should have gained up, got {applied}");
        assert!((peak_dbfs(&s) - -1.0).abs() < 1e-3, "got {}", peak_dbfs(&s));
    }

    #[test]
    fn normalize_leaves_silence_alone() {
        let mut s = vec![0.0_f32; 8];
        assert_eq!(normalize(&mut s, -1.0), 0.0);
        assert!(s.iter().all(|x| *x == 0.0));
    }

    #[test]
    fn normalize_reduces_when_too_hot() {
        // Already above target: gain must be negative (attenuation).
        let mut s = vec![1.5_f32, -1.4];
        let applied = normalize(&mut s, -1.0);
        assert!(applied < 0.0, "expected attenuation, got {applied}");
        assert!(peak(&s) <= 1.0);
    }

    #[test]
    fn apply_gain_scales_and_reports_clipping() {
        // Quiet enough that +1 dB stays inside [-1, 1].
        let mut s = vec![0.5_f32];
        assert!(!apply_gain(&mut s, 1.0), "0.5 @ +1dB must not clip");
        assert!((s[0] - 0.5 * db_to_gain(1.0)).abs() < 1e-6);

        // 0.9 * 10^(1/20) ~= 1.0098 -> over.
        let mut s = vec![0.9_f32];
        assert!(apply_gain(&mut s, 1.0), "0.9 @ +1dB must clip");
        assert!(s[0] > 1.0, "the overshoot is reported, never clamped away");

        let mut s = vec![1.5_f32];
        assert!(apply_gain(&mut s, 0.0), "already clipping stays clipping");
    }

    #[test]
    fn split_into_even_parts() {
        let s: Vec<f32> = (0..10).map(|i| i as f32).collect();
        let parts = split_into(&s, 3);
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0].len(), 3);
        assert_eq!(parts[2].len(), 4, "remainder lands in the last part");
    }

    #[test]
    fn concat_joins() {
        let a = vec![1.0_f32, 2.0];
        let b = vec![3.0_f32];
        assert_eq!(concat(&[&a, &b]), vec![1.0, 2.0, 3.0]);
    }

    #[test]
    fn cut_points_dedups_and_bounds() {
        let pts = cut_points(&[2.0, 1.0, 1.0, 99.0, -1.0], 10, 100);
        assert_eq!(pts, vec![10, 20]);
    }

    #[test]
    fn slice_and_split_respects_channels() {
        // 4 frames of stereo = 8 samples.
        let s: Vec<f32> = (0..8).map(|i| i as f32).collect();
        let segs = slice_and_split(&s, 2, 0, 4, &[2]);
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0], vec![0.0, 1.0, 2.0, 3.0]);
        assert_eq!(segs[1], vec![4.0, 5.0, 6.0, 7.0]);
    }
}
