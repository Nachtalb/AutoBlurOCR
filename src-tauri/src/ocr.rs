//! Windows.Media.Ocr. Ships with the OS, no model files, word-level bounding boxes.
//! One engine, one code path — no engine abstraction until a real video fails (§3).

use crate::{union, Error, Line, Result, Word};

use windows::core::{Interface, HSTRING};
use windows::Globalization::Language;
use windows::Graphics::Imaging::{BitmapAlphaMode, BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine;
use windows::Storage::Streams::Buffer;
use windows::Win32::System::Com::CoIncrementMTAUsage;
use windows::Win32::System::WinRT::{IBufferByteAccess, RoInitialize, RO_INIT_MULTITHREADED};

impl From<windows::core::Error> for Error {
    fn from(e: windows::core::Error) -> Self {
        Error(e.message().to_string())
    }
}

/// Every thread that touches WinRT needs an apartment. Once per thread, not once per frame:
/// each successful `RoInitialize` bumps a per-thread count that is never given back.
///
/// The MTA reference is the load-bearing part. Without it, the last thread that called
/// `RoInitialize` exiting takes the whole multi-threaded apartment down with it — and WinRT
/// frees the activation factories that windows-rs caches in process statics. Those caches are
/// never invalidated, so the next `Buffer::Create` on any thread dereferences a freed vtable
/// and the process dies with an access violation. `CoIncrementMTAUsage` holds the apartment
/// open for the life of the process; it is deliberately never released.
fn init_thread() {
    static MTA: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    MTA.get_or_init(|| unsafe {
        let _ = CoIncrementMTAUsage();
    });
    thread_local! { static DONE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) } }
    DONE.with(|d| {
        if !d.get() {
            // S_FALSE / RPC_E_CHANGED_MODE just mean the apartment is already up.
            unsafe { let _ = RoInitialize(RO_INIT_MULTITHREADED); }
            d.set(true);
        }
    });
}

/// Installed recognizers, so the UI offers what actually exists instead of failing at runtime.
pub fn languages() -> Result<Vec<String>> {
    init_thread();
    let mut v = Vec::new();
    for l in OcrEngine::AvailableRecognizerLanguages()? {
        v.push(l.LanguageTag()?.to_string());
    }
    Ok(v)
}

pub fn max_dim() -> u32 {
    init_thread();
    OcrEngine::MaxImageDimension().unwrap_or(2600)
}

/// A recognizer. One is needed **per worker**: a second `RecognizeAsync` on an engine that is
/// still busy fails with "Another RecognizeAsync operation is already running!", so engines
/// cannot be shared across the pool even though the type is agile.
pub struct Engine(OcrEngine);

impl Engine {
    pub fn new(lang: &str) -> Result<Self> {
        init_thread();
        if !lang.trim().is_empty() {
            if let Ok(l) = Language::CreateLanguage(&HSTRING::from(lang)) {
                if let Ok(e) = OcrEngine::TryCreateFromLanguage(&l) {
                    return Ok(Engine(e));
                }
            }
        }
        OcrEngine::TryCreateFromUserProfileLanguages().map(Engine).map_err(|_| {
            let have = languages().unwrap_or_default();
            Error(format!(
                "no Windows OCR recognizer for {lang:?}. Installed: [{}]. \
                 Add one under Settings > Time & language > Language > (language) > \
                 Language options > Optional features > Optical character recognition.",
                have.join(", ")
            ))
        })
    }

    pub fn lang(&self) -> String {
        self.0
            .RecognizerLanguage()
            .and_then(|l| l.LanguageTag())
            .map(|t| t.to_string())
            .unwrap_or_default()
    }

    /// One frame of BGRA -> lines. `t` is left at 0.0; the caller stamps the source timestamp.
    /// Rects come back in bitmap pixels, origin top-left.
    pub fn recognize(&self, bgra: &[u8], w: u32, h: u32) -> Result<Vec<Line>> {
        init_thread();
        let bmp = bitmap(bgra, w, h)?;
        let res = self.0.RecognizeAsync(&bmp)?.join()?;
        let mut out = Vec::new();
        for line in res.Lines()? {
            let text = line.Text()?.to_string();
            if text.trim().is_empty() {
                continue;
            }
            let mut words = Vec::new();
            for word in line.Words()? {
                let r = word.BoundingRect()?;
                words.push(Word {
                    text: word.Text()?.to_string(),
                    rect: [r.X as f64, r.Y as f64, r.Width as f64, r.Height as f64],
                });
            }
            // Windows OCR reports a rect per word only — the line rect is their union.
            let rect = union(&words.iter().map(|w| w.rect).collect::<Vec<_>>());
            out.push(Line { t: 0.0, text, conf: 0.0, rect, words });
        }
        Ok(out)
    }
}

fn bitmap(bgra: &[u8], w: u32, h: u32) -> Result<SoftwareBitmap> {
    let n = bgra.len() as u32;
    let buf = Buffer::Create(n)?;
    buf.SetLength(n)?;
    let access: IBufferByteAccess = buf.cast()?;
    unsafe {
        let dst = access.Buffer()?;
        std::ptr::copy_nonoverlapping(bgra.as_ptr(), dst, bgra.len());
    }
    // ffmpeg's bgra is straight alpha at 255, so premultiplied is the same bytes and is what
    // the recognizer expects.
    Ok(SoftwareBitmap::CreateCopyWithAlphaFromBuffer(
        &buf,
        BitmapPixelFormat::Bgra8,
        w as i32,
        h as i32,
        BitmapAlphaMode::Premultiplied,
    )?)
}
