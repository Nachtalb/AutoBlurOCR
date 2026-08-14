fn main() {
    // lib.rs uses this to find src-tauri/binaries/<name>-<triple> in dev and under `cargo test`.
    println!("cargo:rustc-env=TARGET_TRIPLE={}", std::env::var("TARGET").unwrap());
    if std::env::var("CARGO_FEATURE_APP").is_ok() {
        tauri_build::build();
    }
}
