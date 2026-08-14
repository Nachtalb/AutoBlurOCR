# SPEC: OCR-driven text redaction for video

Target: Windows 11, offline, no cloud, no GPU required. Evidence-grade output.

## 0. Context

A working HTML editor already exists (`video-box-redact.html`): box drawing, keyframed
movement, visible spans, per-box mode (black bar / blur / pixelate), and ffmpeg filtergraph
emission. Its project format is `redact.json`.

It becomes this app's frontend. Carry over the box model, the interpolation, the span logic and
the filtergraph emitter verbatim — they work and they are tested. Replace only its browser-bound
parts: `<input type=file>` → Tauri dialog, blob URL → asset protocol, download → backend write,
IndexedDB recents → a JSON file next to the executable.

Refactor its layout and styling freely. Do not reimplement `expr()`, `at()` or `graph()` — port
them, and port their self-test with them.

## 1. Scope

### In
- Sample frames from a video, OCR them, index every text occurrence with a bounding box and timestamp.
- Present the deduplicated string list to the user.
- User selects one or more strings → tool generates tracked, time-bounded redaction boxes for every occurrence.
- User reviews and corrects the result before export.
- Export via the existing ffmpeg emitter.

### Out
- Face / plate / person detection.
- Audio redaction.
- Cloud OCR, any network call whatsoever.
- Handwriting.

## 2. Stack

One Tauri v2 app. Rust backend, the existing HTML file as the webview frontend.

- **Frontend: no framework, no bundler, no build step.** One HTML file, plain JS, as today.
  It already does box drawing, keyframes, spans, modes and filtergraph emission. Adding a
  framework buys nothing here and costs a toolchain.
- **Backend: Rust.** Owns OCR, frame decoding, hashing, ffmpeg invocation, file I/O.
- **ffmpeg + ffprobe: bundled Tauri sidecars** (`ffmpeg-x86_64-pc-windows-msvc.exe`). Do not
  resolve from `PATH` — a forensic workstation's `PATH` is not yours to assume, and version
  drift changes filter behaviour. Record the bundled version in the export log.
- **OCR core is a plain Rust module with no Tauri types in its signatures**, so `cargo test`
  can drive it headlessly. The Tauri commands are a thin wrapper over it.

Details an implementation will get wrong if not stated:

- The webview cannot play a video from a raw Windows path. Load it via Tauri's asset protocol
  (`convertFileSrc`), and enable `assetProtocol` scope for the chosen file's directory.
- OCR is long-running. Run it on a background task, emit a Tauri event (`ocr://progress`,
  `{done, total}`) per completed frame, and support cancellation via a shared `AtomicBool`.
- Export progress: parse `time=` out of ffmpeg's stderr and emit `export://progress`.
- All state lives in the frontend's existing box model. The backend is stateless between calls
  apart from the OCR result cache. Do not build a second source of truth in Rust.

## 3. OCR engine

Use **Windows.Media.Ocr** (`Windows.Media.Ocr.OcrEngine`) via the `windows` crate.

Reasons: ships with the OS, no model files to bundle or license, no download, returns
word-level bounding boxes, and is genuinely good on rendered screen text — which is what most
video text redaction is. `OcrEngine::TryCreateFromLanguage` for `de-DE` and `en-US`;
fall back to `TryCreateFromUserProfileLanguages`.

Language is a UI setting (BCP-47). List available engines with `OcrEngine::AvailableRecognizerLanguages`
and populate the dropdown from that, so the user sees what is actually installed rather than
hitting a runtime failure. If the wanted language is missing, say which one and point at
`Settings > Time & language > Language`.

If Windows OCR proves insufficient on real footage (low-res dashcam, camera-of-a-screen), add
RapidOCR/PaddleOCR ONNX behind an engine setting. **Do not build that abstraction up front.**
One engine, one code path, until a real video fails.

## 4. Frame sampling

Do not decode every frame. Pipe raw frames from ffmpeg:

```
ffmpeg -hide_banner -i <input> -vf fps=<RATE> -pix_fmt bgra -f rawvideo -
```

- `--rate` default **2 fps**. Configurable.
- Frame `i` from this stream corresponds to source time `t = i / RATE`. Record it exactly.
- Read `width`/`height` from `ffprobe -show_streams` first; the rawvideo stream carries no header.
- OCR at **full resolution**. Downscaling is the single fastest way to lose small text.
- Parallelise OCR across frames with `rayon`, bounded to `num_cpus - 1` workers. Frames must be
  reordered by timestamp before writing output.

Budget check: 10 min @ 2 fps = 1200 frames. Windows OCR is roughly 40–120 ms/frame at 1080p, so
1–2 minutes wall clock with 8 workers. Acceptable. Print progress to stderr as `frame/total`.

## 5. Backend API

Rust core (testable without Tauri):

```rust
pub fn probe(path: &Path) -> Result<VideoInfo>;                       // w, h, duration, sha256
pub fn ocr_video(path: &Path, opts: &OcrOpts, progress: &dyn Fn(usize, usize),
                 cancel: &AtomicBool) -> Result<OcrResult>;
pub fn export(path: &Path, filtergraph: &str, out: &Path,
              progress: &dyn Fn(f64)) -> Result<ExportReport>;
```

Tauri commands, one per core function, plus `pick_video`, `save_project`, `load_project`.

`OcrResult` is held in the frontend as a JS object. Persist it alongside the project so a
reopened case does not re-OCR — key the cache on `video_sha256` + `rate` + `lang`, and discard
it if any of the three differ.

Also write it out as `ocr.json` on request. Not as an interchange format — as a debug artifact
and as the fixture format for the acceptance tests in §11.

### `OcrResult` schema

```jsonc
{
  "video": "case123.mp4",
  "video_sha256": "…",          // integrity anchor, see §10
  "width": 1920, "height": 1080,
  "duration": 612.4,
  "rate": 2.0,                   // sampling rate, needed to compute span padding
  "engine": "Windows.Media.Ocr", "lang": "de-DE",
  "generated": "2026-08-13T10:04:00+02:00",
  "lines": [
    {
      "t": 12.5,                 // exact source timestamp of the sampled frame
      "text": "Hauptstrasse 14",
      "conf": 0.0,               // Windows OCR gives no confidence; emit 0.0, keep the field
      "rect": [420, 880, 210, 26],   // x, y, w, h in SOURCE VIDEO PIXELS
      "words": [
        { "text": "Hauptstrasse", "rect": [420, 880, 150, 26] },
        { "text": "14",           "rect": [578, 880,  52, 26] }
      ]
    }
  ]
}
```

Coordinate space is source video pixels, origin top-left. This is the same space the HTML
editor's box model uses. **Any mismatch here silently misplaces every redaction — assert it
in a test.**

Keep both line and word rects. Line-level matching is what users want ("redact this address");
word rects are needed when only part of a line is sensitive.

## 6. String index and matching

Build the candidate list the user picks from:

1. Normalise: trim, collapse internal whitespace, casefold (`to_lowercase`), strip surrounding punctuation.
2. Group occurrences by normalised text.
3. Sort by occurrence count descending, then by first appearance.
4. Show: the raw text as most commonly OCR'd, occurrence count, first/last timestamp.

When the user selects a string, match occurrences with, in order:

- **Exact** on the normalised form.
- **Substring** — an occurrence whose normalised line *contains* the selection. Default on;
  a selected plate number should match whether or not OCR captured the surrounding text.
- **Fuzzy** — Levenshtein distance ≤ `max(1, len/8)` on the normalised form, only for strings
  of length ≥ 6. OCR drifts between frames (`0`/`O`, `1`/`l`, `rn`/`m`); exact matching alone
  will leave frames unredacted. Fuzzy matches are flagged distinctly in the review UI.

Each mode is a checkbox. Fuzzy defaults **on** — a false positive is a wasted black box, a
false negative is a disclosure.

Also support a raw **regex** mode for structured targets (plates, IBANs, phone numbers, case
numbers). One text field, `regex` crate, applied to the line text.

## 7. Occurrence → track conversion

This is where the real work is. Input: matched occurrences `[(t, rect)]` for one selected
string. Output: boxes in the editor's model.

```
dt = 1 / rate                      // sampling interval
GAP_BRIDGE = 3                     // samples; configurable
PAD_PX     = 6                     // configurable
```

1. Sort occurrences by `t`.
2. **Cluster spatially.** The same string can appear twice in one frame (two screens, a
   reflection, a repeated caption). Within each frame, occurrences are separate. Across frames,
   assign an occurrence to an existing track if its rect's IoU with the track's last rect is
   ≥ 0.3, or their centres are within `max(w, h)` of each other. Otherwise start a new track.
   Greedy nearest-first assignment is sufficient; do not build a Kalman filter.
3. **Bridge gaps.** Within a track, if two consecutive occurrences are separated by
   ≤ `GAP_BRIDGE * dt`, keep them in one span — OCR drops frames on motion blur and compression
   artefacts, and a flickering redaction is worse than a slightly over-long one.
   A larger gap closes the span and opens a new one in the same track.
4. **Pad the spans.** Each span becomes `[first_t - dt, last_t + dt]`, clamped to `[0, duration]`.
   Frames between samples were never OCR'd; without this padding the text is visible on them.
   **This is a correctness requirement, not a nicety.**
5. **Emit keyframes.** One keyframe per occurrence, at its `t`, with the rect padded by `PAD_PX`
   on each side and clamped to the frame. The editor interpolates linearly between them, which
   handles scrolling and panning text correctly.
6. **Emit one box per track**, with all its spans and all its keyframes, `mode` from the user's
   choice, default `pixelate`.
7. For `blur` and `pixelate` modes the editor fixes the crop size at the largest keyframe rect
   (an ffmpeg `crop` constraint). If a track's rect area varies by more than 2×, warn — it should
   probably be split, or use `black`.

Merge overlapping spans within a track before emitting.

## 8. Review UI (mandatory, not optional)

Generated redactions must be verified before export. Provide:

- **Hit stepper** — jump to each occurrence in turn, box overlaid, with `exact` / `fuzzy` /
  `regex` labelling and the OCR'd text shown. Accept, adjust, or drop each one.
- **Gap report** — for each track, list bridged gaps and their length. A long bridged gap means
  OCR lost the text; the user should confirm the interpolated box still covers it.
- **Unmatched near-misses** — occurrences within Levenshtein distance ≤ 3 of a selected string
  that were *not* matched. This is the false-negative surface, and it is the thing that gets a
  redaction overturned. Show it prominently.
- **Adjacent-frame check** — for each span boundary, render the frame just outside the span with
  the box off, so the user can see whether text is visible there.

Every generated box remains fully editable in the existing editor. Nothing is locked.

## 9. Export

The frontend emits the filtergraph exactly as it does now. The backend writes it to a temp
`boxes.txt` and runs the bundled sidecar:

```
ffmpeg -i <in> -filter_script:v boxes.txt -c:v libx264 -crf 18 -preset veryfast -c:a copy <out>
```

Pass the graph via `-filter_script:v`, never inline. It contains escaped commas, and Windows
argument quoting will mangle them.

- Default mode for OCR-generated boxes is `pixelate`, block size ≥ 12. Blur over text is
  partially recoverable; a pixelated or filled region is not.
- **Verify button**: after export, re-run OCR on the *output* file and assert zero matches for
  every redacted string. Cheap and decisive. Show a pass/fail summary and write it into the log.
  Not optional for evidence work, but the user presses it, so it stays a button.

## 10. Evidence handling

- Never modify the input file. Open read-only.
- Record `video_sha256` of the input in `ocr.json` and in the project file. On load, if the
  hash of the currently loaded video differs, refuse to apply boxes and say why. Boxes are
  coordinates and timestamps; applying them to the wrong file is a silent, serious error.
- Write a sidecar `<output>.redaction-log.json` on export containing: input path and hash,
  output hash, tool version, OCR engine and language, sampling rate, the exact filtergraph,
  the full ffmpeg command, every redacted string, and per-box span/keyframe counts. This is the
  artefact that answers "what exactly did you do to this video".
- The output is a derivative exhibit, re-encoded. Say so in the log.

## 11. Acceptance tests

`cargo test` against the OCR core, plus the ported JS self-test for `expr`/`at`/`graph`. No
mocking framework. Generate the fixture at test time: a 20-second video built with `ffmpeg`
`drawtext` at known positions and times, so ground truth is exact.

1. **Coordinate fidelity** — a `drawtext` string placed at a known (x, y) is reported within ±8 px.
2. **Timestamp fidelity** — text present only from 4.0 s to 6.0 s produces occurrences only in
   that range, ±`dt`.
3. **Span padding** — the generated span starts at or before 4.0 s and ends at or after 6.0 s.
   Never inside.
4. **Gap bridging** — text hidden for 1 s mid-appearance, with `GAP_BRIDGE * dt` ≥ 1 s, yields
   one span, not two.
5. **Spatial clustering** — the same string rendered at two positions in the same frame yields
   two tracks.
6. **Round trip** — export the fixture, re-OCR the output, assert zero matches for the redacted
   string.
7. **Hash guard** — loading a project against a different video is refused.

Test 3 and test 6 are the ones that matter. If either regresses, the tool leaks text.

## 12. Non-negotiables

- No network access, ever. Assert it if practical.
- Input opened read-only.
- Generated redactions are proposals requiring human confirmation. Never auto-export without review.
- The near-miss / gap report is not a nice-to-have. It is the only visibility into what OCR missed.
- Keep the existing manual box workflow fully functional. OCR is an accelerator; hand-placed
  boxes remain the fallback when OCR fails, which it will on low-quality footage.
