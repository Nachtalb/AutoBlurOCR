# Sidecars

ffmpeg and ffprobe are **bundled**, never resolved from `PATH`: a forensic workstation's `PATH`
is not ours to assume, and version drift changes filter behaviour. The bundled version is
recorded in every export log.

Drop static builds here, named with the Rust target triple that Tauri expects:

```
src-tauri/binaries/ffmpeg-x86_64-pc-windows-msvc.exe
src-tauri/binaries/ffprobe-x86_64-pc-windows-msvc.exe
```

From a winget/gyan.dev install:

```powershell
$b = (Get-Command ffmpeg).Source | Split-Path
copy "$b\ffmpeg.exe"  src-tauri\binaries\ffmpeg-x86_64-pc-windows-msvc.exe
copy "$b\ffprobe.exe" src-tauri\binaries\ffprobe-x86_64-pc-windows-msvc.exe
```

`tauri build` copies them next to the installed executable, where `autoblur::tool()` finds them.
For `cargo test` outside a bundle, `tool()` falls back to this directory, or to the
`AUTOBLUR_FFMPEG` / `AUTOBLUR_FFPROBE` environment variables.
