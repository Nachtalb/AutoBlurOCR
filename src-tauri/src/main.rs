#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Thin Tauri wrapper. All state lives in the frontend's box model; the backend is stateless
//! apart from the cancel flag.

use autoblur::{Error, ExportReport, OcrOpts, OcrResult, VideoInfo};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_dialog::DialogExt;

static CANCEL: AtomicBool = AtomicBool::new(false);
static CANCEL_EXPORT: AtomicBool = AtomicBool::new(false);

type R<T> = std::result::Result<T, Error>;

/* ------------------------------------------------------------- dialogs */

#[tauri::command]
async fn pick_video(app: AppHandle) -> Option<String> {
    app.dialog()
        .file()
        .add_filter("video", &["mp4", "mkv", "mov", "avi", "m4v", "mts", "webm", "wmv"])
        .blocking_pick_file()
        .map(|f| f.to_string())
}

#[tauri::command]
async fn pick_open(app: AppHandle) -> Option<String> {
    app.dialog().file().add_filter("json", &["json"]).blocking_pick_file().map(|f| f.to_string())
}

#[tauri::command]
async fn pick_save(app: AppHandle, name: String) -> Option<String> {
    let n = Path::new(&name);
    let ext = n.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_else(|| "json".into());
    app.dialog()
        .file()
        .set_file_name(n.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or(name.clone()))
        .add_filter(&ext, &[&ext])
        .blocking_save_file()
        .map(|f| f.to_string())
}

/* --------------------------------------------------------------- media */

#[tauri::command]
fn probe(app: AppHandle, path: String) -> R<VideoInfo> {
    let p = PathBuf::from(&path);
    // The webview cannot play a raw Windows path; it goes through the asset protocol, whose
    // scope has to be widened to this file before convertFileSrc() will load.
    let _ = app.asset_protocol_scope().allow_file(&p);
    autoblur::probe(&p)
}

#[tauri::command]
fn ocr_languages() -> R<Vec<String>> {
    #[cfg(windows)]
    {
        autoblur::ocr::languages()
    }
    #[cfg(not(windows))]
    {
        Ok(vec![])
    }
}

#[tauri::command]
fn ocr_cancel() {
    CANCEL.store(true, Ordering::SeqCst);
}

#[tauri::command]
fn export_cancel() {
    CANCEL_EXPORT.store(true, Ordering::SeqCst);
}

#[tauri::command]
async fn ocr_video(app: AppHandle, path: String, rate: f64, lang: String) -> R<OcrResult> {
    CANCEL.store(false, Ordering::SeqCst);
    tauri::async_runtime::spawn_blocking(move || {
        let a = app.clone();
        let progress = move |done: usize, total: usize| {
            let _ = a.emit("ocr://progress", json!({ "done": done, "total": total }));
        };
        autoblur::ocr_video(Path::new(&path), &OcrOpts { rate, lang }, &progress, &CANCEL)
    })
    .await
    .map_err(|e| Error(e.to_string()))?
}

#[tauri::command]
async fn export(
    app: AppHandle,
    path: String,
    filtergraph: String,
    out: String,
    meta: Value,
) -> R<ExportReport> {
    CANCEL_EXPORT.store(false, Ordering::SeqCst);
    tauri::async_runtime::spawn_blocking(move || {
        let a = app.clone();
        let b = app.clone();
        let progress = move |f: f64| {
            let _ = a.emit("export://progress", f);
        };
        // Every ffmpeg line reaches the UI as it arrives. A render that is merely slow and one
        // that is wedged look identical from a percentage; ffmpeg's own output says which.
        let log = move |line: &str| {
            let _ = b.emit("export://log", line);
        };
        let report = autoblur::export(
            Path::new(&path), &filtergraph, Path::new(&out), &progress, &log, &CANCEL_EXPORT)?;

        // §10: the artefact that answers "what exactly did you do to this video"
        let mut log = serde_json::to_value(&report)?;
        log["meta"] = meta;
        log["written"] = json!(autoblur::now_iso());
        autoblur::write_text(
            Path::new(&format!("{out}.redaction-log.json")),
            &serde_json::to_string_pretty(&log)?,
        )?;
        Ok(report)
    })
    .await
    .map_err(|e| Error(e.to_string()))?
}

/// Fold the verify result into an existing redaction log.
#[tauri::command]
fn append_log(path: String, key: String, value: Value) -> R<()> {
    let p = PathBuf::from(&path);
    let mut log: Value = serde_json::from_str(&autoblur::read_text(&p)?)?;
    log[key] = value;
    autoblur::write_text(&p, &serde_json::to_string_pretty(&log)?)
}

/// Show the file in Explorer, or open it in whatever plays it. Local only — no network, and
/// nothing is launched that the user did not just produce.
#[tauri::command]
fn reveal(path: String, folder: bool) -> R<()> {
    let p = PathBuf::from(&path);
    if !p.exists() {
        return Err(Error(format!("{path} is gone")));
    }
    let mut c = std::process::Command::new("explorer.exe");
    if folder {
        c.arg("/select,").arg(&p);
    } else {
        c.arg(&p);
    }
    // explorer.exe returns a non-zero exit code even when it succeeds, so the status is useless.
    c.spawn().map(|_| ()).map_err(|e| Error(e.to_string()))
}

/* ---------------------------------------------------------------- files */

#[tauri::command]
fn write_text(path: String, text: String) -> R<()> {
    autoblur::write_text(Path::new(&path), &text)
}

#[tauri::command]
fn read_text(path: String) -> R<String> {
    autoblur::read_text(Path::new(&path))
}

#[tauri::command]
fn recents() -> Vec<String> {
    autoblur::recents().into_iter().filter(|p| Path::new(p).is_file()).collect()
}

#[tauri::command]
fn push_recent(path: String) {
    let mut l = autoblur::recents();
    l.retain(|p| p != &path);
    l.insert(0, path);
    l.truncate(8);
    autoblur::set_recents(&l);
}

#[tauri::command]
fn drop_recent(path: String) {
    let mut l = autoblur::recents();
    l.retain(|p| p != &path);
    autoblur::set_recents(&l);
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            pick_video, pick_open, pick_save, probe, ocr_languages, ocr_cancel, ocr_video,
            export, export_cancel, append_log, reveal, write_text, read_text, recents, push_recent, drop_recent
        ])
        .run(tauri::generate_context!())
        .expect("error while running AutoBlur");
}
