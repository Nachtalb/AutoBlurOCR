//! AutoBlur core. No Tauri types in any signature here, so `cargo test` drives it headlessly.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
#[cfg_attr(not(windows), allow(unused_imports))]
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(windows)]
pub mod ocr;

/* ---------------------------------------------------------------- errors */

#[derive(Debug)]
pub struct Error(pub String);
pub type Result<T> = std::result::Result<T, Error>;

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Error {}
impl Serialize for Error {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error(e.to_string())
    }
}
impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error(e.to_string())
    }
}
#[macro_export]
macro_rules! bail {
    ($($t:tt)*) => { return Err($crate::Error(format!($($t)*))) };
}

/* ------------------------------------------------------------- sidecars */

/// Bundled sidecar, never `PATH`: a forensic workstation's `PATH` is not ours to assume and
/// version drift changes filter behaviour.
///
/// Order: `AUTOBLUR_FFMPEG` / `AUTOBLUR_FFPROBE` override (tests) → next to the executable
/// (how Tauri bundles `externalBin`) → `src-tauri/binaries/<name>-<triple>` (dev + `cargo test`).
pub fn tool(name: &str) -> PathBuf {
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    if let Ok(p) = std::env::var(format!("AUTOBLUR_{}", name.to_uppercase())) {
        return PathBuf::from(p);
    }
    if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
        let p = dir.join(format!("{name}{suffix}"));
        if p.is_file() {
            return p;
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("binaries")
        .join(format!("{name}-{}{suffix}", env!("TARGET_TRIPLE")))
}

fn spawn(path: &Path, args: &[&str]) -> Result<Child> {
    let mut c = Command::new(path);
    c.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c.spawn().map_err(|e| {
        Error(format!(
            "cannot run {}: {e}. Put the bundled sidecars in src-tauri/binaries/ \
             (see binaries/README.md).",
            path.display()
        ))
    })
}

/// First line of `<tool> -version`, recorded in the export log.
pub fn tool_version(name: &str) -> String {
    let p = tool(name);
    match spawn(&p, &["-version"]) {
        Ok(c) => match c.wait_with_output() {
            Ok(o) => String::from_utf8_lossy(&o.stdout).lines().next().unwrap_or("").trim().to_string(),
            Err(_) => String::new(),
        },
        Err(_) => String::new(),
    }
}

/* ---------------------------------------------------------------- probe */

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct VideoInfo {
    pub path: String,
    pub width: u32,
    pub height: u32,
    pub duration: f64,
    pub fps: f64,
    pub sha256: String,
}

/// Input is opened read-only and never written to (§10).
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(format!("{:x}", h.finalize()))
}

fn ratio(s: &str) -> f64 {
    match s.split_once('/') {
        Some((a, b)) => {
            let (a, b) = (a.parse::<f64>().unwrap_or(0.0), b.parse::<f64>().unwrap_or(0.0));
            if b == 0.0 { 0.0 } else { a / b }
        }
        None => s.parse().unwrap_or(0.0),
    }
}

pub fn probe(path: &Path) -> Result<VideoInfo> {
    if !path.is_file() {
        bail!("no such file: {}", path.display());
    }
    let p = path.to_string_lossy().to_string();
    let out = spawn(
        &tool("ffprobe"),
        &["-v", "error", "-print_format", "json", "-show_streams", "-show_format", &p],
    )?
    .wait_with_output()?;
    if !out.status.success() {
        bail!("ffprobe failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let j: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let vs = j["streams"]
        .as_array()
        .and_then(|a| a.iter().find(|s| s["codec_type"] == "video"))
        .ok_or_else(|| Error(format!("{p} has no video stream")))?;

    let width = vs["width"].as_u64().unwrap_or(0) as u32;
    let height = vs["height"].as_u64().unwrap_or(0) as u32;
    if width == 0 || height == 0 {
        bail!("ffprobe reported no frame size for {p}");
    }
    let duration = j["format"]["duration"]
        .as_str()
        .or_else(|| vs["duration"].as_str())
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0);
    let fps = ratio(vs["avg_frame_rate"].as_str().unwrap_or("0/0"));
    let fps = if fps > 0.0 { fps } else { ratio(vs["r_frame_rate"].as_str().unwrap_or("25/1")) };

    Ok(VideoInfo { path: p, width, height, duration, fps, sha256: sha256_file(path)? })
}

/* -------------------------------------------------------- frame sampling */

/// Raw BGRA frames piped out of ffmpeg. Frame `i` is source time `i / rate` exactly (§4).
/// The rawvideo stream carries no header, so the caller supplies the geometry from `probe`.
pub struct FrameReader {
    child: Child,
    out: BufReader<std::process::ChildStdout>,
    pub width: u32,
    pub height: u32,
    frame: usize,
}

impl FrameReader {
    pub fn new(path: &Path, vf: &str, width: u32, height: u32) -> Result<Self> {
        let p = path.to_string_lossy().to_string();
        let mut child = spawn(
            &tool("ffmpeg"),
            &["-hide_banner", "-loglevel", "error", "-nostdin", "-i", &p, "-vf", vf,
              "-pix_fmt", "bgra", "-f", "rawvideo", "-"],
        )?;
        let out = BufReader::with_capacity(1 << 20, child.stdout.take().unwrap());
        Ok(FrameReader { child, out, width, height, frame: 0 })
    }

    pub fn frame_bytes(&self) -> usize {
        self.width as usize * self.height as usize * 4
    }

    /// `Ok(None)` at end of stream. A short final read is a truncated frame and is discarded.
    pub fn next_frame(&mut self) -> Result<Option<(usize, Vec<u8>)>> {
        let mut buf = vec![0u8; self.frame_bytes()];
        let mut got = 0;
        while got < buf.len() {
            match self.out.read(&mut buf[got..])? {
                0 => break,
                n => got += n,
            }
        }
        if got < buf.len() {
            return Ok(None);
        }
        let i = self.frame;
        self.frame += 1;
        Ok(Some((i, buf)))
    }

    pub fn finish(mut self) -> Result<()> {
        let _ = self.child.kill();
        let mut err = String::new();
        if let Some(mut s) = self.child.stderr.take() {
            let _ = s.read_to_string(&mut err);
        }
        let _ = self.child.wait();
        if !err.trim().is_empty() && err.contains("Error") {
            bail!("ffmpeg: {}", err.trim());
        }
        Ok(())
    }
}

/* ------------------------------------------------------------ OCR result */

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct OcrOpts {
    pub rate: f64,
    pub lang: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Word {
    pub text: String,
    /// x, y, w, h in SOURCE VIDEO PIXELS, origin top-left — the editor's box-model space.
    pub rect: [f64; 4],
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Line {
    pub t: f64,
    pub text: String,
    /// Windows OCR gives no confidence; the field is kept so the schema does not change later.
    pub conf: f64,
    pub rect: [f64; 4],
    pub words: Vec<Word>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct OcrResult {
    pub video: String,
    pub video_sha256: String,
    pub width: u32,
    pub height: u32,
    pub duration: f64,
    /// Sampling rate used. `video_fps` is the source's own rate: when they match, every frame
    /// was read and nothing has to be inferred between readings.
    pub rate: f64,
    pub video_fps: f64,
    pub engine: String,
    pub lang: String,
    pub generated: String,
    /// < 1.0 when the frame exceeded the engine's max image dimension and had to be
    /// downscaled for recognition. Rects are already scaled back to source pixels.
    pub ocr_scale: f64,
    pub lines: Vec<Line>,
}

/// Union of the word rects — Windows OCR reports a rect per word, not per line.
pub fn union(rects: &[[f64; 4]]) -> [f64; 4] {
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for r in rects {
        x0 = x0.min(r[0]);
        y0 = y0.min(r[1]);
        x1 = x1.max(r[0] + r[2]);
        y1 = y1.max(r[1] + r[3]);
    }
    if x1 < x0 { [0.0, 0.0, 0.0, 0.0] } else { [x0, y0, x1 - x0, y1 - y0] }
}

pub fn now_iso() -> String {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // days -> civil date, Howard Hinnant's algorithm. UTC; no chrono for one timestamp.
    let (days, secs) = ((d / 86400) as i64, d % 86400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let dd = doy - (153 * mp + 2) / 5 + 1;
    let mm = if mp < 10 { mp + 3 } else { mp - 9 };
    let yy = yoe + era * 400 + if mm <= 2 { 1 } else { 0 };
    format!(
        "{yy:04}-{mm:02}-{dd:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        (secs / 60) % 60,
        secs % 60
    )
}

/* --------------------------------------------------------------- export */

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ExportReport {
    pub input: String,
    pub input_sha256: String,
    pub output: String,
    pub output_sha256: String,
    pub tool_version: String,
    pub ffmpeg: String,
    pub command: Vec<String>,
    pub filtergraph: String,
    pub duration: f64,
    pub derivative: String,
}

/// Ways to hand ffmpeg a filtergraph from a file, newest first. `-/filter:v` is the generic
/// "read this option's value from a file" syntax (ffmpeg 7.0+); `-filter_script:v` is the old
/// spelling, removed in ffmpeg 8. Which one works depends on the bundled build, so the first
/// export tries the new one and falls back — an unrecognised option fails instantly, before any
/// encoding, so the retry costs nothing. The answer is remembered for the process.
const FILTER_FILE_FLAGS: [&str; 2] = ["-/filter:v", "-filter_script:v"];
static FILTER_FLAG: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Runs ffmpeg, pumping `time=` out of its stderr. `Ok((success, stderr_tail))`.
fn run_encode(
    ffmpeg: &Path,
    args: &[&str],
    duration: f64,
    progress: &dyn Fn(f64),
) -> Result<(bool, String)> {
    let mut child = spawn(ffmpeg, args)?;
    // ffmpeg writes progress to stderr as `time=HH:MM:SS.ss`, separated by \r not \n
    let mut tail = String::new();
    if let Some(stderr) = child.stderr.take() {
        let mut rd = BufReader::new(stderr);
        let mut chunk = Vec::new();
        loop {
            chunk.clear();
            if rd.read_until(b'\r', &mut chunk)? == 0 {
                break;
            }
            let s = String::from_utf8_lossy(&chunk);
            for line in s.split(['\r', '\n']) {
                if let Some(t) = parse_time(line) {
                    if duration > 0.0 {
                        progress((t / duration).clamp(0.0, 1.0));
                    }
                }
            }
            tail.push_str(&s);
            if tail.len() > 8000 {
                tail = tail[tail.len() - 4000..].to_string();
            }
        }
    }
    Ok((child.wait()?.success(), tail))
}

/// The graph goes in via a file, never inline: it contains escaped commas and Windows argument
/// quoting mangles them.
pub fn export(
    path: &Path,
    filtergraph: &str,
    out: &Path,
    progress: &dyn Fn(f64),
) -> Result<ExportReport> {
    let info = probe(path)?;
    if filtergraph.trim().is_empty() {
        bail!("empty filtergraph — nothing to redact");
    }
    if path == out {
        bail!("refusing to overwrite the input: the input is evidence and is opened read-only");
    }

    // PID alone is not unique enough: two exports in one process would share the file, and one
    // would delete it while the other's ffmpeg is still reading it.
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let script = std::env::temp_dir().join(format!(
        "autoblur-boxes-{}-{}.txt",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&script, filtergraph)?;

    let (pi, ps, po) = (
        path.to_string_lossy().to_string(),
        script.to_string_lossy().to_string(),
        out.to_string_lossy().to_string(),
    );
    let ffmpeg = tool("ffmpeg");
    let mut flag = FILTER_FLAG.load(Ordering::Relaxed);
    let args = loop {
        let args = vec![
            "-hide_banner", "-nostdin", "-y", "-i", &pi, FILTER_FILE_FLAGS[flag], &ps,
            "-c:v", "libx264", "-crf", "18", "-preset", "veryfast", "-c:a", "copy", &po,
        ];
        let (ok, tail) = run_encode(&ffmpeg, &args, info.duration, progress)?;
        if ok {
            FILTER_FLAG.store(flag, Ordering::Relaxed);
            break args;
        }
        if flag + 1 < FILTER_FILE_FLAGS.len() && tail.contains("Unrecognized option") {
            flag += 1;
            continue;
        }
        let _ = std::fs::remove_file(&script);
        bail!("ffmpeg export failed:\n{}", readable_error(&tail));
    };
    let _ = std::fs::remove_file(&script);
    progress(1.0);

    let command: Vec<String> = std::iter::once(ffmpeg.to_string_lossy().to_string())
        .chain(args.iter().map(|s| s.to_string()))
        .collect();

    Ok(ExportReport {
        input: pi,
        input_sha256: info.sha256,
        output: po,
        output_sha256: sha256_file(out)?,
        tool_version: env!("CARGO_PKG_VERSION").to_string(),
        ffmpeg: tool_version("ffmpeg"),
        command,
        filtergraph: filtergraph.to_string(),
        duration: info.duration,
        derivative: "Output is a derivative exhibit: re-encoded with libx264 -crf 18, \
                     not a bit-exact copy of the input."
            .to_string(),
    })
}

/// ffmpeg echoes the whole filtergraph back when it rejects one, as a single line thousands of
/// characters long, which shoves the actual diagnostic out of any tail you keep. Clip each line
/// so the message survives.
pub fn readable_error(tail: &str) -> String {
    let clipped: Vec<String> = tail
        .split(['\r', '\n'])
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            if l.chars().count() > 200 {
                let head: String = l.chars().take(200).collect();
                format!("{head}… [{} chars of filtergraph elided]", l.chars().count())
            } else {
                l.to_string()
            }
        })
        .collect();
    let keep = clipped.len().saturating_sub(12);
    clipped[keep..].join("\n")
}

/// `... time=00:01:02.34 ...` -> 62.34
pub fn parse_time(line: &str) -> Option<f64> {
    let rest = line.split("time=").nth(1)?.trim_start();
    let hms = rest.split_whitespace().next()?;
    let mut p = hms.split(':');
    let h: f64 = p.next()?.parse().ok()?;
    let m: f64 = p.next()?.parse().ok()?;
    let s: f64 = p.next()?.parse().ok()?;
    Some(h * 3600.0 + m * 60.0 + s)
}

/* ------------------------------------------------------------ ocr_video */

/// Sample, OCR and index every text occurrence. Cross-platform signature; the recognizer
/// itself is Windows-only, so a non-Windows build fails here rather than silently faking it.
#[cfg(not(windows))]
pub fn ocr_video(
    _path: &Path,
    _opts: &OcrOpts,
    _progress: &dyn Fn(usize, usize),
    _cancel: &AtomicBool,
) -> Result<OcrResult> {
    bail!("OCR needs Windows.Media.Ocr; this build is not for Windows")
}

#[cfg(windows)]
pub fn ocr_video(
    path: &Path,
    opts: &OcrOpts,
    progress: &dyn Fn(usize, usize),
    cancel: &AtomicBool,
) -> Result<OcrResult> {
    use rayon::prelude::*;

    let info = probe(path)?;
    if opts.rate <= 0.0 {
        bail!("sampling rate must be > 0");
    }

    // OCR at full resolution — downscaling is the fastest way to lose small text — unless the
    // frame exceeds what the engine accepts, in which case scale and map the rects back.
    let max = ocr::max_dim() as f64;
    let big = info.width.max(info.height) as f64;
    let (ow, oh) = if big > max {
        let s = max / big;
        (((info.width as f64 * s / 2.0).floor() * 2.0) as u32,
         ((info.height as f64 * s / 2.0).floor() * 2.0) as u32)
    } else {
        (info.width, info.height)
    };
    let (sx, sy) = (info.width as f64 / ow as f64, info.height as f64 / oh as f64);

    let mut vf = format!("fps={}", opts.rate);
    if ow != info.width {
        vf.push_str(&format!(",scale={ow}:{oh}:flags=lanczos"));
    }

    let pool = ocr_pool();
    let workers = pool.current_num_threads();
    // One engine per worker, indexed by rayon's worker id — an engine can only run one
    // recognition at a time.
    let engines = (0..workers).map(|_| ocr::Engine::new(&opts.lang)).collect::<Result<Vec<_>>>()?;
    let total = ((info.duration * opts.rate).ceil() as usize).max(1);
    let mut reader = FrameReader::new(path, &vf, ow, oh)?;
    let mut lines: Vec<Line> = Vec::new();
    let mut done = 0usize;

    loop {
        if cancel.load(Ordering::Relaxed) {
            reader.finish()?;
            bail!("cancelled after {done} frames");
        }
        let mut batch = Vec::with_capacity(workers);
        for _ in 0..workers {
            match reader.next_frame()? {
                Some(f) => batch.push(f),
                None => break,
            }
        }
        if batch.is_empty() {
            break;
        }
        // parallel across frames, then reassembled in frame order — timestamps must not shuffle
        let mut got: Vec<(usize, Vec<Line>)> = pool.install(|| {
            batch
                .par_iter()
                .map(|(i, buf)| {
                    let t = *i as f64 / opts.rate;
                    let engine = &engines[rayon::current_thread_index().unwrap_or(0)];
                    engine.recognize(buf, ow, oh).map(|ls| {
                        (*i, ls.into_iter()
                            .map(|mut l| {
                                l.t = t;
                                scale_line(&mut l, sx, sy);
                                l
                            })
                            .collect())
                    })
                })
                .collect::<Result<Vec<_>>>()
        })?;
        got.sort_by_key(|(i, _)| *i);
        for (_, ls) in got {
            lines.extend(ls);
        }
        done += batch.len();
        progress(done, total.max(done));
    }
    reader.finish()?;
    progress(done, done.max(1));

    Ok(OcrResult {
        video: path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default(),
        video_sha256: info.sha256,
        width: info.width,
        height: info.height,
        duration: info.duration,
        rate: opts.rate,
        video_fps: info.fps,
        engine: "Windows.Media.Ocr".into(),
        lang: engines[0].lang(),
        generated: now_iso(),
        ocr_scale: ow as f64 / info.width as f64,
        lines,
    })
}

/// One pool for the process, bounded to `num_cpus - 1`. It has to outlive any single scan:
/// each worker caches an `OcrEngine`, and tearing workers down per run would both throw that
/// cache away and release WinRT objects from dying threads.
#[cfg(windows)]
fn ocr_pool() -> &'static rayon::ThreadPool {
    static POOL: std::sync::OnceLock<rayon::ThreadPool> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(num_cpus::get().saturating_sub(1).max(1))
            .thread_name(|i| format!("autoblur-ocr-{i}"))
            .build()
            .expect("rayon pool")
    })
}

#[cfg(windows)]
fn scale_line(l: &mut Line, sx: f64, sy: f64) {
    let f = |r: &mut [f64; 4]| {
        r[0] *= sx;
        r[1] *= sy;
        r[2] *= sx;
        r[3] *= sy;
    };
    f(&mut l.rect);
    for w in &mut l.words {
        f(&mut w.rect);
    }
}

/* ------------------------------------------------------------ small I/O */

pub fn write_text(path: &Path, text: &str) -> Result<()> {
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let mut f = std::fs::File::create(path)?;
    f.write_all(text.as_bytes())?;
    Ok(())
}

pub fn read_text(path: &Path) -> Result<String> {
    Ok(std::fs::read_to_string(path)?)
}

/// Recents live in a JSON file next to the executable.
pub fn recents_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf))
        .unwrap_or_else(std::env::temp_dir)
        .join("autoblur-recents.json")
}

pub fn recents() -> Vec<String> {
    std::fs::read_to_string(recents_path())
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
        .unwrap_or_default()
}

pub fn set_recents(list: &[String]) {
    let _ = std::fs::write(
        recents_path(),
        serde_json::to_string_pretty(list).unwrap_or_else(|_| "[]".into()),
    );
}
