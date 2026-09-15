//! Waveform peak computation.
//!
//! The GPU canvas never touches raw PCM directly — it renders peaks. We reduce
//! the audio to `bucket_count` windows, each summarised by a min/max pair (and
//! RMS, used for the softer "energy" fill). This is computed once per file and
//! reused at every zoom level by down-sampling the already-cheap peak stream.

use serde::{Deserialize, Serialize};

/// One vertical slice of a waveform: the loudness envelope between min and max.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Peak {
    pub min: f32,
    pub max: f32,
    pub rms: f32,
}

/// The full peak series for an audio file at a given resolution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Peaks {
    /// One entry per bucket, left-to-right in time.
    pub buckets: Vec<Peak>,
    /// How many source samples each bucket summarises.
    pub samples_per_bucket: usize,
    pub sample_rate: u32,
    pub channels: u16,
}

impl Peaks {
    /// Compute `bucket_count` peaks from interleaved f32 samples
    /// (`samples.len() == frames * channels`).
    pub fn from_interleaved(
        samples: &[f32],
        sample_rate: u32,
        channels: u16,
        bucket_count: usize,
    ) -> Self {
        let chans = channels.max(1) as usize;
        let total_samples = samples.len() / chans;
        let buckets = if total_samples == 0 || bucket_count == 0 {
            Vec::new()
        } else {
            let per = (total_samples / bucket_count).max(1);
            let mut out = Vec::with_capacity(bucket_count);
            let mut i = 0;
            for _ in 0..bucket_count {
                let end = (i + per).min(total_samples);
                let mut min = f32::INFINITY;
                let mut max = f32::NEG_INFINITY;
                let mut sum_sq = 0.0f64;
                let mut n = 0usize;
                for f in i..end {
                    // mono-mix the frame for the overview shape
                    let mut acc = 0.0f32;
                    for c in 0..chans {
                        let v = samples[f * chans + c];
                        acc += v;
                        if v < min {
                            min = v;
                        }
                        if v > max {
                            max = v;
                        }
                    }
                    let mono = acc / chans as f32;
                    sum_sq += (mono as f64) * (mono as f64);
                    n += 1;
                }
                if n == 0 {
                    min = 0.0;
                    max = 0.0;
                }
                out.push(Peak {
                    min: if min.is_finite() { min } else { 0.0 },
                    max: if max.is_finite() { max } else { 0.0 },
                    rms: if n > 0 {
                        (sum_sq / n as f64).sqrt() as f32
                    } else {
                        0.0
                    },
                });
                i = end;
                if i >= total_samples {
                    break;
                }
            }
            out
        };

        Self {
            buckets,
            samples_per_bucket: (total_samples / bucket_count).max(1),
            sample_rate,
            channels,
        }
    }

    /// Bucket index for a time in seconds.
    #[must_use]
    pub fn bucket_at(&self, seconds: f64) -> usize {
        let total = self.duration();
        if total <= 0.0 || self.buckets.is_empty() {
            return 0;
        }
        let frac = (seconds / total).clamp(0.0, 1.0);
        (frac * self.buckets.len() as f64) as usize
    }

    #[must_use]
    pub fn duration(&self) -> f64 {
        if self.sample_rate == 0 {
            0.0
        } else {
            (self.buckets.len() * self.samples_per_bucket) as f64 / self.sample_rate as f64
        }
    }

    /// Seconds covered by a single bucket. Everything that converts bucket
    /// indices to times should go through this rather than recomputing it.
    #[must_use]
    pub fn seconds_per_bucket(&self) -> f64 {
        if self.sample_rate == 0 {
            0.0
        } else {
            self.samples_per_bucket as f64 / self.sample_rate as f64
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_min_max_rms() {
        // 2 channels, 4 frames: [1,-1 | 0.5,0.5 | -0.5,-0.5 | 0.25,0.25]
        let samples = [1.0, -1.0, 0.5, 0.5, -0.5, -0.5, 0.25, 0.25];
        let p = Peaks::from_interleaved(&samples, 44_100, 2, 1);
        assert_eq!(p.buckets.len(), 1);
        let b = p.buckets[0];
        assert_eq!(b.min, -1.0);
        assert_eq!(b.max, 1.0);
        assert!(b.rms > 0.0);
    }

    #[test]
    fn empty_input_is_safe() {
        let p = Peaks::from_interleaved(&[], 44_100, 2, 100);
        assert!(p.buckets.is_empty());
    }
}
