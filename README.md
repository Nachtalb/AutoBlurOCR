# Redakt

Find text in a video and black it out. Runs entirely on your machine — no cloud, no network, no GPU.

Point it at a video, let it read the text, tick what should not be visible, check the result, and
render a redacted copy. Every export writes a log recording exactly what was done, and a one-click
check re-reads the finished file to confirm the text is really gone.

![Windows 11](https://img.shields.io/badge/Windows-11-blue) ![offline](https://img.shields.io/badge/network-none-green)

## Install

Grab the latest `Redakt_*_x64-setup.exe` from [Releases](../../releases) and run it. It installs
for the current user, so there is no admin prompt, and ffmpeg is bundled — nothing else to fetch.

Verify the download against the `.sha256` published beside it.

**Coming from AutoBlur?** Uninstall it once, then install Redakt. The rename changes the install
path and the uninstall entry, so the two would otherwise sit side by side. Your saved projects are
plain JSON and carry over untouched; the recent-files list and the interface settings start fresh.

Updating is just running the newer installer — it replaces the existing version in place and
keeps your settings. Nothing is uninstalled, and there is no update check phoning home: this tool
never touches the network.

The interface is in English and German, follows your Windows light/dark setting, and starts in a
**simple** mode that leaves the settings most jobs never touch at their defaults. Switch to
**advanced** in the top right for the sampling rate, dropout bridging, regex filtering, per-range
visibility and the pre-export review panels.

**One prerequisite:** a Windows OCR language pack. *Settings → Time & language → Language →*
(your language) *→ Language options → Optional features → Optical character recognition.* Redakt
lists the packs you actually have installed, so a missing one is obvious before you start.

## Using it

1. **Open a video.** Geometry, duration and a SHA-256 of the file are recorded up front.
2. **Read it.** Redakt samples the video and runs every frame through Windows' OCR. The rate
   defaults to the video's own frame rate, so no frame goes unchecked.
3. **Pick what to hide.** Every distinct piece of text found is listed. Near-identical readings of
   the same thing — `Hauptstrasse 14`, `Hauptstrasse 1A` — are grouped into one row, so a single
   tick catches all its misreadings. Tick as many as you like, or filter with a regular expression
   and watch the list narrow as you type.
4. **Hide it.** Boxes are generated for every appearance and follow the text as it moves. They
   are filled bars — black by default, white or red if that reads better against the footage.
5. **Bleep the sound, if it needs it.** Drag on the bleep track under the video to mark a
   stretch; the audio there is replaced by a 1 kHz tone. A tone rather than silence, so anyone
   watching can tell an edit was made rather than wondering whether the recording simply went
   quiet. Everything you do not mark is copied through exactly as it was.
6. **Check it** (advanced mode). Three tabs, and this step matters:
   - **every hit** — step through each place the text was found and confirm the box sits over it.
     Jump to the frame either side of a redaction with the boxes switched off to see whether
     anything is readable there.
   - **guessed stretches** — where OCR lost the text for a moment and the box was held across the
     gap. Worth watching. Any dropout too long to bridge is listed here first, in red: the box is
     off for those frames and the text is visible. Raise **bridge dropouts up to** until they
     stop appearing — it is an advanced setting, and it starts at 0. This one is flagged
     everywhere, including in simple mode.
   - **almost matched** — text that *nearly* matched what you picked and was left visible. This is
     where a leak hides. One row per distinct reading, however many frames it spans. Read it.
7. **Render.** You get the redacted video, a log beside it, and buttons to open the folder, play
   the file, or re-read it and confirm the text is gone. The folder stays one click away in the
   project panel afterwards. **ffmpeg output** shows what the encoder
   is doing while it works, including its speed, and **stop** ends a render you don't want to wait
   for.

Every generated box stays editable, and you can always drag a box on the video by hand.

**Strip metadata and extra tracks** is on by default, under the advanced settings. It drops the
source file's metadata — title, comments, device, location — its chapters, and every subtitle and
data stream. A subtitle track can spell out the very text the bars cover, which would make the
redaction pointless; leave this on unless you have a reason not to.

## What it writes

Alongside every export, `<output>.redaction-log.json` records the input path and hash, the output
hash, the tool and ffmpeg versions, the OCR engine, language and sampling rate, the exact
filtergraph and command line, every redacted string, and per-box counts. Pressing **check the text
is gone** — on the dialog that appears when a render finishes — folds its verdict into the same
file.

The output is a re-encoded derivative, not a bit-exact copy, and the log says so. The bars are
burned in: ffmpeg decodes each frame, fills the box with solid colour, and re-encodes from that.
There is no overlay to peel off and nothing underneath to recover — which is exactly why blur and
pixelate were removed, since both are reversible transforms of the pixels rather than a discard.

## Handling

- The input is opened read-only and never modified. Export refuses to overwrite it.
- A saved project stores the video's hash. Opening it against a different file is refused —
  boxes are coordinates and timestamps, and applying them to the wrong video would be a silent,
  serious error.
- Generated redactions are proposals. In advanced mode nothing exports without you confirming you
  have checked them.
- Bars are filled, not blurred or pixelated. Both of those are partially recoverable, and both cost
  a separate crop and overlay per box on every frame — on a clip with many boxes that is the
  difference between a render that finishes and one that does not.

## Building it yourself

See [CLAUDE.md](CLAUDE.md) for the architecture, the test suite, and the constraints worth knowing
before changing anything.

```powershell
# ffmpeg + ffprobe as bundled sidecars — see src-tauri/binaries/README.md
cd src-tauri
cargo run                 # run it; no tauri-cli needed, the frontend is static
cargo tauri build         # installer
```

## Licence

MIT — see [LICENSE](LICENSE).
