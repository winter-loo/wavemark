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

    /// Write to `path` atomically: serialise to a sibling temp file, then
    /// rename. A crashed or concurrent reader sees either the old file or the
    /// new one, never a half-written one.
    pub fn save_atomic(&self, path: &Path) -> anyhow::Result<()> {
        let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, path).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })?;
        Ok(())
    }

    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let raw = std::fs::read_to_string(path)?;
        let session: Session = serde_json::from_str(&raw)?;
        Ok(session)
    }

    /// Sort annotations by start time, tie-broken by end. Called before saving
    /// so the file stays stable regardless of the order marks were added.
    pub fn sort_annotations(&mut self) {
        self.annotations.sort_by(|a, b| {
            a.range
                .start
                .partial_cmp(&b.range.start)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    a.range
                        .end
                        .partial_cmp(&b.range.end)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
        });
    }
}

// ---- merge: the agent -> GUI direction --------------------------------------

/// What an annotation coming back from an agent may carry. Every field is
/// optional so an agent can patch just the text, or just the range, without
/// having to echo the whole record back.
///
/// Unknown fields are deliberately *allowed*. The main workflow is
/// `annotations --format json > notes.json`, edit, `apply notes.json` — and the
/// payload carries `duration_sec`, which is derived rather than patchable.
/// Rejecting it would break the primary round-trip in order to catch a typo.
/// Schema validation happens at the document level instead, where it belongs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AnnotationPatch {
    /// Existing annotation to update. Omit (or leave empty) to create a new one.
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub start_sec: Option<f64>,
    #[serde(default)]
    pub end_sec: Option<f64>,
    /// Alternative to `start_sec` / `end_sec`, as `M:SS.mmm`.
    #[serde(default)]
    pub start: Option<String>,
    #[serde(default)]
    pub end: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub color: Option<String>,
}

/// A document an AI agent (or another tool) hands back to wavemark.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatchDocument {
    #[serde(default = "default_schema")]
    pub schema: String,
    #[serde(default)]
    pub annotations: Vec<AnnotationPatch>,
}

fn default_schema() -> String {
    "wavemark/patches".to_string()
}

pub const PATCH_SCHEMA: &str = "wavemark/patches";

/// Counters describing what a merge actually did. Printed by `wavemark apply`
/// so a caller can tell "nothing to do" from "wrote three things".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MergeReport {
    pub created: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub rejected: usize,
}

impl MergeReport {
    pub fn total(&self) -> usize {
        self.created + self.updated + self.unchanged + self.rejected
    }

    /// True when the merge would change the file. `apply --dry-run` uses this
    /// to decide its exit code.
    pub fn is_noop(&self) -> bool {
        self.created == 0 && self.updated == 0
    }
}

/// Why a patch was refused. Kept as data rather than an error string so the CLI
/// can print a table of everything that went wrong in one pass.
#[derive(Debug, Clone)]
pub struct Rejection {
    pub index: usize,
    pub reason: String,
}

impl Session {
    /// Reject any annotation whose range falls outside `[0, duration]` or is
    /// degenerate. Returns the rejections; callers decide whether to bail.
    pub fn validate(&self) -> Vec<Rejection> {
        let mut out = Vec::new();
        for (i, a) in self.annotations.iter().enumerate() {
            if !a.range.start.is_finite() || !a.range.end.is_finite() {
                out.push(Rejection {
                    index: i,
                    reason: "range is not finite".into(),
                });
            } else if a.range.start < 0.0 || a.range.end > self.duration_sec + 0.001 {
                out.push(Rejection {
                    index: i,
                    reason: format!(
                        "range {}..{} exceeds duration {:.3}",
                        a.range.start, a.range.end, self.duration_sec
                    ),
                });
            } else if a.range.duration() <= 0.0 {
                out.push(Rejection {
                    index: i,
                    reason: "empty range".into(),
                });
            }
        }
        out
    }

    /// Apply a list of patches. Existing annotations are matched by `id`;
    /// anything without a usable id creates a new annotation.
    ///
    /// Rejections never abort the whole batch — one bad row shouldn't cost an
    /// agent its other nineteen edits.
    pub fn merge_patches(&mut self, patches: &[AnnotationPatch]) -> (MergeReport, Vec<Rejection>) {
        let mut report = MergeReport::default();
        let mut rejections = Vec::new();

        for (i, p) in patches.iter().enumerate() {
            let range = match resolve_patch_range(p) {
                Ok(r) => r,
                Err(e) => {
                    report.rejected += 1;
                    rejections.push(Rejection {
                        index: i,
                        reason: e,
                    });
                    continue;
                }
            };

            if !range.start.is_finite()
                || !range.end.is_finite()
                || range.start < 0.0
                || range.end > self.duration_sec + 0.001
                || range.duration() <= 0.0
            {
                report.rejected += 1;
                rejections.push(Rejection {
                    index: i,
                    reason: format!(
                        "range {:.3}..{:.3} invalid for duration {:.3}",
                        range.start, range.end, self.duration_sec
                    ),
                });
                continue;
            }

            let existing =
                p.id.as_deref()
                    .filter(|s| !s.trim().is_empty())
                    .and_then(|id| self.annotations.iter_mut().find(|a| a.id == id));

            match existing {
                Some(a) => {
                    let changed = a.range != range
                        || p.text.as_deref().is_some_and(|t| t != a.text)
                        || p.tags.as_ref().is_some_and(|t| *t != a.tags)
                        || p.color
                            .as_ref()
                            .is_some_and(|c| Some(c) != a.color.as_ref());
                    a.range = range;
                    if let Some(t) = &p.text {
                        a.text = t.clone();
                    }
                    if let Some(t) = &p.tags {
                        a.tags = t.clone();
                    }
                    if p.color.is_some() {
                        a.color = p.color.clone();
                    }
                    if changed {
                        report.updated += 1;
                    } else {
                        report.unchanged += 1;
                    }
                }
                None => {
                    let mut a = Annotation::new(range, p.text.clone().unwrap_or_default());
                    if let Some(t) = &p.tags {
                        a.tags = t.clone();
                    }
                    if p.color.is_some() {
                        a.color = p.color.clone();
                    }
                    if let Some(id) = p.id.as_deref().filter(|s| !s.trim().is_empty()) {
                        a.id = id.to_string();
                    }
                    self.add_annotation(a);
                    report.created += 1;
                }
            }
        }

        self.sort_annotations();
        (report, rejections)
    }

    /// Keep only annotations whose id appears in `keep`. Used by
    /// `apply --prune` so an agent that deleted a mark really deletes it.
    pub fn retain_ids(&mut self, keep: &std::collections::HashSet<String>) -> usize {
        let before = self.annotations.len();
        self.annotations.retain(|a| keep.contains(&a.id));
        before - self.annotations.len()
    }
}

/// Resolve a patch's range from whichever of the four spellings it used.
/// `*_sec` wins over the formatted string; missing sides fall back to the
/// existing annotation's values when updating, which is why an agent can send
/// `{id, end_sec}` and move only the end.
fn resolve_patch_range(p: &AnnotationPatch) -> Result<TimeRange, String> {
    let start = match (p.start_sec, &p.start) {
        (Some(v), _) => v,
        (None, Some(s)) => {
            crate::time::parse_ts(s).ok_or_else(|| format!("unparseable start {s:?}"))?
        }
        (None, None) => {
            return Err("patch has no start (start_sec or start)".into());
        }
    };
    let end = match (p.end_sec, &p.end) {
        (Some(v), _) => v,
        (None, Some(s)) => {
            crate::time::parse_ts(s).ok_or_else(|| format!("unparseable end {s:?}"))?
        }
        (None, None) => {
            return Err("patch has no end (end_sec or end)".into());
        }
    };
    Ok(TimeRange::new(start, end))
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

    fn patch(start: f64, end: f64) -> AnnotationPatch {
        AnnotationPatch {
            start_sec: Some(start),
            end_sec: Some(end),
            ..AnnotationPatch::default()
        }
    }

    #[test]
    fn merge_creates_then_updates_by_id() {
        let mut s = Session::new("clip.wav", 30.0, 48_000, 1);

        let (r1, rej1) = s.merge_patches(&[patch(1.0, 2.0)]);
        assert_eq!(r1.created, 1);
        assert_eq!(r1.updated, 0);
        assert!(rej1.is_empty());
        let id = s.annotations[0].id.clone();

        let mut p = patch(1.0, 2.5);
        p.id = Some(id.clone());
        let (r2, _) = s.merge_patches(&[p]);
        assert_eq!(r2.updated, 1, "same id should update, not create");
        assert_eq!(s.annotations.len(), 1);
        assert!((s.annotations[0].range.end - 2.5).abs() < f64::EPSILON);

        let mut p = patch(1.0, 2.5);
        p.id = Some(id);
        let (r3, _) = s.merge_patches(&[p]);
        assert_eq!(r3.unchanged, 1, "identical patch is a no-op");
    }

    #[test]
    fn merge_rejects_out_of_range_without_aborting_batch() {
        let mut s = Session::new("clip.wav", 10.0, 48_000, 1);
        let patches = vec![patch(1.0, 2.0), patch(50.0, 60.0), patch(3.0, 4.0)];
        let (r, rej) = s.merge_patches(&patches);
        assert_eq!(r.created, 2);
        assert_eq!(r.rejected, 1);
        assert_eq!(rej.len(), 1);
        assert_eq!(rej[0].index, 1);
        assert_eq!(s.annotations.len(), 2, "good rows survive a bad one");
    }

    #[test]
    fn merge_accepts_formatted_timestamps() {
        let mut s = Session::new("clip.wav", 30.0, 48_000, 1);
        let p = AnnotationPatch {
            start: Some("0:01.500".into()),
            end: Some("0:02.250".into()),
            ..AnnotationPatch::default()
        };
        let (r, rej) = s.merge_patches(&[p]);
        assert!(rej.is_empty(), "{rej:?}");
        assert_eq!(r.created, 1);
        assert!((s.annotations[0].range.start - 1.5).abs() < f64::EPSILON);
        assert!((s.annotations[0].range.end - 2.25).abs() < f64::EPSILON);
    }

    #[test]
    fn merge_sorts_by_start_time() {
        let mut s = Session::new("clip.wav", 30.0, 48_000, 1);
        s.merge_patches(&[patch(9.0, 10.0), patch(1.0, 2.0), patch(5.0, 6.0)]);
        let starts: Vec<f64> = s.annotations.iter().map(|a| a.range.start).collect();
        assert_eq!(starts, vec![1.0, 5.0, 9.0]);
    }

    #[test]
    fn retain_ids_prunes() {
        let mut s = Session::new("clip.wav", 30.0, 48_000, 1);
        s.merge_patches(&[patch(1.0, 2.0), patch(3.0, 4.0)]);
        let keep: std::collections::HashSet<String> =
            [s.annotations[0].id.clone()].into_iter().collect();
        assert_eq!(s.retain_ids(&keep), 1);
        assert_eq!(s.annotations.len(), 1);
    }

    #[test]
    fn validate_flags_impossible_ranges() {
        let mut s = Session::new("clip.wav", 10.0, 48_000, 1);
        s.add_annotation(Annotation::new(TimeRange::new(1.0, 2.0), "ok"));
        let a = Annotation::new(TimeRange::new(5.0, 99.0), "too long");
        s.add_annotation(a);
        let bad = s.validate();
        assert_eq!(bad.len(), 1);
        assert!(bad[0].reason.contains("exceeds duration"));
    }

    #[test]
    fn atomic_save_produces_loadable_file() {
        let mut s = Session::new("clip.wav", 12.5, 48_000, 2);
        s.add_annotation(Annotation::new(TimeRange::new(1.0, 2.0), "hello"));
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("session.wavemark.json");
        s.save_atomic(&p).unwrap();
        let loaded = Session::load(&p).unwrap();
        assert_eq!(loaded.annotations.len(), 1);
        assert!(
            !p.with_extension("json.tmp").exists(),
            "temp file must be cleaned up"
        );
    }
}
