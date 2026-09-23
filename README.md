# Field Stitch

A Tauri + React + TypeScript desktop app for QA-inspecting drone photo surveys in the field — no internet connection required to run a scan.

## Features

- **Inspect** — point it at a folder of drone JPEGs and get back a flight path, per-photo GPS/blur status, a red/yellow/green coverage heatmap, and "gap" areas with fly-back coordinates. Export the results as JSON or a PDF report.
- **Quick Stitch** — from the most recent Inspect scan, run a coarse-but-real image-stitching pipeline (feature matching + RANSAC + pose-graph optimization) to produce a georeferenced GeoTIFF orthomosaic, shown as an overlay on the map.
- **Offline basemap** — download a real, detailed vector map for your survey area while you still have signal, so the map underneath your flight data still shows real geography once you're out in the field. See [Offline basemap](#offline-basemap) below.

## Getting started

```sh
npm install
npm run tauri dev
```

This starts the full desktop app (Vite dev server + native window + backend). `npm run dev` alone only starts the frontend — the app's `invoke()` calls to the Rust backend won't work without the Tauri window.

The Rust backend links against a native OpenCV build and needs machine-local paths configured in `src-tauri/.cargo/config.toml` (see `.cargo/config.toml.example`) before it will compile. If you're working on this app rather than just using it, see `CLAUDE.md` for the full architecture, build commands, and native-build troubleshooting.

## Usage

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
