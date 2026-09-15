//! wavemark-core: the shared heart of the GUI and CLI.
//!
//! Everything here is pure data: the [TimeRange] math, the [Annotation] /
//! [Session] model (the `.wavemark.json` sidecar format), peak computation for
//! waveform rendering, and — behind the `audio` feature — the symphonia decoder
//! and hound WAV exporter.
//!
//! The GUI ([`wavemark-ui`](../wavemark_ui)) and the CLI
//! ([`wavemark-cli`](../wavemark_cli)) both depend on this crate, so the file
//! format and the audio engine have exactly one source of truth. That is what
//! makes the "mark it in the GUI, read it from the terminal" loop reliable.

pub mod model;
pub mod peaks;
pub mod time;

#[cfg(feature = "audio")]
pub mod audio;

pub use model::{Annotation, Session, TimeRange};
pub use peaks::{Peak, Peaks};
pub use time::{format_hms, format_ms};
