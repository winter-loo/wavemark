//! Audio decode / encode. Behind the `audio` feature so the pure data model
//! stays light for AI-agent tooling that only reads session JSON.
//!
//! Decode uses [symphonia](https://crates.io/crates/symphonia) (pure Rust, no
//! system codecs) for MP3/FLAC/WAV/AAC/Vorbis. Export writes 32-bit float WAV
//! with short linear fades so segments don't click at the cut.

use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use symphonia::core::audio::{AudioBufferRef, SampleBuffer};
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::probe::Hint;

use crate::model::TimeRange;

/// Decoded audio: interleaved f32 samples in `[-1, 1]` across all channels.
///
/// The samples sit behind an `Arc` so a GUI can hand the very same buffer to
/// its audio thread without copying it — a two hour recording is a couple of
/// gigabytes of f32, and duplicating that just to play it is not an option.
pub struct DecodedAudio {
    pub sample_rate: u32,
    pub channels: u16,
    pub duration_sec: f64,
    pub samples: Arc<Vec<f32>>,
}

impl DecodedAudio {
    /// Build a [crate::Peaks] overview at `bucket_count` resolution.
    pub fn peaks(&self, bucket_count: usize) -> crate::Peaks {
        crate::Peaks::from_interleaved(&self.samples, self.sample_rate, self.channels, bucket_count)
    }

    /// Mutable access to the samples, cloning only if the buffer is shared.
    ///
    /// This is the one way to get a `&mut [f32]`; the field itself is behind an
    /// `Arc` and therefore not directly mutable.
    pub fn samples_mut(&mut self) -> &mut [f32] {
        Arc::make_mut(&mut self.samples).as_mut_slice()
    }

    /// Move the samples out, cloning only if the buffer is shared.
    pub fn into_samples(self) -> Vec<f32> {
        match Arc::try_unwrap(self.samples) {
            Ok(v) => v,
            // Not worth panicking over: `Arc::make_mut` on a shared buffer
            // would clone too, this just says so plainly.
            Err(a) => (*a).clone(),
        }
    }
}

/// Decode any supported container/codec to interleaved f32 PCM.
pub fn decode(path: &Path) -> Result<DecodedAudio> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let prober = symphonia::default::get_probe();
    let probed = prober
        .format(&hint, mss, &Default::default(), &Default::default())
        .map_err(|e| anyhow!("probe failed: {e}"))?;

    let mut format = probed.format;

    let (track_id, codec_params) = {
        let track = format
            .default_track()
            .ok_or_else(|| anyhow!("file has no tracks"))?;
        (track.id, track.codec_params.clone())
    };

    let sample_rate = codec_params
        .sample_rate
        .ok_or_else(|| anyhow!("stream has no sample rate"))?;
    let channels = codec_params.channels.map(|c| c.count() as u16).unwrap_or(1);

    let mut decoder = symphonia::default::get_codecs()
        .make(&codec_params, &DecoderOptions::default())
        .map_err(|e| anyhow!("decoder init failed: {e}"))?;

    let mut samples = Vec::new();
    let mut sample_buf: Option<SampleBuffer<f32>> = None;
    let mut total_frames = 0u64;

    while let Ok(packet) = format.next_packet() {
        if packet.track_id() != track_id {
            continue;
        }
        let decoded: AudioBufferRef = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(_) => continue, // skip corrupt packet, keep going
        };
        let frames = decoded.frames();
        let chans = decoded.spec().channels.count();
        let need = frames * chans;
        if sample_buf.is_none() || sample_buf.as_ref().unwrap().capacity() < need {
            let spec = *decoded.spec();
            sample_buf = Some(SampleBuffer::<f32>::new(frames as u64, spec));
        }
        let buf = sample_buf.as_mut().unwrap();
        buf.copy_interleaved_ref(decoded);
        samples.extend_from_slice(buf.samples());
        total_frames += frames as u64;
    }

    let duration_sec = if sample_rate > 0 {
        total_frames as f64 / sample_rate as f64
    } else {
        0.0
    };

    Ok(DecodedAudio {
        sample_rate,
        channels,
        duration_sec,
        samples: Arc::new(samples),
    })
}

/// Write a whole interleaved f32 buffer to a 32-bit float WAV file.
///
/// Used by the DSP commands (normalize, gain, cut, concat). Float rather than
/// int on purpose: intermediate processing can legally exceed `[-1, 1]` and we
/// would rather hand an agent the honest values than silently clip them.
pub fn write_wav(samples: &[f32], sample_rate: u32, channels: u16, out: &Path) -> Result<()> {
    let spec = hound::WavSpec {
        channels,
        sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create(out, spec)
        .with_context(|| format!("cannot create output {}", out.display()))?;
    for &s in samples {
        writer
            .write_sample(s)
            .map_err(|e| anyhow!("write failed: {e}"))?;
    }
    writer
        .finalize()
        .map_err(|e| anyhow!("finalize failed: {e}"))?;
    Ok(())
}

/// Frame index for a time in seconds, clamped to `[0, total_frames]`.
#[must_use]
pub fn frame_at(t: f64, sample_rate: u32, total_frames: usize) -> usize {
    if t <= 0.0 || sample_rate == 0 {
        0
    } else {
        ((t * sample_rate as f64).round() as usize).min(total_frames)
    }
}

/// Export the samples in `range` to a 32-bit float WAV file with short fades.
///
/// `fade_ms` of linear gain ramp is applied at the head and tail so the cut
/// doesn't pop. Pass `0.0` for no fade.
pub fn export_range(
    samples: &[f32],
    sample_rate: u32,
    channels: u16,
    range: TimeRange,
    out: &Path,
    fade_ms: f64,
) -> Result<()> {
    let chans = channels.max(1) as usize;
    let frame_at = |t: f64| (t * sample_rate as f64).round() as usize;
    let mut start_frame = frame_at(range.start);
    let mut end_frame = frame_at(range.end);
    if end_frame < start_frame {
        std::mem::swap(&mut start_frame, &mut end_frame);
    }
    let total_frames = samples.len() / chans;
    start_frame = start_frame.min(total_frames);
    end_frame = end_frame.min(total_frames).max(start_frame);

    let seg_start = start_frame * chans;
    let seg_end = end_frame * chans;
    let seg = &samples[seg_start..seg_end];
    let seg_frames = seg.len() / chans;

    let spec = hound::WavSpec {
        channels,
        sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };

    let mut writer = hound::WavWriter::create(out, spec)
        .with_context(|| format!("cannot create output {}", out.display()))?;

    let fade_n = (fade_ms / 1000.0 * sample_rate as f64).round() as usize;
    let fade_n = fade_n.min(seg_frames / 2);

    for (i, &s) in seg.iter().enumerate() {
        let frame = i / chans;
        let mut v = s;
        if fade_n > 0 {
            // fade-in over the first `fade_n` frames
            if frame < fade_n {
                v *= frame as f32 / fade_n as f32;
            }
            // fade-out over the last `fade_n` frames
            let from_end = seg_frames.saturating_sub(frame);
            if from_end <= fade_n {
                v *= from_end as f32 / fade_n as f32;
            }
        }
        writer
            .write_sample(v)
            .map_err(|e| anyhow!("write failed: {e}"))?;
    }
    writer
        .finalize()
        .map_err(|e| anyhow!("finalize failed: {e}"))?;
    Ok(())
}
