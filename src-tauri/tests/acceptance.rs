//! §11. No mocking framework. The fixture is generated at test time with ffmpeg `drawtext`
//! at known positions and times, so ground truth is exact.
//!
//!   cargo test --no-default-features          # core only, no webview stack
//!   cargo test --no-default-features -- --nocapture --ignored   # + the OCR round trip
//!
//! Sidecars: put ffmpeg/ffprobe in src-tauri/binaries/, or set AUTOBLUR_FFMPEG / AUTOBLUR_FFPROBE.

use autoblur::*;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool as AB;

static NO_CANCEL: AB = AB::new(false);
#[cfg(windows)]
use std::sync::atomic::AtomicBool;

const W: u32 = 1280;
const H: u32 = 720;
const TEXT: &str = "HAUPTSTRASSE 14";
const TX: f64 = 420.0;
const TY: f64 = 300.0;
const T_ON: f64 = 4.0;
const T_OFF: f64 = 6.0;
const RATE: f64 = 2.0;

fn tmp() -> PathBuf {
    let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join("fixtures");
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Fixtures are built once and shared by every test, and the tests run in parallel. Without
/// this, a cold checkout races: two ffmpeg processes write the same file while a third reads a
/// half-written one and ffprobe reports "moov atom not found". Build under a lock, into a temp
/// name, then rename — so a reader either sees no file or a complete one.
fn build_once(out: PathBuf, build: impl FnOnce(&Path)) -> PathBuf {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if !out.is_file() {
        let part = out.with_extension("part");
        let _ = std::fs::remove_file(&part);
        build(&part);
        std::fs::rename(&part, &out).unwrap();
    }
    out
}

/// ffmpeg's filter parser splits options on `:`, so a Windows drive letter inside a value is
/// a fight not worth having: the font is copied next to the fixtures and ffmpeg runs from
/// there, leaving a bare relative filename in the graph.
fn font() -> &'static str {
    build_once(tmp().join("font.ttf"), |part| {
        let cands: &[&str] = if cfg!(windows) {
            &[r"C:\Windows\Fonts\arial.ttf", r"C:\Windows\Fonts\segoeui.ttf",
              r"C:\Windows\Fonts\consola.ttf", r"C:\Windows\Fonts\tahoma.ttf"]
        } else {
            &["/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
              "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
              "/usr/share/fonts/TTF/DejaVuSans.ttf"]
        };
        let f = cands.iter().find(|p| Path::new(p).is_file())
            .unwrap_or_else(|| panic!("no font found, tried {cands:?}"));
        std::fs::copy(f, part).unwrap();
    });
    "font.ttf"
}

/// 20 s, white, one black string at a known place, on only between 4 s and 6 s.
fn fixture() -> PathBuf {
    let vf = format!(
        "drawtext=fontfile={}:text='{TEXT}':fontcolor=black:fontsize=48:x={TX}:y={TY}\
         :enable=between(t\\,{T_ON}\\,{T_OFF})",
        font()
    );
    build_once(tmp().join("fixture.mp4"), |part| {
        run(&["-y", "-f", "lavfi", "-i", &format!("color=c=white:s={W}x{H}:d=20:r=25"),
              "-vf", &vf, "-c:v", "libx264", "-crf", "12", "-pix_fmt", "yuv420p",
              "-f", "mp4", &part.to_string_lossy()]);
    })
}

/// Same string in two places in the same frame (§11.5 ground truth, and a second OCR target).
fn fixture_two() -> PathBuf {
    let f = font();
    let vf = format!(
        "drawtext=fontfile={f}:text='{TEXT}':fontcolor=black:fontsize=48:x=120:y=120\
         :enable=between(t\\,{T_ON}\\,{T_OFF}),\
         drawtext=fontfile={f}:text='{TEXT}':fontcolor=black:fontsize=48:x=700:y=520\
         :enable=between(t\\,{T_ON}\\,{T_OFF})"
    );
    build_once(tmp().join("fixture-two.mp4"), |part| {
        run(&["-y", "-f", "lavfi", "-i", &format!("color=c=white:s={W}x{H}:d=20:r=25"),
              "-vf", &vf, "-c:v", "libx264", "-crf", "12", "-pix_fmt", "yuv420p",
              "-f", "mp4", &part.to_string_lossy()]);
    })
}

fn run(args: &[&str]) {
    let o = Command::new(tool("ffmpeg")).current_dir(tmp()).args(args).output().unwrap_or_else(|e| {
        panic!("cannot run {}: {e}. Put sidecars in src-tauri/binaries/ or set AUTOBLUR_FFMPEG.",
               tool("ffmpeg").display())
    });
    assert!(o.status.success(), "ffmpeg {args:?}\n{}", String::from_utf8_lossy(&o.stderr));
}

/// Sampled frames as (t, bgra).
fn sample(path: &Path, rate: f64, w: u32, h: u32) -> Vec<(f64, Vec<u8>)> {
    let mut r = FrameReader::new(path, &format!("fps={rate}"), w, h).unwrap();
    let mut v = Vec::new();
    while let Some((i, buf)) = r.next_frame().unwrap() {
        v.push((i as f64 / rate, buf));
    }
    r.finish().unwrap();
    v
}

fn px(buf: &[u8], w: u32, x: u32, y: u32) -> (u8, u8, u8) {
    let i = ((y * w + x) * 4) as usize;
    (buf[i + 2], buf[i + 1], buf[i]) // bgra -> rgb
}

/// Any pixel darker than mid-grey inside the rect.
fn has_ink(buf: &[u8], w: u32, r: (u32, u32, u32, u32)) -> bool {
    (r.1..r.1 + r.3).any(|y| (r.0..r.0 + r.2).any(|x| px(buf, w, x, y).0 < 128))
}

/* ------------------------------------------------------------ plumbing */

#[test]
fn parse_time_reads_ffmpeg_progress() {
    assert_eq!(parse_time("frame=1 fps=0 time=00:01:02.34 bitrate=N/A"), Some(62.34));
    assert_eq!(parse_time("time=00:00:00.00"), Some(0.0));
    assert_eq!(parse_time("no time here"), None);
    assert_eq!(parse_time("time=N/A"), None);
}

#[test]
fn a_rejected_filtergraph_still_yields_a_readable_error() {
    let noise = "x".repeat(9000);
    let tail = format!("{noise}\r\n[Parsed_crop_1] Failed to configure input pad\r\nConversion failed!");
    let e = readable_error(&tail);
    assert!(e.contains("Failed to configure input pad"), "real message lost: {e}");
    assert!(e.len() < 1000, "still dominated by the echoed graph: {} chars", e.len());
}

#[test]
fn union_is_the_bounding_box() {
    assert_eq!(union(&[[10.0, 10.0, 5.0, 5.0], [20.0, 8.0, 5.0, 4.0]]), [10.0, 8.0, 15.0, 7.0]);
    assert_eq!(union(&[]), [0.0, 0.0, 0.0, 0.0]);
}

/// Is a pid still a live process? Only used to prove nothing outlived its owner.
fn alive(pid: u32) -> bool {
    if cfg!(windows) {
        let o = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"]).output().unwrap();
        String::from_utf8_lossy(&o.stdout).contains(&pid.to_string())
    } else {
        Command::new("kill").args(["-0", &pid.to_string()]).output().unwrap().status.success()
    }
}

/// `std::process::Child` does not kill on drop, and every `?` in the OCR loop drops the reader.
/// A surviving ffmpeg blocks forever on a pipe nobody drains and keeps `ffmpeg.exe` open until
/// the machine reboots, which is what made the next installer stop with "error opening file for
/// writing". Nothing may outlive its owner.
#[test]
fn a_dropped_frame_reader_leaves_no_ffmpeg_behind() {
    let f = fixture();
    let i = probe(&f).unwrap();
    let pid = {
        let mut r = FrameReader::new(&f, "fps=2", i.width, i.height).unwrap();
        let pid = r.pid();
        r.next_frame().unwrap().expect("the reader should produce a frame");
        assert!(alive(pid), "ffmpeg should still be running while the reader is alive");
        pid // dropped here WITHOUT finish(), exactly as an early `?` would
    };
    for _ in 0..50 {
        if !alive(pid) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    panic!("ffmpeg {pid} outlived its FrameReader; it will hold ffmpeg.exe open until reboot");
}

#[test]
fn probe_reports_geometry_and_a_stable_hash() {
    let f = fixture();
    let i = probe(&f).unwrap();
    assert_eq!((i.width, i.height), (W, H));
    assert!((i.duration - 20.0).abs() < 0.2, "duration {}", i.duration);
    assert!((i.fps - 25.0).abs() < 0.1, "fps {}", i.fps);
    assert_eq!(i.sha256.len(), 64);
    assert_eq!(i.sha256, probe(&f).unwrap().sha256, "hash must be stable");
    assert_ne!(i.sha256, probe(&fixture_two()).unwrap().sha256, "different files, different hash");
}

/// §4: frame `i` of the rawvideo stream is source time `i / RATE`, exactly.
/// Checked without OCR, so a recognizer bug cannot mask a sampling bug.
#[test]
fn frame_index_maps_to_source_time() {
    let frames = sample(&fixture(), RATE, W, H);
    assert!(frames.len() >= 39 && frames.len() <= 41, "20 s @ 2 fps -> ~40 frames, got {}", frames.len());

    let region = (TX as u32 - 10, TY as u32 - 10, 500, 90);
    for (t, buf) in &frames {
        let ink = has_ink(buf, W, region);
        // ±dt around the boundaries: the sample at exactly 4.0 / 6.0 may fall either side.
        if *t <= T_ON - 1.0 / RATE || *t >= T_OFF + 1.0 / RATE {
            assert!(!ink, "text visible at {t}s, outside [{T_ON}, {T_OFF}]");
        } else if *t > T_ON && *t < T_OFF {
            assert!(ink, "text missing at {t}s, inside [{T_ON}, {T_OFF}]");
        }
    }
}

/* -------------------------------------------------------------- export */

#[test]
fn export_applies_the_graph_and_logs_hashes() {
    let inp = fixture();
    let out = tmp().join("exported.mp4");
    // drawbox over the text, enabled exactly over the padded span
    let graph = "drawbox=x=410:y=290:w=520:h=80:color=red@1:t=fill:enable=between(t\\,3.5\\,6.5)";
    let seen = std::cell::RefCell::new(Vec::<f64>::new());
    let rep = export(&inp, graph, &out, &|f| seen.borrow_mut().push(f), &|_| {}, &NO_CANCEL).unwrap();
    let seen = seen.into_inner();

    assert!(out.is_file());
    assert_eq!(rep.input_sha256, probe(&inp).unwrap().sha256);
    assert_eq!(rep.output_sha256, sha256_file(&out).unwrap());
    assert_ne!(rep.input_sha256, rep.output_sha256, "output is a derivative, not a copy");
    assert!(rep.command.iter().any(|a| a == "-/filter:v" || a == "-filter_script:v"),
            "the graph must go in from a file, never inline: {:?}", rep.command);
    assert!(!rep.command.iter().any(|a| a.contains("drawbox")), "graph leaked onto the command line");
    assert!(seen.iter().any(|&f| f > 0.0) && seen.last() == Some(&1.0), "progress {seen:?}");
    assert!(!rep.ffmpeg.is_empty(), "bundled ffmpeg version must be recorded");

    // the redaction actually landed, and only inside its span
    let frames = sample(&out, RATE, W, H);
    let at = |t: f64| &frames.iter().min_by(|a, b| (a.0 - t).abs().total_cmp(&(b.0 - t).abs())).unwrap().1;
    let (r, g, b) = px(at(5.0), W, 600, 320);
    assert!(r > 150 && g < 90 && b < 90, "box missing at 5 s: {r},{g},{b}");
    let (r, g, b) = px(at(1.0), W, 600, 320);
    assert!(r > 200 && g > 200 && b > 200, "box present outside its span at 1 s: {r},{g},{b}");
}

#[test]
fn export_refuses_to_touch_the_input() {
    let inp = fixture();
    let e = export(&inp, "drawbox=x=0:y=0:w=8:h=8:color=red@1:t=fill", &inp, &|_| {}, &|_| {}, &NO_CANCEL);
    assert!(e.is_err(), "must refuse to overwrite the input");
    assert!(export(&inp, "  ", &tmp().join("x.mp4"), &|_| {}, &|_| {}, &NO_CANCEL).is_err(), "must refuse an empty graph");
}

/* ------------------------------------------------------- OCR (Windows) */

#[cfg(windows)]
mod win {
    use super::*;

    /// Windows OCR needs a recognizer language pack, which hosted CI images may not have.
    /// A missing OS feature is not a broken build — but it must be loud, never silent, or the
    /// decisive round-trip test quietly stops covering anything.
    pub fn have_ocr() -> bool {
        match autoblur::ocr::languages() {
            Ok(l) if !l.is_empty() => true,
            _ => {
                eprintln!("SKIPPED: no Windows OCR recognizer installed, OCR tests did not run. \
                           Settings > Time & language > Language > Language options > \
                           Optional features > Optical character recognition.");
                false
            }
        }
    }

    fn ocr(path: &Path) -> OcrResult {
        let c = AtomicBool::new(false);
        autoblur::ocr_video(path, &OcrOpts { rate: RATE, lang: "en-US".into() }, &|_, _| {}, &c)
            .expect("OCR failed — is an English OCR language pack installed?")
    }

    fn hits<'a>(r: &'a OcrResult) -> Vec<&'a Line> {
        r.lines.iter().filter(|l| l.text.to_lowercase().replace(' ', "").contains("hauptstrasse")).collect()
    }

    #[test]
    fn languages_are_listed() {
        let l = autoblur::ocr::languages().unwrap_or_default();
        println!("recognizers: {l:?}   max image dimension: {}", autoblur::ocr::max_dim());
        if !have_ocr() { return; }
        assert!(l.iter().all(|s| s.contains('-')), "expected BCP-47 tags, got {l:?}");
    }

    /// §11.1 — coordinates are reported in SOURCE VIDEO PIXELS, origin top-left.
    /// A mismatch here silently misplaces every redaction.
    #[test]
    fn coordinate_fidelity() {
        if !have_ocr() { return; }
        let r = ocr(&fixture());
        assert_eq!((r.width, r.height), (W, H));
        let h = hits(&r);
        assert!(!h.is_empty(), "OCR found nothing; lines: {:?}", r.lines.iter().map(|l| &l.text).collect::<Vec<_>>());
        let l = h[0];
        println!("rect {:?} for {:?} at {}s", l.rect, l.text, l.t);
        assert!((l.rect[0] - TX).abs() <= 8.0, "x {} vs {TX}", l.rect[0]);
        assert!((l.rect[1] - TY).abs() <= 8.0, "y {} vs {TY}", l.rect[1]);
        assert!(l.rect[2] > 100.0 && l.rect[0] + l.rect[2] < W as f64, "w {}", l.rect[2]);
        assert!(l.words.len() >= 2, "word rects are needed when only part of a line is sensitive");
        let u = union(&l.words.iter().map(|w| w.rect).collect::<Vec<_>>());
        assert!((u[0] - l.rect[0]).abs() < 0.01, "line rect must be the union of its word rects");
    }

    /// §11.2 — text present only from 4.0 s to 6.0 s produces occurrences only there, ±dt.
    #[test]
    fn timestamp_fidelity() {
        if !have_ocr() { return; }
        let r = ocr(&fixture());
        let dt = 1.0 / RATE;
        for l in hits(&r) {
            assert!(l.t >= T_ON - dt - 1e-6 && l.t <= T_OFF + dt + 1e-6, "occurrence at {}s", l.t);
        }
        assert!(hits(&r).len() >= 3, "expected ~5 samples in a 2 s window, got {}", hits(&r).len());
    }

    /// §11.5 ground truth — the same string twice in one frame must stay two occurrences.
    #[test]
    fn two_positions_stay_separate_occurrences() {
        if !have_ocr() { return; }
        let r = ocr(&fixture_two());
        let mut by_t = std::collections::HashMap::<String, usize>::new();
        for l in hits(&r) {
            *by_t.entry(format!("{:.2}", l.t)).or_default() += 1;
        }
        assert!(by_t.values().any(|&n| n >= 2), "expected 2 occurrences in one frame: {by_t:?}");
    }

    /// §11.6 — the decisive one. Export through the app's own matcher and filtergraph
    /// (tools/gen-boxes.mjs runs the same code the UI runs), then re-OCR the output.
    #[test]
    #[ignore = "slow: two OCR passes plus an encode; run with --ignored"]
    fn round_trip_leaves_no_text() {
        if !have_ocr() { return; }
        let inp = fixture();
        let r = ocr(&inp);
        let oj = tmp().join("ocr.json");
        write_text(&oj, &serde_json::to_string(&r).unwrap()).unwrap();

        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let o = Command::new("node")
            .current_dir(root)
            .args(["tools/gen-boxes.mjs", &oj.to_string_lossy(), TEXT])
            .output()
            .expect("node is needed for the round trip test");
        assert!(o.status.success(), "gen-boxes: {}", String::from_utf8_lossy(&o.stderr));
        let graph = String::from_utf8(o.stdout).unwrap();
        println!("graph: {graph}");
        assert!(!graph.trim().is_empty());

        let out = tmp().join("roundtrip.mp4");
        export(&inp, &graph, &out, &|_| {}, &|_| {}, &NO_CANCEL).unwrap();

        let after = ocr(&out);
        let left = hits(&after);
        assert!(left.is_empty(), "redacted text still readable after export at {:?}",
                left.iter().map(|l| (l.t, &l.text)).collect::<Vec<_>>());
    }
}

/* --------------------------------------------- moving text at video fps */

/// Text that moves across the frame, sampled at the video's own rate. This is the case that
/// produced "the box misses frames during movement" and a failing ffmpeg command.
#[cfg(windows)]
mod moving {
    use super::*;

    fn fixture_moving() -> PathBuf {
        // Moves the whole time but stays fully inside the frame, and is occluded for 0.4 s in
        // the middle so OCR loses it — the bridged-gap path the box has to survive.
        let vf = format!(
            "drawtext=fontfile={}:text='{TEXT}':fontcolor=black:fontsize=48:x='100+t*60':y=300,\
             drawbox=x=0:y=280:w=1280:h=90:color=white@1:t=fill:enable=between(t\\,5\\,5.4)",
            font()
        );
        build_once(tmp().join("fixture-moving.mp4"), |part| {
            run(&["-y", "-f", "lavfi", "-i", "color=c=white:s=1280x720:d=10:r=30",
                  "-vf", &vf, "-c:v", "libx264", "-crf", "12", "-pix_fmt", "yuv420p",
                  "-f", "mp4", &part.to_string_lossy()]);
        })
    }

    #[test]
    #[ignore = "stress: OCRs 300 frames"]
    fn moving_text_survives_export() {
        if !super::win::have_ocr() { return; }
        let inp = fixture_moving();
        let c = AtomicBool::new(false);
        let r = autoblur::ocr_video(
            &inp, &OcrOpts { rate: 30.0, lang: "en-US".into() }, &|_, _| {}, &c).unwrap();
        let hits = r.lines.iter()
            .filter(|l| l.text.to_lowercase().replace(' ', "").contains("hauptstrasse"))
            .count();
        println!("sampled {} frames, {hits} hits", (r.duration * r.rate) as usize);

        let oj = tmp().join("ocr-moving.json");
        write_text(&oj, &serde_json::to_string(&r).unwrap()).unwrap();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let o = Command::new("node").current_dir(root)
            .args(["tools/gen-boxes.mjs", &oj.to_string_lossy(), TEXT, "pixelate"])
            .output().unwrap();
        assert!(o.status.success(), "gen-boxes: {}", String::from_utf8_lossy(&o.stderr));
        let graph = String::from_utf8(o.stdout).unwrap();
        println!("graph is {} bytes", graph.len());

        let out = tmp().join("moving-out.mp4");
        let rep = export(&inp, &graph, &out, &|_| {}, &|_| {}, &NO_CANCEL);
        match &rep {
            Err(e) => panic!("EXPORT FAILED with a {}-byte graph:\n{e}", graph.len()),
            Ok(_) => {}
        }

        let after = autoblur::ocr_video(
            &out, &OcrOpts { rate: 30.0, lang: "en-US".into() }, &|_, _| {}, &c).unwrap();
        let left: Vec<_> = after.lines.iter()
            .filter(|l| l.text.to_lowercase().replace(' ', "").contains("hauptstrasse"))
            .map(|l| l.t).collect();
        assert!(left.is_empty(), "text still readable at {left:?}");
    }
}
