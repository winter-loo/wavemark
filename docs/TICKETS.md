# Tickets

Source of truth for this file is [`scripts/tickets.json`](../scripts/tickets.json). Edit that, then re-run `python3 scripts/render-tickets.py`.

Issue numbers below assume creation order on a fresh repository (1..27).

## v0.1 — Core loop  —  21/21 closed

| # | Ticket | Labels | Blocked by | Status |
| -:| ------ | ------ | ---------- | ------ |
| 1 | [[infra] Bootstrap the Cargo workspace (wavemark-core / wavemark-cli / wavemark-ui)](#1-workspace) | `infra` `core` | — | done |
| 2 | [[infra] CI: build, test, clippy -D warnings, fmt --check](#2-ci) | `infra` | #1 | done |
| 3 | [[core] TimeRange / Annotation / Session data model](#3-model) | `core` `ai-bridge` | #1 | done |
| 4 | [[core] Sidecar .wavemark.json — one shared source of truth for GUI and CLI](#4-sidecar) | `core` `ai-bridge` | #3 | done |
| 5 | [[audio] Decode mp3 / wav / flac / aac / ogg / m4a via symphonia](#5-decode) | `audio` `core` | #1 | done |
| 6 | [[audio] Peak extraction (min / max / RMS buckets) for a GPU-composited waveform](#6-peaks) | `audio` | #5 | done |
| 7 | [[audio] Export a selected range to 32-bit float WAV with fade in/out](#7-export) | `audio` `cli` | #5 | done |
| 8 | [[core] Timestamp formatting and parsing (M:SS.mmm / H:MM:SS.mmm) + unit tests](#8-time) | `core` | #1 | done |
| 9 | [[gui] Main waveform canvas — large centre display, GPU-composited](#9-main-waveform) | `gui` | #6 | done |
| 10 | [[gui] Bottom overview waveform bar — find where the sound is](#10-overview) | `gui` | #6 | done |
| 11 | [[gui] Link the two waveforms: overview -> main canvas navigation](#11-link-views) | `gui` | #9, #10 | done |
| 12 | [[gui] Drag-to-select a range + selection overlay](#12-select) | `gui` | #9 | done |
| 13 | [[gui] Wheel zoom around the cursor anchor, clamped to file bounds](#13-zoom) | `gui` | #9 | done |
| 14 | [[gui] Annotation markers on the waveform + annotation list panel](#14-annotate-ui) | `gui` `ai-bridge` | #4, #12 | done |
| 15 | [[gui] Transport: play / pause / stop / seek + playhead rendering](#15-transport) | `gui` | #9 | done |
| 16 | [[cli] `wavemark info` — duration, sample rate, channels](#16-cli-info) | `cli` `ai-bridge` | #4 | done |
| 17 | [[cli] `wavemark annotations --format json|markdown` — the AI-agent bridge](#17-cli-annotations) | `cli` `ai-bridge` | #4 | done |
| 18 | [[cli] `wavemark export --id ID | --range START-END [-o OUT] [--fade-ms N]`](#18-cli-export) | `cli` | #7 | done |
| 19 | [[cli] Sidecar auto-resolution: --session, next to audio, or single file in cwd](#19-cli-resolve) | `cli` `ai-bridge` | #4 | done |
| 20 | [[ai-bridge] Stable `wavemark/annotations` JSON payload schema](#20-payload) | `ai-bridge` `cli` | #17 | done |
| 21 | [[docs] README: the human <-> AI loop, CLI reference, architecture](#21-docs) | `docs` | #17 | done |

## v0.2 — AI round-trip  —  0/2 closed

| # | Ticket | Labels | Blocked by | Status |
| -:| ------ | ------ | ---------- | ------ |
| 22 | [[ai-bridge] `wavemark batch-export` — every annotation to its own WAV + one manifest](#22-batch-export) | `ai-bridge` `cli` `enhancement` | #20, #18 | open |
| 23 | [[ai-bridge] `wavemark apply` — merge an agent-produced manifest back into the session](#23-apply) | `ai-bridge` `cli` `enhancement` | #20 | open |

## v0.3 — Everyday editing  —  0/4 closed

| # | Ticket | Labels | Blocked by | Status |
| -:| ------ | ------ | ---------- | ------ |
| 24 | [[enhancement] Silence detection -> auto-generate candidate annotations](#24-silence) | `enhancement` `audio` `ai-bridge` | #6 | open |
| 25 | [[enhancement] Everyday DSP: normalize / gain, trim, split, concatenate](#25-dsp) | `enhancement` `audio` `cli` | #7 | open |
| 26 | [[enhancement] Keyboard shortcuts + undo/redo](#26-kbd-undo) | `enhancement` `gui` | #12, #14 | open |
| 27 | [[enhancement] Real audio output (rodio/cpal) — make playback audible](#27-audio-out) | `enhancement` `audio` `gui` | #15 | open |

---

## Detail

### #1 — [infra] Bootstrap the Cargo workspace (wavemark-core / wavemark-cli / wavemark-ui)

`workspace` · milestone **v0.1 — Core loop** · **done**

## Why

Three crates with hard dependency direction `ui -> core <- cli`.

The separation matters for one specific reason: **`wavemark-core` must never depend on a GUI toolkit.** The CLI has to read exactly the same session file the GUI writes, from a headless box with no display. If core pulls in gpui, `wavemark annotations` stops working over SSH — and that command is the entire point of the project.

## Scope

- [x] Workspace root `Cargo.toml`, `resolver = "2"`, edition 2021, MSRV 1.80
- [x] `wavemark-core` — model + peaks + time + audio (audio gated behind a feature flag so the pure model stays dependency-light)
- [x] `wavemark-cli` — the agent-facing binary
- [x] `wavemark-ui` — the gpui-kit binary
- [x] `rustfmt.toml`, `.gitignore`, MIT `LICENSE`

## Acceptance

`cargo check --workspace` passes; `wavemark-core` compiles with no GUI dependency in its tree.

## Implementation notes

`wavemark-core` uses `default = []` with `audio = ["dep:symphonia", "dep:hound"]`. A consumer that only wants to parse `.wavemark.json` pays for `serde` and nothing else.

*Blocks #2, #3, #5, #8.*

### #2 — [infra] CI: build, test, clippy -D warnings, fmt --check

`ci` · milestone **v0.1 — Core loop** · **done**

## Why

This repo is meant to be extended by agents. A green check on every push is the cheapest possible guardrail against an agent (or a human) quietly breaking the session format.

## Scope

- [x] `.github/workflows/ci.yml` on push + PR to `main`
- [x] `cargo build` + `cargo test` for `wavemark-core` and `wavemark-cli`
- [x] `cargo clippy --all-features -- -D warnings`
- [x] `cargo fmt --check`
- [x] dependency caching

## Open

The `wavemark-ui` crate is not built in CI yet — gpui-kit needs system libraries (X11/wayland, vulkan/metal toolchain) that vary per runner. Needs a containerised or matrix build. Tracked separately.

*Blocked by #1.*

### #3 — [core] TimeRange / Annotation / Session data model

`model` · milestone **v0.1 — Core loop** · **done**

## Why

An annotation is worthless to an agent without an exact time range. So the range is not a property of the annotation — **the annotation *is* a range plus a note.** That is the whole model.

## Scope

- [x] `TimeRange { start, end }` with `new()` normalising reversed input, `duration()`, `contains()` (half-open), `overlaps()`, `clamp_to()`
- [x] `Annotation { id, range, text, tags, color, created_at }` — `id` is a uuid v4 so an agent can reference one stably across round-trips
- [x] `Session { version, audio_path, duration_sec, sample_rate, channels, annotations, last_selection }`
- [x] `SESSION_VERSION = 1` for forward-compatible migrations
- [x] Unit tests for normalisation, containment, overlap, clamping

## Acceptance

8/8 unit tests pass. `Annotation::new(range, text).with_tags([...])` reads well at the call site.

*Blocked by #1 · Blocks #4.*

### #4 — [core] Sidecar .wavemark.json — one shared source of truth for GUI and CLI

`sidecar` · milestone **v0.1 — Core loop** · **done**

## Why

This file is the product. The GUI writes it; the CLI reads it. No database, no daemon, no IPC — just a JSON file next to the audio.

```
interview.wav
interview.wav.wavemark.json   <- both sides touch only this
```

## Scope

- [x] `Session::sidecar_path()` — `song.wav` -> `song.wav.wavemark.json`
- [x] `Session::resolve_sidecar()` — locate the sidecar for a given audio file
- [x] `save()` / `load()` with pretty JSON and context-bearing errors
- [x] `add_annotation` / `remove_annotation` / `annotation(id)`
- [x] `last_selection` remembered so a re-open resumes where you left off

## Design decision

Sidecar rather than embedded metadata: the source file is never modified. An agent can rewrite the sidecar as many times as it likes and the original audio stays byte-identical.

*Blocked by #3 · Blocks #14, #16, #17, #19.*

### #5 — [audio] Decode mp3 / wav / flac / aac / ogg / m4a via symphonia

`decode` · milestone **v0.1 — Core loop** · **done**

## Why

Pure Rust, no C toolchain, no ffmpeg. `cargo build` and it decodes.

## Scope

- [x] `DecodedAudio { sample_rate, channels, duration_sec, samples }` (mono-mixed f32 interleaved)
- [x] Format probing with extension hint, codec registry lookup, full packet drain
- [x] `decode(path)` returning anyhow-contextualised errors

## Implementation notes

Pinned to **`symphonia 0.5`**. The 0.6 line moved `SampleBuffer` out of `core::sample`, renamed `make_decoder` -> `make_audio_decoder`, and reordered `FormatOptions` arguments. 0.5 is the version whose API matches the current published docs. Worth revisiting when the ecosystem settles.

Non-obvious bits that cost real time:
- `prober.format(&hint, mss, ...)` — hint is the **first** argument
- `SampleBuffer::new(frames as u64, *decoded.spec())` — takes duration *and* spec
- `buf.copy_interleaved_ref(decoded)` returns `()`, not a `Result`

*Blocked by #1 · Blocks #6, #7.*

### #6 — [audio] Peak extraction (min / max / RMS buckets) for a GPU-composited waveform

`peaks` · milestone **v0.1 — Core loop** · **done**

## Why

The waveform is **not a bitmap**. Computing min/max/RMS per bucket lets the UI draw the wave as a row of GPU-composited rectangles. Redrawing 600 rects every frame is free; re-rasterising a bitmap is not. It also means zoom is a different down-sampling of the same data, not a re-render.

## Scope

- [x] `Peak { min, max, rms }`, `Peaks { buckets, samples_per_bucket, sample_rate, channels }`
- [x] `from_interleaved()` — mono-mix frames, bucket them
- [x] `bucket_at()`, `duration()`
- [x] 100 buckets/second baseline; main canvas and overview bar down-sample from it

## Acceptance

Peak data computed once at load; both waveform views read from the same buffer at different resolutions.

*Blocked by #5 · Blocks #9, #10, #24.*

### #7 — [audio] Export a selected range to 32-bit float WAV with fade in/out

`export` · milestone **v0.1 — Core loop** · **done**

## Why

An agent asked to "fix this clip" needs an actual clip, not a timestamp in the abstract. And a hard-edged cut pops — a 5 ms linear fade is the difference between a usable stem and a click.

## Scope

- [x] `export_range(samples, sample_rate, channels, range, out, fade_ms)`
- [x] 32-bit float WAV via `hound` (no clipping on intermediate processing)
- [x] Linear fade-in / fade-out, clamped so short ranges can't overlap
- [x] Range clamped to file bounds

## Acceptance

`export --range 0.5-1.5` on a 2 s file produces a 1.000 s WAV.

*Blocked by #5 · Blocks #18, #25.*

### #8 — [core] Timestamp formatting and parsing (M:SS.mmm / H:MM:SS.mmm) + unit tests

`time` · milestone **v0.1 — Core loop** · **done**

## Why

Agents are markedly better at `"start": "0:01.234"` than at `"start": 1.234`. Human-readable timestamps in the payload remove a whole class of off-by-one-unit errors, and the parser accepts both forms on the way back in.

## Scope

- [x] `format_ms()` -> `M:SS.mmm`, upgrading to `H:MM:SS.mmm` past an hour
- [x] `parse_ts()` accepting `M:SS.mmm` or bare seconds
- [x] Round-trip and boundary tests

## Bug found and fixed

`format_ms` did `seconds.round() as u64` and then treated the value as milliseconds — every timestamp was 1000x too small. Now `(seconds * 1000.0).round() as u64`. Caught by the unit tests, not by inspection.

*Blocked by #1.*

### #9 — [gui] Main waveform canvas — large centre display, GPU-composited

`main-waveform` · milestone **v0.1 — Core loop** · **done**

## Why

The brief asked for a *大方* (generous) waveform in the centre. This is where precision happens: after the overview bar gets you to the right neighbourhood, this is where you land on the exact millisecond.

## Scope

- [x] Full-width bar row, symmetric about the centre line
- [x] Selection overlay drawn on top
- [x] Annotation markers
- [x] Playhead line
- [x] Theme-aware colours via `cx.theme()`

## Implementation notes

600 bars (`MAIN_BARS`), reduced from an initial 1200 — 1200 divs per frame was measurably less smooth while the playhead animated. Bar count is a constant in `state.rs` if you want to trade fidelity for frame time.

*Blocked by #6 · Blocks #11, #12, #13, #15.*

### #10 — [gui] Bottom overview waveform bar — find where the sound is

`overview` · milestone **v0.1 — Core loop** · **done**

## Why

The second waveform is the unusual bit of this design and the one worth defending.

A single zoomed waveform gives you precision *or* context, never both. Two waveforms split the job:

| View | Question it answers |
| --- | --- |
| bottom overview bar | *where in the file is there sound at all?* |
| big centre canvas | *exactly which milliseconds?* |

On a 40-minute interview, the overview bar is the only thing standing between you and scrolling.

## Scope

- [x] Always renders the entire file (never zooms)
- [x] 400 bars down-sampled from the same peak buffer
- [x] Current viewport window highlighted
- [x] Click / drag to jump and scrub

*Blocked by #6 · Blocks #11.*

### #11 — [gui] Link the two waveforms: overview -> main canvas navigation

`link-views` · milestone **v0.1 — Core loop** · **done**

## Why

Two waveforms that don't talk to each other are just two waveforms. The whole design hinges on coarse-to-fine:

```
overview:  "there's something around 12:30"
   |
   +-- click/drag
   v
main view: "it's 12:31.480 to 12:33.910"
```

## Scope

- [x] `focus(t)` centres the main view on a time
- [x] `time_at_frac()` / `frac_at_time()` map pixels <-> seconds
- [x] Viewport window indicator on the overview bar
- [x] Clicking an annotation in the list selects it and reveals it

## Acceptance

Clicking the overview bar recentres the main canvas on that time, keeping the current zoom level.

*Blocked by #9, #10.*

### #12 — [gui] Drag-to-select a range + selection overlay

`select` · milestone **v0.1 — Core loop** · **done**

## Why

Selection is the atom everything else is built on — export, annotate, and loop all consume it.

## Scope

- [x] `begin_drag` / `update_drag` / `end_drag` with a drag anchor
- [x] Reversed drags (right-to-left) normalise correctly
- [x] Sub-20 ms drags treated as "clear selection" rather than a degenerate range
- [x] Selection rendered as a translucent overlay on the main canvas
- [x] Selection persisted to `last_selection` in the sidecar

## Acceptance

Dragging backwards produces the same `TimeRange` as dragging forwards over the same span.

*Blocked by #9 · Blocks #14, #26.*

### #13 — [gui] Wheel zoom around the cursor anchor, clamped to file bounds

`zoom` · milestone **v0.1 — Core loop** · **done**

## Why

Zooming around the cursor — not around the centre — is what makes a waveform feel physical. The sample under your pointer stays under your pointer.

## Scope

- [x] `zoom_at(anchor_frac, factor)` keeps the anchor time stationary
- [x] Clamped to a 20 ms minimum span (no zooming past sample-level usefulness)
- [x] Clamped to file bounds at both ends
- [x] Edge correction so the view never scrolls past 0 or `duration`

## Acceptance

Zooming at the far right edge does not drift the viewport off the end of the file.

*Blocked by #9.*

### #14 — [gui] Annotation markers on the waveform + annotation list panel

`annotate-ui` · milestone **v0.1 — Core loop** · **done**

## Why

Where the note becomes data. Select a range, type what's wrong, hit save — and the sidecar updates, which means the CLI sees it immediately.

## Scope

- [x] Text input for note body, tag input (comma separated)
- [x] Markers drawn at annotation ranges on the main canvas
- [x] List panel with timestamps; click to select and reveal
- [x] Delete
- [x] Auto-save to sidecar on every mutation

## Design decision

Every mutation writes through to disk immediately. No "unsaved changes" state, no save button to forget. It also means an agent tailing the file sees annotations as you make them.

*Blocked by #4, #12 · Blocks #26.*

### #15 — [gui] Transport: play / pause / stop / seek + playhead rendering

`transport` · milestone **v0.1 — Core loop** · **done**

## Why

You often need to hear a boundary before you commit to it.

## Scope

- [x] `play()` / `pause()` / `stop()` / `seek(t)`
- [x] `tick(dt)` advancing the playhead, looping within the selection when one exists
- [x] `follow_playhead()` auto-scrolls the view to keep the playhead visible
- [x] Playhead line rendered on the main canvas and the overview bar

## Known limitation — read before closing

**There is no audio output device yet.** The transport advances playhead state and UI correctly but produces no sound. Real playback needs `rodio`/`cpal` wired to the decoded buffer. Tracked as a follow-up ticket.

*Blocked by #9 · Blocks #27.*

### #16 — [cli] `wavemark info` — duration, sample rate, channels

`cli-info` · milestone **v0.1 — Core loop** · **done**

## Why

The first thing an agent needs to know before it proposes an edit is how long the file is. Cheap, fast, no decoding of the full stream required beyond what's needed.

## Scope

- [x] Human-readable by default (`2.000 s (0:02.000), 44100 Hz, 1 ch, 2 annotations`)
- [x] Annotation count surfaced alongside audio metadata

## Acceptance

`wavemark info -f clip.wav` prints duration in both raw seconds and `M:SS.mmm`.

*Blocked by #4.*

### #17 — [cli] `wavemark annotations --format json|markdown` — the AI-agent bridge

`cli-annotations` · milestone **v0.1 — Core loop** · **done**

## Why

**This is the reason the project exists.**

Every annotation added in the GUI, with its exact time range, available in a terminal as machine-readable JSON. `wavemark annotations -f x.wav --format json | your-agent` is the entire human -> agent hand-off.

## Scope

- [x] `--format json` — stable payload, see the schema ticket
- [x] `--format markdown` — for pasting into a prompt or a PR
- [x] Both raw `start_sec` floats and formatted `start` / `end` strings in every record

## Acceptance

Selection `0.5 -> 1.0` round-trips as `start_sec: 0.5, start: "0:00.500", end: "0:01.000"`.

*Blocked by #4 · Blocks #20, #21.*

### #18 — [cli] `wavemark export --id ID | --range START-END [-o OUT] [--fade-ms N]`

`cli-export` · milestone **v0.1 — Core loop** · **done**

## Why

An agent can only act on audio it can read. Two entry points cover both workflows: `--id` for "that thing I marked", `--range` for "this window, computed elsewhere".

## Scope

- [x] `--id` resolves an annotation by uuid and exports its range
- [x] `--range 0.5-1.5` with `parse_ts()`-compatible bounds
- [x] `-o` output path
- [x] `--fade-ms` fade length

## Acceptance

`export --id a2` on a 0.600 s annotation produces a 0.600 s WAV.

*Blocked by #7 · Blocks #22.*

### #19 — [cli] Sidecar auto-resolution: --session, next to audio, or single file in cwd

`cli-resolve` · milestone **v0.1 — Core loop** · **done**

## Why

Friction here kills the workflow. An agent running `wavemark annotations` in a directory should not have to be told which session file to read.

## Scope

Resolution order:

1. explicit `--session <path>`
2. sidecar adjacent to `--audio` (`song.wav` -> `song.wav.wavemark.json`)
3. if exactly one `*.wavemark.json` exists in the cwd, use it
4. otherwise error with an actionable message

## Design note

Step 3 is deliberately strict: **exactly one**, not "the first one found". Silently picking a session in a directory with several would be a bad failure mode for an automated caller.

*Blocked by #4.*

### #20 — [ai-bridge] Stable `wavemark/annotations` JSON payload schema

`payload` · milestone **v0.1 — Core loop** · **done**

## Why

An agent is a consumer with no tolerance for surprises. Versioned, documented, redundant-by-design output.

## Scope

- [x] `schema` discriminator field (`"wavemark/annotations"`) so a consumer can validate what it got
- [x] audio context block (path, duration, sample rate, channels)
- [x] annotations array with `id`, `start_sec`, `end_sec`, `duration_sec`, `start`, `end`, `text`, `tags`

## Design decision

Both `start_sec` (float) and `start` (`"0:00.500"`) are present on every record. Redundant on purpose: the float is for computation, the string is for the model's comprehension and for echoing back in a prompt. Costs a few bytes, prevents a category of error.

*Blocked by #17 · Blocks #22, #23.*

### #21 — [docs] README: the human <-> AI loop, CLI reference, architecture

`docs` · milestone **v0.1 — Core loop** · **done**

## Why

The concept only lands if the first screen explains the loop. Someone should understand *why there is a CLI at all* within ten seconds.

## Scope

- [x] The loop diagram (GUI -> sidecar -> CLI -> agent)
- [x] "Why two waveforms?" — the design justification, not just a feature list
- [x] Quick start and build instructions
- [x] Annotated JSON payload example
- [x] Full CLI reference table
- [x] Sidecar format documentation
- [x] Crate architecture
- [x] Roadmap

## Open

No screenshots yet — `assets/` and `docs/` are empty. Needs a real GUI run, which needs a display.

*Blocked by #17.*

### #22 — [ai-bridge] `wavemark batch-export` — every annotation to its own WAV + one manifest

`batch-export` · milestone **v0.2 — AI round-trip** · **open**

## Why

The natural next step after single export. An agent working a 40-minute interview wants all twelve marked regions at once, each as its own file, plus one manifest tying them together — not twelve sequential invocations.

## Scope

- [ ] `wavemark batch-export -f interview.wav -o out/`
- [ ] One WAV per annotation, named by id (and optionally by slugified text)
- [ ] A single `manifest.json` carrying the full annotation payload plus per-file paths
- [ ] `--fade-ms` applied consistently
- [ ] Non-zero exit with a clear message when there are no annotations

## Notes

Should reuse the `wavemark/annotations` schema with a `files` array added rather than inventing a second format.

*Blocked by #20, #18.*

### #23 — [ai-bridge] `wavemark apply` — merge an agent-produced manifest back into the session

`apply` · milestone **v0.2 — AI round-trip** · **open**

## Why

Right now the loop is one-way: human annotates, agent reads. Closing it is what makes this genuinely collaborative rather than a fancy export button.

```
wavemark annotations -f x.wav --format json > notes.json
# ...agent reads, edits, adds its own findings...
wavemark apply -f x.wav notes.json
# annotations now visible in the GUI
```

## Scope

- [ ] Read a `wavemark/annotations` document
- [ ] Upsert by `id`; create when absent, update when present
- [ ] `--dry-run` printing a diff of what would change
- [ ] `--prune` to drop annotations missing from the manifest
- [ ] Atomic write, and refuse to write if `schema` is unrecognised
- [ ] Validate every range against the audio duration

## Design tension worth resolving

Concurrent edits: the GUI writes on every keystroke-ish mutation, an agent writes in batch. Needs either file locking or a merge that reasons about `created_at`. Decide before implementing.

*Blocked by #20.*

### #24 — [enhancement] Silence detection -> auto-generate candidate annotations

`silence` · milestone **v0.3 — Everyday editing** · **open**

## Why

The overview bar already shows you *where* the sound is. The obvious next move is to stop making you look.

RMS is already computed per bucket, so the detection itself is nearly free — the work is in the UX.

## Scope

- [ ] Threshold (dBFS) + minimum-length parameters
- [ ] Sliding-window scan over the existing `Peaks` buffer
- [ ] Emit candidate annotations tagged `auto:silence` for review
- [ ] CLI flag to dump them as JSON without touching the session
- [ ] Merge/debounce adjacent runs

## Design note

Generated annotations must be distinguishable from human ones — hence the `auto:` tag prefix. An agent should be able to tell what a person actually said.

*Blocked by #6.*

### #25 — [enhancement] Everyday DSP: normalize / gain, trim, split, concatenate

`dsp` · milestone **v0.3 — Everyday editing** · **open**

## Why

Exporting a marked range is the core loop, but it's the *only* edit right now. These are the operations people reach for next, and they're all cheap on a decoded f32 buffer.

## Scope

- [ ] `normalize` — peak-normalise to a target dBFS, optionally per-selection
- [ ] `gain` — apply fixed dB, with clip detection
- [ ] `trim` — keep a range, drop the rest (inverse of export)
- [ ] `split` — cut at a timestamp or at every annotation boundary
- [ ] `concat` — join files, with sample-rate agreement enforced
- [ ] All destructive operations write a new file; the source is never modified

## Notes

Should be CLI-first (agent-friendly) with GUI buttons added afterwards. Every one of these is a pure function over `&[f32]` — keep them in `wavemark-core`, not the CLI.

*Blocked by #7.*

### #26 — [enhancement] Keyboard shortcuts + undo/redo

`kbd-undo` · milestone **v0.3 — Everyday editing** · **open**

## Why

Marking up forty regions with the mouse is tedious. Keyboard-first editing is what makes the tool viable for real sessions.

## Scope

- [ ] `space` play/pause, `home`/`end` jump to start/end
- [ ] `+`/`-` zoom, arrows nudge the playhead
- [ ] `a` annotate current selection, `e` export it
- [ ] `j`/`k` next/previous annotation
- [ ] `delete` remove selected annotation
- [ ] `cmd/ctrl+z` / `shift+cmd/ctrl+z` undo/redo over session mutations

## Design note

Undo should snapshot the `Session` struct — it's small and `Clone`. Snapshotting audio samples would be absurd.

*Blocked by #12, #14.*

### #27 — [enhancement] Real audio output (rodio/cpal) — make playback audible

`audio-out` · milestone **v0.3 — Everyday editing** · **open**

## Why

Straightforward gap, documented rather than hidden: **the transport currently advances a playhead and draws it, but no sound comes out.**

The state machine (`play`/`pause`/`stop`/`seek`/`tick`) and all rendering are correct and complete. What's missing is a device.

## Scope

- [ ] Pick `rodio` (simpler) or `cpal` (more control)
- [ ] Stream the decoded buffer from an arbitrary offset
- [ ] Handle sample-rate conversion when the device rate differs from the file rate
- [ ] Respect the selection loop already implemented in `tick()`
- [ ] Gate behind a feature flag so a headless build stays dependency-light

## Acceptance

Pressing play produces audio, and the playhead stays in sync with what you hear.

*Blocked by #15.*

---

## Dependency graph

```text
  #2  ci                     <-- #1  workspace
  #3  model                  <-- #1  workspace
  #4  sidecar                <-- #3  model
  #5  decode                 <-- #1  workspace
  #6  peaks                  <-- #5  decode
  #7  export                 <-- #5  decode
  #8  time                   <-- #1  workspace
  #9  main-waveform          <-- #6  peaks
  #10 overview               <-- #6  peaks
  #11 link-views             <-- #9  main-waveform
  #11 link-views             <-- #10 overview
  #12 select                 <-- #9  main-waveform
  #13 zoom                   <-- #9  main-waveform
  #14 annotate-ui            <-- #4  sidecar
  #14 annotate-ui            <-- #12 select
  #15 transport              <-- #9  main-waveform
  #16 cli-info               <-- #4  sidecar
  #17 cli-annotations        <-- #4  sidecar
  #18 cli-export             <-- #7  export
  #19 cli-resolve            <-- #4  sidecar
  #20 payload                <-- #17 cli-annotations
  #21 docs                   <-- #17 cli-annotations
  #22 batch-export           <-- #20 payload
  #22 batch-export           <-- #18 cli-export
  #23 apply                  <-- #20 payload
  #24 silence                <-- #6  peaks
  #25 dsp                    <-- #7  export
  #26 kbd-undo               <-- #12 select
  #26 kbd-undo               <-- #14 annotate-ui
  #27 audio-out              <-- #15 transport
```
