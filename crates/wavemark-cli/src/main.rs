//! wavemark CLI — read the session you built in the GUI, hand the result to an
//! AI agent.
//!
//! The flagship command is `wavemark annotations`: it prints every annotation
//! with its exact time range as JSON or Markdown. Pipe that into an agent's
//! stdin and it knows precisely where in the audio to look:
//!
//! ```sh
//! wavemark annotations -f clip.wav --format json | your-ai-agent
//! wavemark export --id a1f3 -o clip-a1f3.wav
//! ```

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};
use serde::Serialize;
use wavemark_core::audio::{decode, export_range, frame_at, write_wav};
use wavemark_core::dsp;
use wavemark_core::silence::{detect_silence, detect_sound, SilenceOptions};
use wavemark_core::time::{format_ms, parse_ts};
use wavemark_core::{Annotation, MergeReport, PatchDocument, Session, TimeRange, PATCH_SCHEMA};

#[derive(Parser)]
#[command(
    name = "wavemark",
    version,
    about = "A CLI-friendly audio editor for human↔AI-agent collaboration"
)]
struct Cli {
    /// Path to a `.wavemark.json` session file. If omitted, wavemark looks for a
    /// sidecar (`AUDIO.wavemark.json`) or a single `*.wavemark.json` in cwd.
    #[arg(long, short = 's', global = true)]
    session: Option<PathBuf>,

    /// Audio file to operate on. Used to locate a sidecar session and to resolve
    /// the audio for export. Required when no `--session` is given.
    #[arg(long, short = 'f', global = true)]
    audio: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print all annotations with exact time ranges — the payload for an AI agent.
    Annotations {
        /// Output format: `json` (machine/agent) or `markdown` (human review).
        #[arg(long, default_value = "json")]
        format: Format,
    },
    /// Export an annotation (by id) or an explicit range to a WAV file.
    Export {
        /// Export the range of this annotation id.
        #[arg(long)]
        id: Option<String>,
        /// Export an explicit range as `START-END`, each side `M:SS.mmm` or seconds.
        #[arg(long)]
        range: Option<String>,
        /// Output path. Defaults to `<audio-stem>-<id>.wav`.
        #[arg(long, short = 'o')]
        out: Option<PathBuf>,
        /// Fade in/out length in milliseconds (default 5).
        #[arg(long, default_value = "5")]
        fade_ms: f64,
    },
    /// Print session metadata (path, duration, sample rate, channels, #annotations).
    Info,
    /// List annotations, one per line.
    List,

    /// Export every annotation to its own WAV plus one manifest.
    BatchExport {
        /// Output directory. Created if missing.
        #[arg(long, short = 'o', default_value = "wavemark-export")]
        out: PathBuf,
        /// Fade in/out length in milliseconds (default 5).
        #[arg(long, default_value = "5")]
        fade_ms: f64,
        /// Name each file from the annotation text instead of its id.
        #[arg(long)]
        name_by_text: bool,
    },
    /// Merge an agent-produced patch document back into the session.
    Apply {
        /// Path to the patch JSON. `-` reads stdin.
        manifest: PathBuf,
        /// Print what would change without writing anything.
        #[arg(long)]
        dry_run: bool,
        /// Drop annotations that are absent from the manifest.
        #[arg(long)]
        prune: bool,
    },
    /// Detect silence (or sound) and print candidate ranges.
    Silence {
        /// RMS threshold in dBFS; below this is silence.
        #[arg(long, default_value = "-50")]
        threshold_dbfs: f32,
        /// Ignore runs shorter than this many seconds.
        #[arg(long, default_value = "0.35")]
        min_len: f64,
        /// Shrink each run by this many seconds on both sides.
        #[arg(long, default_value = "0.05")]
        pad: f64,
        /// Report runs of sound instead of runs of silence.
        #[arg(long)]
        sound: bool,
        /// Output `table` (human) or `json` (agent).
        #[arg(long, default_value = "table")]
        format: Format,
        /// Write the results into the session as `auto:*` annotations.
        #[arg(long)]
        write: bool,
    },
    /// Peak-normalize the audio to a target level.
    Normalize {
        /// Output path (required — the source file is never modified).
        #[arg(long, short = 'o')]
        out: PathBuf,
        /// Target peak in dBFS. Negative, hence `allow_hyphen_values`.
        #[arg(long, default_value = "-1", allow_hyphen_values = true)]
        target_dbfs: f32,
    },
    /// Apply a fixed gain in decibels.
    Gain {
        /// Decibels to apply; negative attenuates.
        #[arg(long, allow_hyphen_values = true)]
        db: f32,
        /// Output path (required — the source file is never modified).
        #[arg(long, short = 'o')]
        out: PathBuf,
    },
    /// Remove a range and close the gap. The inverse of `export`.
    Cut {
        /// Range to remove, as `START-END`.
        #[arg(long)]
        range: String,
        /// Output path (required — the source file is never modified).
        #[arg(long, short = 'o')]
        out: PathBuf,
        /// Fade in/out length in milliseconds applied at the join.
        #[arg(long, default_value = "5")]
        fade_ms: f64,
    },
    /// Split the audio into parts, or at explicit timestamps.
    Split {
        /// Output directory.
        #[arg(long, short = 'o', default_value = "wavemark-split")]
        out: PathBuf,
        /// Split into N equal parts.
        #[arg(long)]
        parts: Option<usize>,
        /// Comma-separated cut times (`M:SS.mmm` or seconds).
        #[arg(long, value_delimiter = ',')]
        at: Vec<String>,
    },
    /// Concatenate files. Sample rates must match.
    Concat {
        /// Files to join, in order.
        #[arg(required = true)]
        files: Vec<PathBuf>,
        /// Output path.
        #[arg(long, short = 'o', required = true)]
        out: PathBuf,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum Format {
    Json,
    Markdown,
    Table,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // `concat` works on bare files and needs no session at all.
    if let Command::Concat { files, out } = &cli.command {
        return concat_cmd(files, out);
    }

    // Sessions are resolved lazily. Several commands (silence --write, apply,
    // info on a fresh file) legitimately run against audio that has no sidecar
    // yet and create one; demanding a session up front would block those.
    let mut session: Option<Session> = resolve_session(&cli).ok();
    let target = session_target(&cli)?;

    // Commands that read annotations need one to exist already.
    fn need(s: &Option<Session>) -> Result<&Session> {
        s.as_ref().ok_or_else(|| {
            anyhow!(
                "no session found for this audio: mark something in the GUI, \
                 or run `wavemark silence --write` to generate one"
            )
        })
    }

    let cli_audio = cli.audio.clone();
    match &cli.command {
        Command::Annotations { format } => print_annotations(need(&session)?, *format),
        Command::Export {
            id,
            range,
            out,
            fade_ms,
        } => {
            let audio = audio_path(cli_audio.as_deref(), session.as_ref())?;
            export_cmd(
                &audio,
                need(&session)?,
                id.clone(),
                range.clone(),
                out.clone(),
                *fade_ms,
            )
        }
        Command::BatchExport {
            out,
            fade_ms,
            name_by_text,
        } => {
            let audio = audio_path(cli_audio.as_deref(), session.as_ref())?;
            batch_export_cmd(&audio, need(&session)?, out, *fade_ms, *name_by_text)
        }
        Command::Apply {
            manifest,
            dry_run,
            prune,
        } => apply_cmd(
            &mut session,
            &target,
            cli_audio.as_deref(),
            manifest,
            *dry_run,
            *prune,
        ),
        Command::Silence {
            threshold_dbfs,
            min_len,
            pad,
            sound,
            format,
            write,
        } => {
            let audio = audio_path(cli_audio.as_deref(), session.as_ref())?;
            silence_cmd(
                &mut session,
                &target,
                &audio,
                SilenceOptions {
                    threshold_dbfs: *threshold_dbfs,
                    min_len_sec: *min_len,
                    pad_sec: *pad,
                },
                *sound,
                *format,
                *write,
            )
        }
        Command::Normalize { out, target_dbfs } => {
            let audio = audio_path(cli_audio.as_deref(), session.as_ref())?;
            normalize_cmd(&audio, out, *target_dbfs)
        }
        Command::Gain { db, out } => {
            let audio = audio_path(cli_audio.as_deref(), session.as_ref())?;
            gain_cmd(&audio, out, *db)
        }
        Command::Cut {
            range,
            out,
            fade_ms,
        } => {
            let audio = audio_path(cli_audio.as_deref(), session.as_ref())?;
            cut_cmd(&audio, range, out, *fade_ms)
        }
        Command::Split { out, parts, at } => {
            let audio = audio_path(cli_audio.as_deref(), session.as_ref())?;
            split_cmd(&audio, session.as_ref(), out, *parts, at)
        }
        Command::Info => {
            // `info` is usually the very first thing anyone runs against a new
            // file, so it must work before a sidecar exists — synthesise the
            // session from the audio rather than demanding one.
            let audio = audio_path(cli_audio.as_deref(), session.as_ref())?;
            let s = ensure_session(&mut session, &audio)?;
            print_info(s)
        }
        Command::List => print_list(need(&session)?),
        Command::Concat { .. } => unreachable!("handled above"),
    }
}

/// Where the session file lives, so `apply` / `silence --write` can write back
/// to the same place they read from.
fn session_target(cli: &Cli) -> Result<PathBuf> {
    if let Some(p) = &cli.session {
        return Ok(p.clone());
    }
    if let Some(audio) = &cli.audio {
        if let Some(sidecar) = Session::resolve_sidecar(audio) {
            return Ok(sidecar);
        }
        return Ok(Session::sidecar_path(audio));
    }
    let found: Vec<PathBuf> = std::fs::read_dir(".")
        .map_err(|e| anyhow!("reading cwd: {e}"))?
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.ends_with(".wavemark.json"))
        })
        .map(|e| e.path())
        .collect();
    match found.len() {
        1 => Ok(found.into_iter().next().expect("len == 1")),
        0 => bail!("no session to write; pass --session PATH"),
        _ => bail!("multiple sessions in cwd; pass --session to disambiguate"),
    }
}

/// Where to write a session change. Kept next to `main` because every
/// mutating command needs the same answer.
fn persist(session: &Session, path: &Path) -> Result<()> {
    session
        .save_atomic(path)
        .with_context(|| format!("writing session {}", path.display()))
}

/// Find and load the session file from `--session`, the audio sidecar, or cwd.
fn resolve_session(cli: &Cli) -> Result<Session> {
    if let Some(p) = &cli.session {
        return Session::load(p).with_context(|| format!("loading session {}", p.display()));
    }
    if let Some(audio) = &cli.audio {
        if let Some(sidecar) = Session::resolve_sidecar(audio) {
            return Session::load(&sidecar)
                .with_context(|| format!("loading sidecar {}", sidecar.display()));
        }
    }
    // Fall back: exactly one *.wavemark.json in cwd.
    let found: Vec<PathBuf> = std::fs::read_dir(".")
        .map_err(|e| anyhow!("reading cwd: {e}"))?
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.ends_with(".wavemark.json"))
        })
        .map(|e| e.path())
        .collect();
    if found.len() == 1 {
        return Session::load(&found[0]).with_context(|| format!("loading {}", found[0].display()));
    }
    if found.is_empty() {
        bail!(
            "no session found: pass --session PATH, or --audio FILE with a .wavemark.json sidecar"
        );
    }
    bail!("multiple sessions found in cwd; pass --session to disambiguate");
}

fn audio_path(cli_audio: Option<&Path>, session: Option<&Session>) -> Result<PathBuf> {
    if let Some(a) = cli_audio {
        return Ok(a.to_path_buf());
    }
    let Some(session) = session else {
        bail!("no audio file: pass --audio PATH");
    };
    let p = Path::new(&session.audio_path);
    if p.is_file() {
        return Ok(p.to_path_buf());
    }
    bail!(
        "audio file '{}' not found; pass --audio to point at it",
        session.audio_path
    );
}

/// Same as [`ensure_session`], but for callers that have already decoded the
/// audio and don't want to pay for it twice.
fn ensure_session_from<'a>(
    slot: &'a mut Option<Session>,
    audio: &Path,
    decoded: &wavemark_core::audio::DecodedAudio,
) -> &'a mut Session {
    if slot.is_none() {
        *slot = Some(Session::new(
            audio.display().to_string(),
            decoded.duration_sec,
            decoded.sample_rate,
            decoded.channels,
        ));
    }
    slot.as_mut().expect("just populated")
}

/// Get the session, creating one from the audio file if the sidecar doesn't
/// exist yet. Used by `apply` and `silence --write`, both of which can be the
/// first thing anyone runs against a file.
fn ensure_session<'a>(slot: &'a mut Option<Session>, audio: &Path) -> Result<&'a mut Session> {
    if slot.is_none() {
        let d = decode(audio).with_context(|| format!("decoding {}", audio.display()))?;
        *slot = Some(Session::new(
            audio.display().to_string(),
            d.duration_sec,
            d.sample_rate,
            d.channels,
        ));
    }
    Ok(slot.as_mut().expect("just populated"))
}

// ---- annotations: the AI-agent payload --------------------------------------

#[derive(Serialize)]
struct PayloadAudio<'a> {
    path: &'a str,
    duration_sec: f64,
    sample_rate: u32,
    channels: u16,
}

#[derive(Serialize)]
struct PayloadAnnotation<'a> {
    id: &'a str,
    start_sec: f64,
    end_sec: f64,
    duration_sec: f64,
    start: String,
    end: String,
    text: &'a str,
    tags: &'a [String],
}

#[derive(Serialize)]
struct Payload<'a> {
    schema: &'a str,
    audio: PayloadAudio<'a>,
    annotations: Vec<PayloadAnnotation<'a>>,
}

fn build_payload(session: &Session) -> Payload<'_> {
    let annotations = session
        .annotations
        .iter()
        .map(|a| PayloadAnnotation {
            id: &a.id,
            start_sec: a.range.start,
            end_sec: a.range.end,
            duration_sec: a.range.duration(),
            start: format_ms(a.range.start),
            end: format_ms(a.range.end),
            text: &a.text,
            tags: &a.tags,
        })
        .collect();
    Payload {
        schema: "wavemark/annotations",
        audio: PayloadAudio {
            path: &session.audio_path,
            duration_sec: session.duration_sec,
            sample_rate: session.sample_rate,
            channels: session.channels,
        },
        annotations,
    }
}

fn print_annotations(session: &Session, format: Format) -> Result<()> {
    match format {
        Format::Json => {
            let json = serde_json::to_string_pretty(&build_payload(session))?;
            println!("{json}");
        }
        Format::Table => bail!("`annotations` supports json or markdown; use `list` for a table"),
        Format::Markdown => {
            println!("# wavemark annotations");
            println!();
            println!(
                "- audio: `{}` — {:.3}s · {} Hz · {} ch",
                session.audio_path, session.duration_sec, session.sample_rate, session.channels
            );
            println!();
            if session.annotations.is_empty() {
                println!("_no annotations_");
            } else {
                println!("| # | id | start | end | dur | text | tags |");
                println!("|---|---|---|---|---|---|---|");
                for (i, a) in session.annotations.iter().enumerate() {
                    println!(
                        "| {} | {} | {} | {} | {:.3}s | {} | {} |",
                        i + 1,
                        a.id,
                        format_ms(a.range.start),
                        format_ms(a.range.end),
                        a.range.duration(),
                        a.text.replace('|', "\\|"),
                        a.tags.join(", "),
                    );
                }
            }
        }
    }
    Ok(())
}

// ---- export -----------------------------------------------------------------

fn parse_range(s: &str) -> Result<TimeRange> {
    let (l, r) = s
        .split_once('-')
        .ok_or_else(|| anyhow!("range must be `START-END`"))?;
    let start = parse_ts(l.trim()).ok_or_else(|| anyhow!("bad start: {l}"))?;
    let end = parse_ts(r.trim()).ok_or_else(|| anyhow!("bad end: {r}"))?;
    Ok(TimeRange::new(start, end))
}

fn export_cmd(
    audio: &Path,
    session: &Session,
    id: Option<String>,
    range: Option<String>,
    out: Option<PathBuf>,
    fade_ms: f64,
) -> Result<()> {
    let range = match (id, range) {
        (Some(id), _) => {
            let ann = session
                .annotation(&id)
                .ok_or_else(|| anyhow!("no annotation with id {id}"))?;
            ann.range
        }
        (None, Some(r)) => parse_range(&r)?,
        (None, None) => bail!("pass --id ID or --range START-END"),
    };

    let decoded = decode(audio).with_context(|| format!("decoding {}", audio.display()))?;

    let stem = audio
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("segment");
    let out = out.unwrap_or_else(|| {
        PathBuf::from(format!("{stem}-{:.3}-{:.3}.wav", range.start, range.end))
    });

    export_range(
        &decoded.samples,
        decoded.sample_rate,
        decoded.channels,
        range,
        &out,
        fade_ms,
    )?;
    println!(
        "exported {} -> {} ({:.3}s)",
        format_ms(range.start),
        out.display(),
        range.duration()
    );
    Ok(())
}

// ---- batch export -----------------------------------------------------------

/// One entry in the batch manifest: the annotation plus where its audio landed.
#[derive(Serialize)]
struct ManifestEntry<'a> {
    id: &'a str,
    file: String,
    start_sec: f64,
    end_sec: f64,
    duration_sec: f64,
    start: String,
    end: String,
    text: &'a str,
    tags: &'a [String],
}

#[derive(Serialize)]
struct BatchManifest<'a> {
    schema: &'a str,
    audio: PayloadAudio<'a>,
    count: usize,
    annotations: Vec<ManifestEntry<'a>>,
}

pub const BATCH_SCHEMA: &str = "wavemark/batch-export";

/// Turn annotation text into something safe to use as a filename.
fn slug(text: &str, max: usize) -> String {
    let s: String = text
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    let s = s.trim_matches('-').to_lowercase();
    let s: String = s.chars().take(max).collect();
    if s.is_empty() {
        "untitled".to_string()
    } else {
        s.trim_end_matches('-').to_string()
    }
}

fn batch_export_cmd(
    audio: &Path,
    session: &Session,
    out_dir: &Path,
    fade_ms: f64,
    name_by_text: bool,
) -> Result<()> {
    if session.annotations.is_empty() {
        bail!("session has no annotations to export");
    }
    std::fs::create_dir_all(out_dir).with_context(|| format!("creating {}", out_dir.display()))?;

    let decoded = decode(audio).with_context(|| format!("decoding {}", audio.display()))?;
    let stem = audio
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("segment");

    let mut entries = Vec::with_capacity(session.annotations.len());
    for a in &session.annotations {
        let base = if name_by_text {
            format!("{}-{:03}-{}", stem, entries.len() + 1, slug(&a.text, 40))
        } else {
            format!("{}-{}", stem, slug(&a.id, 40))
        };
        let path = out_dir.join(format!("{base}.wav"));
        export_range(
            &decoded.samples,
            decoded.sample_rate,
            decoded.channels,
            a.range,
            &path,
            fade_ms,
        )
        .with_context(|| format!("exporting {} for {}", path.display(), a.id))?;

        entries.push(ManifestEntry {
            id: &a.id,
            file: path.display().to_string(),
            start_sec: a.range.start,
            end_sec: a.range.end,
            duration_sec: a.range.duration(),
            start: format_ms(a.range.start),
            end: format_ms(a.range.end),
            text: &a.text,
            tags: &a.tags,
        });
        println!(
            "  {} -> {} ({:.3}s)",
            a.id,
            path.display(),
            a.range.duration()
        );
    }

    let manifest = BatchManifest {
        schema: BATCH_SCHEMA,
        audio: PayloadAudio {
            path: &session.audio_path,
            duration_sec: session.duration_sec,
            sample_rate: session.sample_rate,
            channels: session.channels,
        },
        count: entries.len(),
        annotations: entries,
    };
    let mpath = out_dir.join("manifest.json");
    std::fs::write(&mpath, serde_json::to_string_pretty(&manifest)? + "\n")
        .with_context(|| format!("writing {}", mpath.display()))?;

    println!(
        "exported {} annotation(s) to {}/ and {}",
        session.annotations.len(),
        out_dir.display(),
        mpath.display()
    );
    Ok(())
}

// ---- apply: the agent -> GUI direction --------------------------------------

fn read_manifest(path: &Path) -> Result<String> {
    if path == Path::new("-") {
        let mut s = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut s).context("reading stdin")?;
        Ok(s)
    } else {
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))
    }
}

fn apply_cmd(
    slot: &mut Option<Session>,
    target: &Path,
    cli_audio: Option<&Path>,
    manifest: &Path,
    dry_run: bool,
    prune: bool,
) -> Result<()> {
    let audio = audio_path(cli_audio, slot.as_ref())?;
    let session = ensure_session(slot, &audio)?;
    let raw = read_manifest(manifest)?;
    let doc: PatchDocument =
        serde_json::from_str(&raw).with_context(|| format!("parsing {}", manifest.display()))?;

    // Refuse anything we don't recognise rather than guessing. A schema
    // mismatch almost always means the agent produced the wrong shape, and
    // silently half-applying that is worse than failing.
    if doc.schema != PATCH_SCHEMA && doc.schema != "wavemark/annotations" {
        bail!(
            "unrecognised schema {:?} (expected {:?} or \"wavemark/annotations\")",
            doc.schema,
            PATCH_SCHEMA
        );
    }
    if doc.annotations.is_empty() {
        println!("manifest contains no annotations; nothing to do");
        return Ok(());
    }

    let before = session.annotations.clone();
    let (report, rejections) = session.merge_patches(&doc.annotations);

    let pruned = if prune {
        let keep: HashSet<String> = doc
            .annotations
            .iter()
            .filter_map(|p| p.id.clone())
            .filter(|s| !s.trim().is_empty())
            .collect();
        session.retain_ids(&keep)
    } else {
        0
    };

    print_report(&report, pruned, &rejections, dry_run);

    if dry_run {
        println!(
            "\ndry run — {} unchanged ({} -> {} annotations)",
            target.display(),
            before.len(),
            session.annotations.len()
        );
        return Ok(());
    }

    persist(session, target)?;
    println!("\nwrote {}", target.display());
    if !rejections.is_empty() {
        // Partial success is still success, but a scripted caller deserves a
        // non-zero exit so it notices.
        std::process::exit(2);
    }
    Ok(())
}

fn print_report(
    r: &MergeReport,
    pruned: usize,
    rejections: &[wavemark_core::Rejection],
    dry_run: bool,
) {
    let verb = if dry_run { "would change" } else { "changed" };
    println!(
        "{} created  {} updated  {} unchanged  {} rejected{}",
        r.created,
        r.updated,
        r.unchanged,
        r.rejected,
        if pruned > 0 {
            format!("  {pruned} pruned")
        } else {
            String::new()
        }
    );
    if r.is_noop() && rejections.is_empty() {
        println!("nothing {verb}");
    }
    for rej in rejections {
        println!("  rejected [{}]: {}", rej.index, rej.reason);
    }
}

// ---- silence ----------------------------------------------------------------

fn silence_cmd(
    slot: &mut Option<Session>,
    target: &Path,
    audio: &Path,
    opts: SilenceOptions,
    want_sound: bool,
    format: Format,
    write: bool,
) -> Result<()> {
    let decoded = decode(audio).with_context(|| format!("decoding {}", audio.display()))?;
    // ~100 buckets/sec matches the GUI's own resolution.
    let peaks = decoded.peaks((decoded.duration_sec * 100.0).ceil() as usize);
    let spans = if want_sound {
        detect_sound(&peaks, &opts)
    } else {
        detect_silence(&peaks, &opts)
    };

    match format {
        Format::Table => {
            if spans.is_empty() {
                println!(
                    "no {} runs found (threshold {:.0} dBFS, min {:.2}s)",
                    if want_sound { "sound" } else { "silence" },
                    opts.threshold_dbfs,
                    opts.min_len_sec
                );
            } else {
                println!(
                    "{} {} run(s):",
                    spans.len(),
                    if want_sound { "sound" } else { "silence" }
                );
                for (i, s) in spans.iter().enumerate() {
                    println!(
                        "  {:>3}  {} – {}  ({:.3}s)",
                        i + 1,
                        format_ms(s.start),
                        format_ms(s.end),
                        s.duration()
                    );
                }
            }
        }
        Format::Json => {
            let out: Vec<_> = spans
                .iter()
                .map(|s| {
                    serde_json::json!({
                        "start_sec": s.start,
                        "end_sec": s.end,
                        "duration_sec": s.duration(),
                        "start": format_ms(s.start),
                        "end": format_ms(s.end),
                    })
                })
                .collect();
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "schema": "wavemark/spans",
                    "kind": if want_sound { "sound" } else { "silence" },
                    "threshold_dbfs": opts.threshold_dbfs,
                    "count": out.len(),
                    "spans": out,
                }))?
            );
        }
        Format::Markdown => bail!("`silence` supports table or json"),
    }

    if write {
        let session = ensure_session_from(slot, audio, &decoded);
        let tag = if want_sound {
            "auto:sound"
        } else {
            "auto:silence"
        };
        for s in &spans {
            let a = Annotation::new(TimeRange::new(s.start, s.end), String::new())
                .with_tags(vec![tag.to_string()]);
            session.add_annotation(a);
        }
        session.sort_annotations();
        persist(session, target)?;
        println!(
            "wrote {} {} annotation(s) to {}",
            spans.len(),
            tag,
            target.display()
        );
    }
    Ok(())
}

// ---- dsp commands -----------------------------------------------------------

/// Load audio and return it plus the frame bounds of an optional range.
fn load(audio: &Path) -> Result<(wavemark_core::audio::DecodedAudio, usize)> {
    let d = decode(audio).with_context(|| format!("decoding {}", audio.display()))?;
    let frames = if d.channels == 0 {
        0
    } else {
        d.samples.len() / d.channels as usize
    };
    Ok((d, frames))
}

fn normalize_cmd(audio: &Path, out: &Path, target_dbfs: f32) -> Result<()> {
    let (mut d, _) = load(audio)?;
    let before = dsp::peak_dbfs(&d.samples);
    let applied = dsp::normalize(d.samples_mut(), target_dbfs);
    write_wav(&d.samples, d.sample_rate, d.channels, out)?;
    println!(
        "normalized {:.1} dBFS -> {:.1} dBFS (applied {:+.2} dB) -> {}",
        before,
        dsp::peak_dbfs(&d.samples),
        applied,
        out.display()
    );
    Ok(())
}

fn gain_cmd(audio: &Path, out: &Path, db: f32) -> Result<()> {
    let (mut d, _) = load(audio)?;
    let clipped = dsp::apply_gain(d.samples_mut(), db);
    write_wav(&d.samples, d.sample_rate, d.channels, out)?;
    println!(
        "applied {:+.2} dB -> {} ({:.1} dBFS peak)",
        db,
        out.display(),
        dsp::peak_dbfs(&d.samples)
    );
    if clipped {
        eprintln!("warning: output clips (peaks exceed 0 dBFS)");
        std::process::exit(3);
    }
    Ok(())
}

fn cut_cmd(audio: &Path, range: &str, out: &Path, fade_ms: f64) -> Result<()> {
    let (d, total_frames) = load(audio)?;
    let r = parse_range(range)?;
    let chans = d.channels.max(1) as usize;
    let a = frame_at(r.start, d.sample_rate, total_frames);
    let b = frame_at(r.end, d.sample_rate, total_frames);
    if b <= a {
        bail!("empty range {}–{}", format_ms(r.start), format_ms(r.end));
    }
    let mut out_samples = d.samples[..a * chans].to_vec();
    out_samples.extend_from_slice(&d.samples[b * chans..]);

    // Fade the join so removing a chunk doesn't leave a click at the seam.
    let fade_n = ((fade_ms / 1000.0) * d.sample_rate as f64).round() as usize;
    if fade_n > 0 {
        let frames = out_samples.len() / chans;
        let fade_n = fade_n.min(frames / 2).max(1);
        for f in 0..fade_n {
            let g = f as f32 / fade_n as f32;
            for c in 0..chans {
                let i = (a.saturating_sub(fade_n) + f) * chans + c;
                if i < out_samples.len() {
                    out_samples[i] *= g;
                }
            }
        }
    }

    write_wav(&out_samples, d.sample_rate, d.channels, out)?;
    let removed = (b - a) as f64 / d.sample_rate as f64;
    println!(
        "cut {:.3}s ({} – {}) -> {} ({:.3}s remaining)",
        removed,
        format_ms(r.start),
        format_ms(r.end),
        out.display(),
        (total_frames - (b - a)) as f64 / d.sample_rate as f64
    );
    Ok(())
}

fn split_cmd(
    audio: &Path,
    session: Option<&Session>,
    out_dir: &Path,
    parts: Option<usize>,
    at: &[String],
) -> Result<()> {
    if parts.is_none() && at.is_empty() {
        bail!("pass --parts N or --at TIMES");
    }
    let (d, total_frames) = load(audio)?;
    let chans = d.channels.max(1) as usize;

    let cuts: Vec<usize> = if !at.is_empty() {
        let times: Result<Vec<f64>> = at
            .iter()
            .map(|s| parse_ts(s).ok_or_else(|| anyhow!("bad time: {s}")))
            .collect();
        dsp::cut_points(&times?, d.sample_rate, total_frames)
    } else {
        // --parts N: cuts every duration/N seconds.
        let n = parts.unwrap_or(1).max(1);
        let step = total_frames / n;
        (1..n).map(|k| k * step).filter(|c| *c > 0).collect()
    };

    let segments = dsp::slice_and_split(&d.samples, chans, 0, total_frames, &cuts);
    std::fs::create_dir_all(out_dir).with_context(|| format!("creating {}", out_dir.display()))?;
    let stem = audio
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("segment");

    for (i, seg) in segments.iter().enumerate() {
        let path = out_dir.join(format!("{stem}-{:03}.wav", i + 1));
        write_wav(seg, d.sample_rate, d.channels, &path)?;
        let secs = seg.len() as f64 / chans as f64 / d.sample_rate as f64;
        println!("  {} ({:.3}s)", path.display(), secs);
    }
    println!(
        "split into {} part(s) in {}/",
        segments.len(),
        out_dir.display()
    );

    // Remind the user that annotation timestamps refer to the *original* file.
    if session.is_some_and(|s| !s.annotations.is_empty()) && !cuts.is_empty() {
        println!(
            "note: {} annotation(s) still reference the original timeline",
            session.map(|s| s.annotations.len()).unwrap_or(0)
        );
    }
    Ok(())
}

fn concat_cmd(files: &[PathBuf], out: &Path) -> Result<()> {
    if files.len() < 2 {
        bail!("concat needs at least two files");
    }
    let mut rate: Option<u32> = None;
    let mut chans: Option<u16> = None;
    let mut parts: Vec<Vec<f32>> = Vec::with_capacity(files.len());

    for f in files {
        let d = decode(f).with_context(|| format!("decoding {}", f.display()))?;
        match rate {
            Some(r) if r != d.sample_rate => bail!(
                "sample rate mismatch: {} is {} Hz, expected {} Hz",
                f.display(),
                d.sample_rate,
                r
            ),
            None => rate = Some(d.sample_rate),
            _ => {}
        }
        match chans {
            Some(c) if c != d.channels => bail!(
                "channel count mismatch: {} has {}, expected {}",
                f.display(),
                d.channels,
                c
            ),
            None => chans = Some(d.channels),
            _ => {}
        }
        parts.push(d.into_samples());
    }

    let refs: Vec<&[f32]> = parts.iter().map(|p| p.as_slice()).collect();
    let joined = dsp::concat(&refs);
    // rate/chans are Some because files.len() >= 2 guarantees a first decode.
    let rate = rate.expect("at least one file decoded");
    let chans = chans.expect("at least one file decoded");
    write_wav(&joined, rate, chans, out)?;
    let secs = joined.len() as f64 / chans.max(1) as f64 / rate as f64;
    println!(
        "concatenated {} file(s) -> {} ({:.3}s)",
        files.len(),
        out.display(),
        secs
    );
    Ok(())
}

// ---- info / list ------------------------------------------------------------

fn print_info(session: &Session) -> Result<()> {
    println!("audio:        {}", session.audio_path);
    println!(
        "duration:     {:.3} s ({})",
        session.duration_sec,
        format_ms(session.duration_sec)
    );
    println!("sample_rate:  {} Hz", session.sample_rate);
    println!("channels:     {}", session.channels);
    println!("annotations:  {}", session.annotations.len());
    Ok(())
}

fn print_list(session: &Session) -> Result<()> {
    if session.annotations.is_empty() {
        println!("(no annotations)");
        return Ok(());
    }
    for a in &session.annotations {
        let _ = a as &Annotation; // keep the import meaningful for greppers
        println!(
            "{}  {} – {}  {}",
            a.id,
            format_ms(a.range.start),
            format_ms(a.range.end),
            a.text
        );
    }
    Ok(())
}
