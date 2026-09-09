use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    tauri_build::build();
    copy_vcpkg_runtime_dlls();
}

/// `cargo`'s `[env]` config can't override `PATH` (see `.cargo/config.toml`), so
/// opencv/gdal's DLLs (and their transitive deps) from vcpkg's `installed/x64-windows/bin`
/// won't otherwise be found when `cargo run`/`tauri dev` launches the built exe.
/// Windows always searches the exe's own directory for DLLs regardless of PATH,
/// so copy them there instead. Only runs on Windows when VCPKG_ROOT is set; a
/// no-op everywhere else (e.g. CI without the native toolchain configured).
fn copy_vcpkg_runtime_dlls() {
    println!("cargo:rerun-if-env-changed=VCPKG_ROOT");
    if env::var_os("CARGO_CFG_WINDOWS").is_none() {
        return;
    }
    let Some(vcpkg_root) = env::var_os("VCPKG_ROOT") else {
        return;
    };
    let bin_dir = PathBuf::from(vcpkg_root).join("installed/x64-windows/bin");
    if !bin_dir.is_dir() {
        return;
    }

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR set by cargo"));
    // OUT_DIR is target/<profile>/build/<pkg>-<hash>/out; the exe lives in target/<profile>.
    let Some(target_profile_dir) = out_dir.ancestors().nth(3) else {
        return;
    };

    let marker = target_profile_dir.join(".vcpkg-dlls-copied");
    if marker.exists() {
        return;
    }

    if let Ok(entries) = fs::read_dir(&bin_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("dll") {
                continue;
            }
            let dest = target_profile_dir.join(path.file_name().unwrap());
            let _ = fs::copy(&path, &dest);
        }
    }
    let _ = fs::write(&marker, b"");
}
