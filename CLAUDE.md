# Working on AutoBlur

Implements `SPEC.md`. The original browser editor is preserved in `legacy/` — its box model,
interpolation, span logic and filtergraph emitter were carried over, and only its browser-bound
parts (file input, blob URLs, downloads, IndexedDB) were replaced.

```
src/index.html            frontend — one file, plain JS, no framework, no build step
src-tauri/src/lib.rs      core: probe, frame sampling, export. No Tauri types in any signature.
src-tauri/src/ocr.rs      Windows.Media.Ocr
src-tauri/src/main.rs     Tauri commands — a thin wrapper over the core
src-tauri/tests/          acceptance tests against ffmpeg-generated fixtures
tools/pure.mjs            loads the frontend's DOM-free block into node
tools/jstest.mjs          runs the frontend's self-test headlessly
tools/gen-boxes.mjs       ocr.json + a string -> boxes.txt, using the app's own code
```

The UI is English and German (`L` in the app section; static text carries `data-i18n`,
`data-i18n-title` or `data-i18n-ph` and is swapped by `applyLang()`, anything built in JS goes
through `tx()`). It has a simple and an advanced mode — simple sets `:root.simple`, which hides
every `.adv` element and pins the OCR rate to the video's. Theme is
`:root.light` over CSS variables, defaulting to the OS setting. All three persist in
`localStorage`.

`src/index.html` is split by `PURE START` / `PURE END` markers. Everything between them is
DOM-free and is executed headlessly by `tools/*.mjs`. Keep it that way: it is the only reason the
matching, tracking and filtergraph code can be tested without a browser.

## Build and test

```powershell
node tools\jstest.mjs                                              # frontend self-test
cd src-tauri
cargo run                                                          # run the app
cargo test --no-default-features                                   # core, no webview stack
cargo test --no-default-features -- --include-ignored              # + OCR and export round trips
cargo tauri build                                                  # installer
```

`--no-default-features` drops the `app` feature so the core builds without the webview stack.
Sidecars resolve from `AUTOBLUR_FFMPEG` / `AUTOBLUR_FFPROBE`, then next to the executable, then
`src-tauri/binaries/<name>-<triple>.exe` — never from `PATH`, because a forensic workstation's
`PATH` is not ours to assume and version drift changes filter behaviour.

Fixtures are generated at test time with ffmpeg `drawtext` at known positions and times, so ground
truth is exact. Coverage is split by where the code lives: coordinate and timestamp fidelity,
two-appearances-in-one-frame and the export round trip are in `tests/acceptance.rs`; span padding,
gap bridging, spatial clustering, keyframe stepping, chunking and the hash guard are in the
frontend's `selfTest()`, because that is where the occurrence→track conversion runs.

The round trip is the one that matters. It exports a redacted fixture and re-OCRs the output. If
it regresses, the tool leaks text.

## Versioning

`Cargo.toml` and `tauri.conf.json` must agree, and the release workflow refuses to publish if the
tag disagrees with either. That version is written into every `redaction-log.json`, so two builds
sharing a version number makes an exhibit untraceable. Bump it for anything you ship.

## Constraints found by running it, not by reading about it

Each of these is commented at its call site. They cost real debugging time and are easy to
"simplify" back into a bug.

**ffmpeg's expression evaluator has a budget of about 100 operations.** A flat sum spends it as
fast as nesting does — measured on ffmpeg 8, 98 summed terms parse and 99 fail with
`Failed to configure input pad`. No expression shape escapes it, so a track with more keyframes
than fit is emitted as several boxes, each active over its own slice of time and sharing boundary
keyframes so there is no seam.

**Every box is a filled `drawbox`; there is no blur and no pixelate.** They needed a split, a
crop, two scales and an overlay per box, each running on every frame whether that box was enabled
or not. With long tracks split into chunks it reached 0.0 fps — not slow, stopped. `drawbox` writes
in place, chains with a plain comma and needs no stream labels, so any number of boxes stays one
cheap pass. The box model has no `mode` or `strength` field any more; a project saved by an older
build has them stripped on load.

**Near misses are grouped by distinct reading before they are shown or counted.** The same
misreading on 200 consecutive frames is one thing to judge. Ungrouped, a handful of near-identical
chat names produced 846 rows, which is not a review surface, it is a wall — and the same number in
the pre-export dialog reads as a catastrophe rather than a list.

**Generated boxes step between readings; they do not interpolate.** OCR measured a rect on a
specific frame, and sampling at the video's own rate means every frame has one. Sliding between
them replaces a measurement with a guess. Hand-drawn boxes keep sliding, because there the
keyframes are waypoints a human set.

**Track association is per-axis and predicted from measured speed.** A single radius of
`max(w, h)` uses a text line's *width* as its *vertical* tolerance, so a 150 px wide name adopts
another copy of itself 150 px above — in a scrolling list, a different message. That merge paints
one box spanning everything between them.

**Gap bridging is measured in seconds, not samples.** Counting samples ties the bridged window to
the sampling rate: three samples is 1.5 s at 2 fps but 0.1 s at 30 fps. Sampling faster would then
split spans that used to hold and switch the box off mid-dropout — exactly backwards.

**Nothing may outlive its owner.** `std::process::Child` does not kill on drop, and every `?` in
the OCR loop drops the `FrameReader` that owns one — a frame read error, a failed recognition, a
panic. The orphaned ffmpeg then blocks forever writing into a pipe nobody drains and keeps
`ffmpeg.exe` open until the machine is rebooted, at which point the next installer stops with
"error opening file for writing" and the only ways out are abort or ignore. `Reaped` kills and
waits in `Drop`, so no early return has to remember; `a_dropped_frame_reader_leaves_no_ffmpeg_behind`
fails without it.

**A dropout the bridge refuses to cross is a hole, and it has to be reported.** The span closes,
the box goes off, and the text is on screen for those frames. Span padding covers one sampling
interval either side, so a dropout is covered when it is no longer than two of them; anything
longer than that and longer than the bridge is exposed. Measured over 200 randomised scrolls,
2.5% of frames leak at a bridge of 0, 0.5% at 0.1 and 0.06% at 0.2 — and none of it was reported
anywhere until `exposedGaps` existed. It reports every refused gap with no upper bound on length
and no test of whether the text moved in between: over-reporting a warning costs a line in a
list, under-reporting it costs the disclosure. The warning appears next to the generate button
(the only place simple mode can show it), in the review panel, and it forces the pre-export
dialog even in simple mode. The bridge itself defaults to 0 and is an advanced setting, so the
simple-mode wording says where to find it rather than naming a control that is not on screen.

`exposedGaps` accounts for the leaks where the box is off. A residual ~12% of leaking frames in
that harness are a different failure: the box is on but its held rect is stale by one sample of
motion, at the first or last frame of an appearance. That is the documented ±1 sampling interval
limit, and it is not what this reports.

**A straight line across a dropout is not coverage.** Between two readings either side of a gap the
box would travel a path the text never took. It holds a rect covering both ends instead, or walks
across in steps when one rect would balloon. Over-covering costs pixels; under-covering is a
disclosure.

**`CoIncrementMTAUsage` pins the MTA for the process lifetime.** Without it, the last thread that
called `RoInitialize` exiting tears WinRT down and frees the activation factories that windows-rs
caches in process statics. Those caches are never invalidated, so the next `Buffer::Create` on any
thread dereferences a freed vtable. It only shows up under concurrency, far from the cause.

**One `OcrEngine` per worker.** A second `RecognizeAsync` on a busy engine fails outright with
"Another RecognizeAsync operation is already running!", so engines cannot be shared across the
pool even though the type is agile.

**The graph goes to ffmpeg from a file, never inline** — it contains escaped commas that Windows
argument quoting mangles. Which flag depends on the build: `-/filter:v` on ffmpeg 7+, and
`-filter_script:v` on older ones, removed in ffmpeg 8. The first export tries the new spelling,
falls back, and remembers.

**ffmpeg echoes the whole filtergraph when it rejects one**, as a single line thousands of
characters long, which shoves the real diagnostic out of any tail you keep. `readable_error` clips
long lines so the message survives.

**`window.alert` is swallowed and `window.confirm` returns true without asking** in this webview.
Messages and the pre-export confirmation are in-page. A native dialog there would have been a
review gate that silently always said yes.

## The vendored NSIS installer template

`src-tauri/windows/installer.nsi` is Tauri's own template with two changes, each marked
`AutoBlur patch`.

The second frees a sidecar that some stray process still holds. The template checks for and
closes the app, but ffmpeg and ffprobe are separate processes and nothing looks for them, so one
survivor stopped the install dead. Killing them by name would take out the user's own ffmpeg;
instead each sidecar is renamed aside — Windows refuses to delete a running image but will rename
one, the open handle following the file — the new copy is written over the freed name, and the
`.old` leftover is swept up by the next install or the uninstaller.

The first: Upstream shows a page whenever a previous install is detected and preselects
"uninstall before installing", so the ordinary way to update is uninstall-then-reinstall. An
upgrade writes new files over old ones and needs no removal, so the patch skips that page when
the incoming version is newer. Same-version runs keep their Add/Reinstall vs Uninstall choice,
and downgrades keep the page and the `ALLOWDOWNGRADES` guard.

This is a fork, so it can rot. `UPSTREAM.txt` pins the bundler version and the hash of the file
it came from, and the release workflow fails if the installed `tauri-bundler` is a different
version or its template changed. To re-sync: copy the upstream file over ours, re-apply the
marked block, update `UPSTREAM.txt`.

The alternative — `tauri-plugin-updater` — was rejected deliberately. It would have the app call
GitHub on launch, and "no network access, ever" is the guarantee this tool makes.

## Things deliberately not built

- No OCR engine abstraction. One engine, one code path, until a real video defeats it — then add
  RapidOCR/PaddleOCR ONNX behind a setting.
- No second source of truth in Rust. All state lives in the frontend's box model; the backend is
  stateless apart from the cancel flag and the OCR cache.

## Known limits

- Text clipped by the frame edge stops matching once part of it is cut off. It surfaces in
  **almost matched** rather than being redacted silently.
- Text moving faster than roughly its own width between readings splits into separate boxes.
  Sampling at the video's frame rate — the default — makes this rare.
