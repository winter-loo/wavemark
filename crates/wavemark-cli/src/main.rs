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

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};
use serde::Serialize;
use wavemark_core::audio::{decode, export_range};
use wavemark_core::time::{format_ms, parse_ts};
use wavemark_core::{Annotation, Session, TimeRange};

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
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum Format {
    Json,
    Markdown,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let session = resolve_session(&cli)?;
    let cli_audio = cli.audio.as_deref();
    match &cli.command {
        Command::Annotations { format } => print_annotations(&session, *format),
        Command::Export {
            id,
            range,
            out,
            fade_ms,
        } => {
            let audio = audio_path(cli_audio, &session)?;
            export_cmd(
                &audio,
                &session,
                id.clone(),
                range.clone(),
                out.clone(),
                *fade_ms,
            )
        }
        Command::Info => print_info(&session),
        Command::List => print_list(&session),
    }
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

fn audio_path(cli_audio: Option<&Path>, session: &Session) -> Result<PathBuf> {
    if let Some(a) = cli_audio {
        return Ok(a.to_path_buf());
    }
    let p = Path::new(&session.audio_path);
    if p.is_file() {
        return Ok(p.to_path_buf());
    }
    bail!(
        "audio file '{}' not found; pass --audio to point at it",
        session.audio_path
    );
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
