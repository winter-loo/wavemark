//! The wavemark data model: time ranges, annotations, and the session file.
//!
//! A **Session** is the `.wavemark.json` sidecar that sits next to an audio
//! file (`song.wav` → `song.wavemark.json`). It stores the audio metadata and
//! every annotation the user made. The GUI writes it; the CLI reads it; an AI
//! agent consumes the CLI's output. One format, three producers, one consumer.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A half-open time range `[start, end)` in seconds.
///
/// It always stores `start <= end`; the constructor normalizes the order so a
/// drag that ends to the left of its start still produces a valid range.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TimeRange {
    pub start: f64,
    pub end: f64,
}

impl TimeRange {
    pub fn new(start: f64, end: f64) -> Self {
        Self {
            start: start.min(end),
            end: start.max(end),
        }
    }

    /// Duration in seconds, never negative.
    #[must_use]
    pub fn duration(&self) -> f64 {
        (self.end - self.start).max(0.0)
    }

    #[must_use]
    pub fn contains(&self, t: f64) -> bool {
        t >= self.start && t < self.end
    }

    #[must_use]
    pub fn overlaps(&self, other: &TimeRange) -> bool {
        self.start < other.end && other.start < self.end
    }

    /// Clamp this range into `[0, max]`.
    #[must_use]
    pub fn clamp_to(&self, max: f64) -> Self {
        let start = self.start.clamp(0.0, max);
        let end = self.end.clamp(0.0, max);
        Self::new(start, end)
    }
}

/// A user annotation bound to an exact audio range. This is the unit of work an
/// AI agent receives: "between 00:12.345 and 00:15.678 the speaker says X".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Annotation {
    /// Stable, opaque id (UUID v4 by default). The CLI and AI agent use it to
    /// refer back to a specific selection across turns.
    pub id: String,
    pub range: TimeRange,
    pub text: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Optional color token ("#ff8800" or "red") used by the GUI only.
    #[serde(default)]
    pub color: Option<String>,
    /// ISO-8601 creation timestamp (local).
    pub created_at: String,
}

impl Annotation {
    pub fn new(range: TimeRange, text: impl Into<String>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            range,
            text: text.into(),
            tags: Vec::new(),
            color: None,
            created_at: chrono::Local::now().to_rfc3339(),
        }
    }

    #[must_use]
    pub fn with_tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self
    }
}

/// The on-disk session format. Versioned so future wavemark releases can evolve
/// it without breaking AI agents that pinned an older schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// Schema version. Bump on breaking changes.
    pub version: u32,
    /// Path to the audio file. Stored as the user passed it (may be relative).
    pub audio_path: String,
    pub duration_sec: f64,
    pub sample_rate: u32,
    pub channels: u16,
    #[serde(default)]
    pub annotations: Vec<Annotation>,
    /// The last selection the user made, even if it has no annotation yet.
    #[serde(default)]
    pub last_selection: Option<TimeRange>,
}

pub const SESSION_VERSION: u32 = 1;
/// The extension wavemark appends to an audio file's name for its sidecar.
pub const SESSION_EXT: &str = "wavemark.json";

impl Session {
    pub fn new(
        audio_path: impl Into<String>,
        duration_sec: f64,
        sample_rate: u32,
        channels: u16,
    ) -> Self {
        Self {
            version: SESSION_VERSION,
            audio_path: audio_path.into(),
            duration_sec,
            sample_rate,
            channels,
            annotations: Vec::new(),
            last_selection: None,
        }
    }

    /// The sidecar path for a given audio file (`song.wav` → `song.wavemark.json`).
    #[must_use]
    pub fn sidecar_path(audio_path: &Path) -> PathBuf {
        let mut out = audio_path.as_os_str().to_os_string();
        out.push(".");
        out.push(SESSION_EXT);
        PathBuf::from(out)
    }

    /// Find a session file by walking from `audio_path` up to the same file with
    /// the `.wavemark.json` suffix. Returns the path if it exists.
    #[must_use]
    pub fn resolve_sidecar(audio_path: &Path) -> Option<PathBuf> {
        let sidecar = Self::sidecar_path(audio_path);
        sidecar.exists().then_some(sidecar)
    }

    pub fn add_annotation(&mut self, ann: Annotation) -> &Annotation {
        self.annotations.push(ann);
        self.annotations.last().expect("just pushed")
    }

    pub fn remove_annotation(&mut self, id: &str) -> bool {
        let before = self.annotations.len();
        self.annotations.retain(|a| a.id != id);
        self.annotations.len() != before
    }

    pub fn annotation(&self, id: &str) -> Option<&Annotation> {
        self.annotations.iter().find(|a| a.id == id)
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let raw = std::fs::read_to_string(path)?;
        let session: Session = serde_json::from_str(&raw)?;
        Ok(session)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_normalizes_order() {
        let r = TimeRange::new(10.0, 3.0);
        assert_eq!(r.start, 3.0);
        assert_eq!(r.end, 10.0);
        assert!((r.duration() - 7.0).abs() < f64::EPSILON);
    }

    #[test]
    fn range_overlap_and_contains() {
        let a = TimeRange::new(0.0, 5.0);
        let b = TimeRange::new(4.0, 9.0);
        assert!(a.overlaps(&b));
        assert!(a.contains(4.0));
        assert!(!a.contains(5.0)); // half-open
    }

    #[test]
    fn sidecar_path_is_adjacent() {
        let p = Session::sidecar_path(Path::new("/a/clip.wav"));
        assert_eq!(p.to_str().unwrap(), "/a/clip.wav.wavemark.json");
    }

    #[test]
    fn session_round_trips() {
        let mut s = Session::new("clip.wav", 12.5, 48_000, 2);
        s.add_annotation(Annotation::new(TimeRange::new(1.0, 2.0), "hello"));
        let tmp = tempfile::NamedTempFile::new()
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        s.save(&tmp).unwrap();
        let loaded = Session::load(&tmp).unwrap();
        assert_eq!(loaded.duration_sec, 12.5);
        assert_eq!(loaded.annotations.len(), 1);
        assert_eq!(loaded.annotations[0].text, "hello");
        assert_eq!(loaded.version, SESSION_VERSION);
    }
}
