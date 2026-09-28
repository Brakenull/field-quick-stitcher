use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    tauri_build::build();
    copy_vcpkg_runtime_dlls();
    embed_protomaps_key();
}

/// Bakes `PROTOMAP_KEY` into the binary (read back via `option_env!` in
/// `commands/offline_basemap.rs`) so a built app can download basemaps with no
/// `.env` next to it. Taken from a build-time env var if set, else the
/// repo-root `.env` (gitignored). Missing key = no embed; the download
/// command then errors with a clear message instead of failing to compile.
fn embed_protomaps_key() {
    println!("cargo:rerun-if-env-changed=PROTOMAP_KEY");
    // Watched unconditionally, even while the file doesn't exist: watching it
    // only when present meant a build made before `.env` was created cached a
    // key-less result that cargo never re-ran once `.env` appeared (it had
    // nothing to watch). A missing watched path makes cargo re-run this
    // script on every build instead - fine, since it's cheap, and it keeps
    // doing so only until `.env` is created.
    println!("cargo:rerun-if-changed=../.env");
    let env_path = PathBuf::from("../.env");

    let key = env::var("PROTOMAP_KEY").ok().filter(|k| !k.trim().is_empty()).or_else(|| {
        let contents = fs::read_to_string(&env_path).ok()?;
        contents.lines().find_map(|line| {
            let (name, value) = line.split_once('=')?;
            (name.trim() == "PROTOMAP_KEY").then(|| value.trim().trim_matches('"').trim_matches('\'').to_string())
        })
    });
    match key.filter(|k| !k.is_empty()) {
        Some(key) => println!("cargo:rustc-env=PROTOMAP_KEY={key}"),
        None => println!(
            "cargo:warning=No PROTOMAP_KEY found (repo-root .env or env var) - offline map downloads will fail until one is set. See .env.example."
        ),
    }
}

/// `cargo`'s `[env]` config can't override `PATH` (see `.cargo/config.toml`), so
/// opencv's DLLs (and their transitive deps) from vcpkg's `installed/x64-windows/bin`
/// (`VCPKG_ROOT` is `<repo>/vcpkg_env`, set up by `setup.ps1`)
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
