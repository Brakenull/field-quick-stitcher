# Field Stitch

A Tauri + React + TypeScript desktop app for QA-inspecting drone photo surveys in the field — no internet connection required to run a scan.

## Features

- **Inspect** — point it at a folder of drone JPEGs and get back a flight path, per-photo GPS/blur status, a red/yellow/green coverage heatmap, and "gap" areas with fly-back coordinates. Export the results as JSON or a PDF report.
- **Quick Stitch** — from the most recent Inspect scan, run a coarse-but-real image-stitching pipeline (feature matching + RANSAC + pose-graph optimization) to produce a georeferenced GeoTIFF orthomosaic, shown as an overlay on the map.
- **Offline basemap** — download a real, detailed vector map for your survey area while you still have signal, so the map underneath your flight data still shows real geography once you're out in the field. See [Offline basemap](#offline-basemap) below.

## Screenshots

<p align="center">
  <a href="https://r2.brakenull.dev/Screenshots/field-stitch/01.png"><img src="https://r2.brakenull.dev/Screenshots/field-stitch/01.png" alt="Field Stitch screenshot 1" width="49%"></a>
  <a href="https://r2.brakenull.dev/Screenshots/field-stitch/02.png"><img src="https://r2.brakenull.dev/Screenshots/field-stitch/02.png" alt="Field Stitch screenshot 2" width="49%"></a>
  <br>
  <a href="https://r2.brakenull.dev/Screenshots/field-stitch/03.png"><img src="https://r2.brakenull.dev/Screenshots/field-stitch/03.png" alt="Field Stitch screenshot 3" width="49%"></a>
  <a href="https://r2.brakenull.dev/Screenshots/field-stitch/04.png"><img src="https://r2.brakenull.dev/Screenshots/field-stitch/04.png" alt="Field Stitch screenshot 4" width="49%"></a>
</p>

## Getting started

Windows 10/11 x64 is the supported platform.

### 1. Install the prerequisites

| Tool | Notes |
| --- | --- |
| [Visual Studio 2022 or later](https://visualstudio.microsoft.com/downloads/) (Community or Build Tools) | Workload **Desktop development with C++**, plus the individual component **C++ Clang Compiler for Windows**. The workload includes vcpkg; no separate vcpkg install is needed. (A standalone [LLVM](https://github.com/llvm/llvm-project/releases) in `C:\Program Files\LLVM` works instead of the Clang component.) |
| [Node.js](https://nodejs.org/) 20 or later (LTS) | For the frontend. |
| [Rust](https://rustup.rs/) | For the backend (Tauri). |
| [Git](https://git-scm.com/download/win) | vcpkg downloads its package definitions with it. |

Also needed: the two ONNX models in `src-tauri/resources/` (not in git, see the README there), and optionally a Protomaps key in `.env` for offline map downloads (see `.env.example`).

### 2. Run the setup script

In PowerShell, from the repository folder:

```powershell
powershell -ExecutionPolicy Bypass -File .\setup.ps1
```

It checks the prerequisites (and lists anything missing; it never installs them), then:

1. installs the native libraries from `vcpkg.json` (OpenCV) with Visual Studio's vcpkg into `vcpkg_env\`. **The first run builds OpenCV from source, which takes 20–40+ minutes;**
2. writes `src-tauri\.cargo\config.toml`, which tells cargo where OpenCV, libclang and the MSVC headers are (machine-specific and gitignored; see `.cargo/config.toml.example`);
3. installs the npm packages;
4. runs `cargo check`, which also generates the OpenCV bindings.

It is safe to run again, for example after pulling changes or updating Visual Studio.

| Option | Effect |
| --- | --- |
| `-LlvmBin <folder>` | Folder with `libclang.dll` and `clang.exe` (default: Visual Studio's Clang, then `C:\Program Files\LLVM\bin`) |
| `-SkipNpm` | Skip `npm ci` |
| `-SkipCheck` | Skip the final `cargo check` |
| `-Clean` | Delete `vcpkg_env\` and `src-tauri\target\` first and rebuild everything |

### 3. Start the app

```sh
npm run tauri dev
```

This starts the full desktop app (Vite dev server + native window + backend). `npm run dev` alone only starts the frontend — the app's `invoke()` calls to the Rust backend won't work without the Tauri window. `npm run tauri build` makes the Windows installer (`src-tauri\target\release\bundle\`).

If you're working on this app rather than just using it, see `CLAUDE.md` for the full architecture, build commands, and native-build troubleshooting.

## Usage

### Try it with demo data

No drone photos of your own? Use the demo survey: 156 DJI photos of the Jablunkov Pass fortifications in Czechia.

1. Download `Jablunkov_Pass_Fortifications_CZ.zip` (~820 MB).
2. Extract it. You'll get a `Jablunkov_Pass_Fortifications_CZ\` folder with `dji_0001.jpg` … `dji_0156.jpg` inside.
3. Start the app and drag the `Jablunkov_Pass_Fortifications_CZ` folder onto the sidebar, or pick it with "Browse folder…". Then follow [Inspect](#inspect) and [Quick Stitch](#quick-stitch) below.

### Inspect

Drag a folder of drone photos onto the sidebar (or use "Browse folder…"). The scan runs entirely offline — it only reads EXIF/XMP metadata already embedded in your photos, no network access needed. Once it finishes, you'll see the flight path, a coverage heatmap, and any blur or gap alerts on the map; click an alert to fly to it.

### Quick Stitch

Once a scan has finished, use the Quick Stitch panel to generate a georeferenced mosaic GeoTIFF from the same photos. Pick a backend (ONNX is the default and much more reliable; ORB is a CPU-only fallback) and a neighbor-matching mode (Capped is faster; Uncapped matches every overlapping photo pair, which is slower but more robust on densely-overlapping surveys).

### Offline basemap

The main map shows your flight path and footprints regardless, but a real geographic basemap underneath it needs to be downloaded ahead of time — there's no live internet tile service baked into the app.

- **Download Map tab**: type in a bounding box and a max zoom level, then download. This is the way to prepare a map for a *specific* site before you go out — do it while you still have a connection.
- **Automatic, tied to Inspect**: dropping a photo folder into Inspect also checks your connection and, if you're online, automatically downloads a basemap for that flight's own GPS area (no bounding box to fill in) and loads it under the results you're looking at. If you're offline and haven't downloaded anything yet, you'll see a warning instead — the scan itself still works fine either way.
- **Only one basemap is stored at a time.** Downloading a new area — manually or via the automatic Inspect trigger — replaces whatever was downloaded before. If you work multiple survey sites, you'll need to re-download when you switch back to an earlier one.
- **Previewing what's downloaded**: hover the cached-basemap card in the Download Map tab to see a small preview of the currently-downloaded area. This preview is independent of the main map — the main map only shows a basemap that was just downloaded for the flight you're currently inspecting, not whatever happens to be cached from a previous session.

## Project structure

See `CLAUDE.md` for the full architecture writeup (frontend/backend layout, Tauri IPC surface, native build environment) and `.claude/docs/` for detailed technical specs of each feature, if present.
