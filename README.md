# AutoBlur

Offline, OCR-driven text redaction for video. Windows 11, no cloud, no GPU, no network.

Implements `SPEC-ocr-redact.md`. The existing HTML editor is the frontend: its box model,
interpolation, span logic and filtergraph emitter (`expr` / `at` / `graph`) are ported verbatim
along with their self-test. Only the browser-bound parts were replaced.

```
src/index.html            frontend — one file, plain JS, no framework, no build step
src-tauri/src/lib.rs      core: probe, frame sampling, export. No Tauri types in any signature.
src-tauri/src/ocr.rs      Windows.Media.Ocr
src-tauri/src/main.rs     Tauri commands — a thin wrapper over the core
src-tauri/tests/          §11 acceptance tests against an ffmpeg-generated fixture
tools/jstest.mjs          runs the frontend's self-test headlessly
tools/gen-boxes.mjs       ocr.json + a string -> boxes.txt, using the app's own code
```

## Build

```powershell
# 1. sidecars — see src-tauri/binaries/README.md
$b = (Get-Command ffmpeg).Source | Split-Path
copy "$b\ffmpeg.exe"  src-tauri\binaries\ffmpeg-x86_64-pc-windows-msvc.exe
copy "$b\ffprobe.exe" src-tauri\binaries\ffprobe-x86_64-pc-windows-msvc.exe

cd src-tauri
cargo run                                  # run it — no tauri-cli needed, the frontend is static

cargo install tauri-cli --version "^2"     # only for the installer
cargo tauri build                          # NSIS installer
```

`cargo run` resolves the sidecars from `src-tauri/binaries/`; a bundled build finds them next to
the executable. The winget ffmpeg "full" builds are ~200 MB each — use a smaller static build if
installer size matters.

An OCR language pack must be installed:
*Settings > Time & language > Language > (language) > Language options > Optional features >
Optical character recognition*. The dropdown lists what is actually installed, so a missing
pack is visible before you start rather than at runtime.

## Test

```powershell
node tools\jstest.mjs                                            # frontend self-test
cd src-tauri
cargo test --no-default-features                                 # core, no webview stack
cargo test --no-default-features -- --ignored --nocapture        # + the export round trip
```

Acceptance coverage (§11): coordinate fidelity, timestamp fidelity, two-positions-one-frame and
the round trip live in `src-tauri/tests/acceptance.rs`; span padding, gap bridging, spatial
clustering and the hash guard live in the frontend's `selfTest()` because that is where the
occurrence→track conversion runs. Tests 3 and 6 are the ones that matter — if either regresses,
the tool leaks text.

## Workflow

1. **open video** — probed for geometry, duration and sha256; loaded through the asset protocol.
2. **run** OCR — ffmpeg pipes BGRA frames at the sampling rate, `rayon` recognises them across
   `num_cpus - 1` workers, results reordered by timestamp. Frame *i* is source time *i / rate*.
3. **tick what to hide.** Near-identical readings of the same thing ("Hauptstrasse 14",
   "Hauptstrasse 1A") are clustered by edit distance into one collapsible row, so one tick
   catches every spelling. Tick several and redact them in one go. A green dot marks strings
   that already have boxes. The regex field unlocks after reading and filters the list live, so
   you can see exactly what it will catch before committing.
4. **hide it** — occurrences cluster into tracks by IoU, dropouts shorter than the bridge window
   are held across, spans are padded by one sampling interval on each side, and keyframes are
   simplified (see below).
5. **check** — three tabs: every hit, the stretches OCR lost and the box guessed through, and
   the "almost matched" text that was left visible. Use the adjacent-frame buttons to look just
   outside a redaction with the boxes hidden. Every generated box stays fully editable;
   hand-placed boxes still work exactly as before.
6. **export** — the graph goes to ffmpeg from a file, never inline (it contains escaped commas
   that Windows argument quoting would mangle). Which flag does that depends on the bundled
   build: `-/filter:v` on ffmpeg 7+, `-filter_script:v` on older ones — it was removed in
   ffmpeg 8. The first export tries the new spelling and falls back, then remembers.
   A `<output>.redaction-log.json` sidecar records input/output hashes, tool and
   ffmpeg versions, engine, language, rate, the exact graph and command, the redacted strings and
   per-box span/keyframe counts.
7. **verify export** — re-OCRs the output and asserts zero matches for every redacted string.
   The result is folded into the log. Cheap and decisive.

## Notes

- Input is opened read-only and never written to. Export refuses to overwrite it.
- A project stores `video_sha256`. Loading it against a different video is refused, with the
  reason: boxes are coordinates and timestamps, and applying them to the wrong file is a silent,
  serious error.
- Cached OCR is keyed on video hash + rate + language, and discarded if any of the three differ.
- Default mode for generated boxes is `pixelate`: blur over text is partially recoverable.
- Frames larger than `OcrEngine::MaxImageDimension` are downscaled for recognition only, and the
  rects are mapped back to source pixels. `ocr_scale` in `ocr.json` records it; anything below
  1.0 is shown in the UI.
- No engine abstraction. One engine, one code path, until a real video fails — then add
  RapidOCR/PaddleOCR ONNX behind a setting.
- The OCR rate defaults to the video's own frame rate: anything slower leaves frames nobody
  looked at, and the interpolation has to guess across them.

Three things the spec got wrong, found by running it on moving text at 30 fps:

- **The filtergraph must be a flat sum, not nested `if()`.** ffmpeg's expression parser
  (`libavutil/eval.c`) recurses once per nesting level and gives up at about 100. One keyframe
  per video frame produced a 272-deep chain and the export died with
  `Failed to configure input pad on Parsed_crop_1`. Addition is parsed in a loop, so a flat sum
  of guarded segments costs one level and is exact for any length.
- **Gap bridging is in seconds, not samples.** Counting samples ties the bridged window to the
  sampling rate: three samples is 1.5 s at 2 fps but 0.1 s at 30 fps. Raising the rate to match
  the video therefore *shrank* the window, split spans that used to hold, and switched the box
  off mid-dropout — the opposite of what sampling faster should do.
- **A straight lerp across a dropout is not coverage.** Between two detections either side of a
  gap the box travels in a straight line the text never took. Across any gap the box now holds
  a rect covering both ends. Over-covering costs pixels; under-covering is a disclosure.

Keyframes are thinned with Douglas–Peucker at a 1 px tolerance (well under the box margin, so
it can never expose text): a pan that produced 287 keyframes emits 8, and a 21 KB filtergraph
becomes 1.1 KB.

`window.alert` is swallowed and `window.confirm` returns true without asking in this webview, so
messages and the pre-export confirmation are in-page, not native dialogs. A native dialog there
would have been a review gate that silently always said yes.

Two WinRT constraints cost real debugging time and are commented at their call sites, so nobody
has to rediscover them:

- **The MTA must be pinned for the process lifetime** (`CoIncrementMTAUsage`). If the last thread
  that called `RoInitialize` exits, WinRT tears down and frees the activation factories that
  windows-rs caches in process statics — those caches are never invalidated, so the next
  `Buffer::Create` on any thread dereferences a freed vtable and the process dies with an access
  violation. It presents as a crash only under concurrency, which sends you hunting in the wrong
  place.
- **One `OcrEngine` per worker.** A second `RecognizeAsync` on a busy engine fails with
  "Another RecognizeAsync operation is already running!", so engines cannot be shared across the
  pool even though the type is agile.
