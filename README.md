# wavemark

**A tiny, GPU-accelerated audio editor that is designed to be driven by an AI agent.**

You mark things up in a graphical editor — select a range, type what's wrong with
it, hit save. Then in your terminal you run one command and get every annotation
back with its exact time range, ready to pipe into an agent:

```sh
wavemark annotations -f interview.wav --format json | your-agent
```

That's the whole idea. Humans are good at *seeing* where the problem is. Agents
are good at *fixing* it, but only if you can tell them where to look. wavemark
is the bridge: a GUI for the seeing, a CLI for the telling.

---

## The loop

```text
   ┌──────────────── 🎧 GUI (gpui-kit, GPU) ────────────────┐
   │  big waveform    →  find the exact range               │
   │  overview bar    →  find *roughly* where the sound is  │
   │  annotate        →  "clipping here", "cut this"        │
   └──────────────────────────┬─────────────────────────────┘
                              │ writes
                              ▼
                  interview.wav.wavemark.json        ← one shared source of truth
                              │
                              ▼  reads
   ┌──────────────── ⌨️ CLI (`wavemark`) ───────────────────┐
   │  annotations --format json | markdown                  │
   │  export --id a1f3 -o clip.wav                          │
   └──────────────────────────┬─────────────────────────────┘
                              │ pipes into
                              ▼
                       🤖 your AI agent
```

The GUI and the CLI never talk to each other directly — they share a plain JSON
sidecar next to the audio file. That means annotations made in the GUI are
immediately visible in the terminal (and to any other tool), with no daemon, no
database, and no lock-in.

## Why two waveforms?

Because one is not enough. The **overview bar** at the bottom shows the entire
file, so you can see at a glance where the loud parts are and jump straight to
them. The **big canvas** in the middle shows a zoomed window, so you can trim a
selection to the exact millisecond.

Roughly: overview bar = *where*, big canvas = *exactly where*.

## Quick start

```sh
git clone https://github.com/<owner>/wavemark
cd wavemark

cargo build --release                 # both the GUI and the CLI

./target/release/wavemark-ui          # open the editor (or double-click it)
./target/release/wavemark --help      # the CLI
```

Making your first annotation:

1. `wavemark-ui` → **Open…** → pick `interview.wav`
2. Drag across the interesting part of the big waveform
3. Type `speaker talks over the guest` → **Add annotation**
4. Back in the terminal:

```sh
$ wavemark annotations -f interview.wav --format markdown

# wavemark annotations

- audio: `interview.wav` — 3724.800s · 48000 Hz · 2 ch

| # | id   | start      | end        | dur    | text                          | tags           |
|---|------|------------|------------|--------|-------------------------------|----------------|
| 1 | a1f3 | 0:12.345   | 0:15.678   | 3.333s | speaker talks over the guest  | crosstalk      |
```

## The AI-agent payload

`--format json` is what you actually pipe into an agent. It is deliberately
flat, self-describing, and includes both raw seconds and a human-readable
timestamp, so an agent never has to guess the units:

```json
{
  "schema": "wavemark/annotations",
  "audio": {
    "path": "interview.wav",
    "duration_sec": 3724.8,
    "sample_rate": 48000,
    "channels": 2
  },
  "annotations": [
    {
      "id": "a1f3",
      "start_sec": 12.345,
      "end_sec": 15.678,
      "duration_sec": 3.333,
      "start": "0:12.345",
      "end": "0:15.678",
      "text": "speaker talks over the guest",
      "tags": ["crosstalk"]
    }
  ]
}
```

Give an agent this plus the exported audio and it has everything: *what* to
change, *where*, and the actual samples.

```sh
# hand the agent the segments you marked
wavemark annotations -f interview.wav | my-agent --audio interview.wav
```

## CLI reference

| command | what it does |
|---|---|
| `wavemark annotations [-f AUDIO] [--format json\|markdown]` | print every annotation with exact ranges — **the agent payload** |
| `wavemark export (-f AUDIO) (--id ID \| --range START-END) [-o OUT] [--fade-ms N]` | write a range to 32-bit float WAV |
| `wavemark info` | duration, sample rate, channels, annotation count |
| `wavemark list` | one annotation per line |

Global flags: `--session PATH` (explicit `.wavemark.json`), `--audio/-f PATH`
(the audio file, used to find the sidecar and to export).

**Session resolution** — if you don't pass `--session`, wavemark looks for
`AUDIO.wavemark.json` next to the audio file, then for a single
`*.wavemark.json` in the current directory. So in practice, `cd` into the
folder and just run `wavemark annotations -f interview.wav`.

Timestamps accept either form: `0:12.345` or plain seconds `12.345`.

```sh
wavemark export -f interview.wav --range 0:12.345-0:15.678 -o crosstalk.wav
wavemark export -f interview.wav --id a1f3            # the range of annotation a1f3
```

Exports are 32-bit float WAV with a short (default 5 ms) linear fade at each
end, so cuts don't pop.

## The session file

```json
{
  "version": 1,
  "audio_path": "interview.wav",
  "duration_sec": 3724.8,
  "sample_rate": 48000,
  "channels": 2,
  "annotations": [
    {
      "id": "a1f3",
      "range": { "start": 12.345, "end": 15.678 },
      "text": "speaker talks over the guest",
      "tags": ["crosstalk"],
      "color": null,
      "created_at": "2026-09-16T10:11:12+08:00"
    }
  ],
  "last_selection": { "start": 12.345, "end": 15.678 }
}
```

It sits next to the audio as a sidecar (`interview.wav` →
`interview.wav.wavemark.json`), it is plain JSON, and it is versioned so the
schema can evolve without breaking agents that pinned an older one.

## Architecture

Three crates, because the data model must be shared but the GUI must not drag
the CLI down with it:

| crate | what it is |
|---|---|
| `wavemark-core` | the data model (`TimeRange`, `Annotation`, `Session`), peak math, and — behind the `audio` feature — the symphonia decoder and hound exporter |
| `wavemark-cli` | the `wavemark` binary: annotations, export, info, list |
| `wavemark-ui` | the gpui-kit GUI |

```text
wavemark-core  ──┬── wavemark-cli   (adds clap)
                 └── wavemark-ui    (adds gpui-kit)
```

The waveform is not a bitmap. Both the main canvas and the overview bar are
rows of thin GPU-composited elements whose heights come from the peak data, so
zooming and scrolling stay smooth on long files. Peaks are computed once
(min/max/RMS per bucket) and down-sampled for whichever view is asking.

## Building

Everything except the GUI is pure Rust and builds anywhere:

```sh
cargo build -p wavemark-core -p wavemark-cli
cargo test -p wavemark-core
```

The GUI needs a GPU, a recent stable Rust (gpui-kit tracks current stable —
older toolchains can trip on unstable features it depends on), and, on Linux,
the usual X11/Wayland dev packages:

```sh
# Debian / Ubuntu
sudo apt install -y \
  libxkbcommon-x11-dev libx11-dev libxext-dev libxft-dev libxinerama-dev \
  libxcursor-dev libxrender-dev libxfixes-dev libwayland-dev libxkbfile-dev \
  libssl-dev pkg-config cmake

cargo build -p wavemark-ui
```

On macOS it should just work (Metal). Windows is untested but gpui-kit supports
it.

## Roadmap

Open tickets on the issue tracker cover the rest, including:

- real audio playback through your speakers (the transport currently animates
  the playhead only)
- waveform channels rendered side by side for stereo files
- undo / redo
- batch export of every annotation at once
- drag-to-move annotation edges

## License

MIT. See [LICENSE](./LICENSE).
