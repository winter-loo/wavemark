# wavemark GUI review — Linux desktop edition

Review of `crates/wavemark-ui` at `v0.1.0-rc.1` (`baedc47`), read against the
published Linux binary.

**Method / caveat.** I installed Xvfb and ran the shipped `wavemark-ui` under
`llvmpipe` software rendering to look at it directly. The process starts clean
(no panic, no stderr), but no window maps to the X root and every capture came
back byte-identical to the blank desktop — software GL can't satisfy GPUI's
present path. So **everything below is read from the code, not from a
screenshot.** Layout maths, colour values and handler coverage are verified;
"does it *look* right" is not. That gap is itself finding L7.

---

## Resolved since this review (v0.1.0-rc.3)

| # | What changed |
|---|---|
| **P1** | Time ruler added above the main waveform, with tick precision that follows the zoom. |
| **P4** | **Real audio output.** New `crates/wavemark-ui/src/audio.rs` owns a rodio `MixerDeviceSink`. `play` now makes sound. |
| **P5** | **The device is the clock.** `tick(0.033)` is gone; `poll_playback()` reads `Player::get_pos()`, so the playhead cannot drift from the audio. |
| **P6** | **Scrubbing works** — drag on the time ruler seeks the live stream. |

### How P4 was verified without a sound card

The container has no `/dev/snd`, so `~/.asoundrc` was pointed at ALSA's `file`
plugin and the captured stream was analysed:

```text
device rate      44100 Hz  (rodio resamples our 48 kHz source)
non-zero frames  44100     = exactly 1.000 s of a 1 s source
dominant tone    440.0 Hz  (the test tone, exactly)
peak             0.24999   (source amplitude 0.25, unclipped)
window [0.5,0.75) ends at 0.750 s  — no overshoot, no run-on
muted            all-zero frames, playback still advances
```

That is `cargo test -p wavemark-ui` with `WAVEMARK_AUDIO_TEST=1`; without the
variable the device test skips, so CI (no sound card) stays green.

### Design notes worth keeping

- **`DecodedAudio::samples` is now `Arc<Vec<f32>>`.** The GUI hands the audio
  thread the decoder's own buffer instead of copying it — a two-hour recording
  is a couple of gigabytes of f32.
- **One `Player` per armed window, not one per app.** `Player::clear()` sleeps
  until the old sound drains, which on the UI thread could block for minutes;
  dropping a `Player` is non-blocking.
- **The source is bounded, not timer-truncated.** `PcmSource` covers
  `[start, end)` and simply runs out, so "play the selection" ends *at* the
  selection.
- **The device is opened lazily**, on the first play/stop/seek, so a machine
  with no sound card still gets a usable editor and a one-line status message.

---

## P0 — the product's core promise is not reachable from the UI

The whole pitch is *"select a range, annotate it, and hand exact time ranges to
an agent."* Three of the UI's sharpest edges are on exactly that path.

| # | Gap | Where |
|---|---|---|
| **P1** | **No time ruler on either waveform.** You cannot tell what second you are looking at. The only time readout is `sel: x – y s` in the header. For a tool whose output is timestamps, this is the single biggest UI omission. | `app.rs:364` `waveform()`, `app.rs:464` `overview()` |
| **P2** | **No way to set an exact range.** Selection is drag-only — no numeric entry, no snap, no editable start/end fields. "确切的时间点" currently means "wherever your mouse happened to stop." | `app.rs:379-401` |
| **P3** | **Selection has no drag handles.** Miss by 30 ms and you re-drag from scratch. | `state.rs:231-248` |
| ~~**P4**~~ | ~~**Playback is silent**~~ — **fixed in rc.3**, see above. | `audio.rs` |
| ~~**P5**~~ | ~~**Playhead drift.**~~ — **fixed in rc.3**: the device is the clock now. | `state.rs` `poll_playback()` |
| ~~**P6**~~ | ~~**No scrub.**~~ — **fixed in rc.3**: drag on the ruler. | `app.rs` `ruler()` |

## P1 — navigation is missing its most-used verbs

| # | Gap | Where |
|---|---|---|
| **N1** | **No scroll wheel at all.** No wheel handler exists anywhere in the crate, so no scroll-to-pan and no ctrl+wheel-to-zoom. On Linux desktop these are muscle memory. | grep `wheel` → 0 hits |
| **N2** | **Panning barely exists.** The only ways to move the viewport are clicking the overview bar (`focus`) or `j`/`k` between annotations. No drag-pan, no keyboard pan. | `state.rs:222` |
| **N3** | **Zoom is hard-anchored at 0.5.** Both keys and both buttons pass a literal `0.5`, so zoom always pivots on the centre of the view — never on the playhead, the selection, or the cursor. To zoom into something you must first navigate it to the centre. | `app.rs:196-197`, `app.rs:327`, `app.rs:333` |
| **N4** | **Overview viewport window can't be dragged**, only jumped by clicking. | `app.rs:493-502` |

## L — Linux desktop integration (almost entirely absent)

This is the weakest area. Right now wavemark is a *binary*, not a *desktop
application*.

| # | Gap | Evidence |
|---|---|---|
| **L1** | **No `.desktop` file.** Won't appear in GNOME/KDE/COSMIC launchers. | `find -name '*.desktop'` → none |
| **L2** | **No icon.** `assets/` exists and is *empty* — clearly intended, never filled. | `ls assets/` |
| **L3** | **No MIME registration.** Can't "Open with wavemark" from Nautilus/Dolphin. | none |
| **L4** | **Tarball is the only distribution.** No AppImage, no `.deb`, no Flatpak. AppImage is the de-facto "just works" answer for Linux desktop apps, and this app's whole install story is currently `tar -xzf` into `$PATH`. | release assets |
| **L5** | **No Wayland support.** `gpui-pre-0.3.5` initialises an X11 client; with no `DISPLAY` it panics inside `unwrap()`. It'll live on XWayland, but a hard panic instead of an error message is the wrong failure mode. | verified: `Failed to initialize X11 client` |
| **L6** | **GUI ignores argv entirely.** `main()` never reads `env::args`. Verified: `wavemark-ui demo.wav` silently opens an empty window. This kills both CLI-friendliness *and* MIME association (which passes the file as argv). | `main.rs:16` |
| **L7** | **No headless/screenshot path.** Can't be smoke-tested or visually regression-tested in CI. This is why this review has no screenshots. | — |
| **L8** | **No window title**, no default size, no minimum size. | `main.rs:22` `WindowOptions::default()` |
| **L9** | **No XDG state dir** — no recent-files, no persisted window geometry. | — |
| **L10** | **HiDPI / fractional scaling unverified.** Nothing scales explicitly. | — |

## U — UI & visual polish

**Theme**

- **U1: 7 hardcoded hex colours** (`0x0b0f14` canvas, `0x38bdf8` bars, `0x22d3ee` selection, `0xef4444` playhead, `0xf59e0b` markers, `0x64748b` overview, `0x0f151c` overview bg). They're a Tailwind palette that ignores `cx.theme()`. On a light GNOME theme the chrome goes light while both waveform panels stay near-black. Derive them from the theme.

**Information architecture**

- **U2: header is a wall of 11 equal-weight buttons** — Open, Play, Pause, Stop, Undo, Redo, Zoom+, Zoom−, Mark silence, Fit. No grouping, no icons, transport mixed with view controls. Overflows well before 1024 px.
- **U3: no menu bar.** Linux desktop apps are expected to have File/Edit/View.
- **U4: status text is crammed into the header** as `format!("{name} · {status}")`, so "annotation saved — now visible to `wavemark annotations`" competes with the filename and loses. Needs a bottom status bar.
- **U5: no duration / sample-rate / channel readout.** `info` has it; the GUI doesn't.
- **U6: no empty state.** A bare dark box where "drag an audio file here" should be.

**Annotations**

- **U7: the list can't scroll** — `overflow_hidden()` on the container. 7 annotations already fill it; 50 are unreachable.
- **U8: tags are invisible and unaddable.** The model carries them (`auto:sound`, `editor:draft`) and the CLI writes them, but the GUI shows neither — the panel renders only range + text.
- **U9: empty-text annotations render as blank rows.** The 4 `auto:silence`/`auto:sound` entries are unlabelled gaps. Use the tag as the label when text is empty.
- **U10: no edit-in-place.** Typo means delete and retype.
- **U11: no current-row highlight**, so you can't tell which annotation `j`/`k` landed on.
- **U12: markers are 2 px ticks pinned to the top edge** and overlap into a smear when annotations cluster; they're also not clickable, though the panel has "Go to".
- **U13: `Mark silence` uses hardcoded defaults** with no threshold/length UI, and there's no `Mark sound` counterpart even though the CLI has `--sound`.

**Copy & platform**

- **U14: the hint line says `⌘Z`** — that's the Mac glyph. On Linux it must read `Ctrl+Z`.
- **U15: export writes to CWD** with an auto-composed name and no save dialog (`app.rs:95-102`). The user won't know where the file went.

**Performance**

- **U16: 600 `flex_1()` divs rebuilt every frame.** At 30 fps while playing that's ~18k element builds/sec through GPUI's layout engine, and below 600 px wide each bar is sub-pixel. The "GPU-composited" goal would be better served by one canvas paint per frame.

## Non-issues I checked and cleared

- Backward drag works — `TimeRange::new` normalises start/end (`model.rs:23`).
- `export_range(..., 5.0)` is 5 **milliseconds** of fade, not 5 seconds.
- `frac_of` using `window.viewport_size()` is correct *today* only because the root has no horizontal padding. It's fragile, not broken.

## Suggested order

Updated after rc.3 — P1, P4, P5 and P6 are done.

1. **P2/P3 precise selection** — numeric entry and drag handles. These *are* the product.
2. **N1 wheel pan/zoom**, then **N2 keyboard pan**.
3. **L1–L4 desktop integration** — cheap, and it's what makes it a Linux app rather than a binary.
4. **L6 argv** — `wavemark-ui file.wav` should open the file; it also unblocks MIME "open with".
5. **U7/U8/U9/U12 annotation panel**, then **U1/U2/U3 theme and chrome**.
