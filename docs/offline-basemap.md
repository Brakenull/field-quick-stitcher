# Offline basemap tiles

`FlightMap.tsx` loads a `basemap.pmtiles` file (via the `pmtiles` protocol +
`@protomaps/basemaps` styling) as an offline base layer under the flight
path/footprints/heatmap. If none is available, the map falls back to a blank
background - the flight geometry still renders fine, there's just no
geographic context underneath it.

## Primary mechanism: the in-app downloader

The **"Offline map area" panel** in the sidebar (`OfflineBasemapPanel.tsx`,
backed by the `download_offline_basemap`/`get_offline_basemap_info` Tauri
commands in `src-tauri/src/commands/offline_basemap.rs`) is the production
way to get a basemap: type in a bounding box and max zoom before heading out
to fly (while still online), and it downloads directly - no external CLI, no
bundled binary. It reimplements what `go-pmtiles extract` does, natively in
Rust via the `pmtiles` crate (`AsyncPmTilesReader` for HTTP range-request
tile fetching against Protomaps' public daily planet build, `PmTilesWriter`
for creating the local archive).

**REPLACE semantics**: exactly one basemap is active at a time, at a fixed
location under Tauri's app-data dir
(`%APPDATA%\com.brake.main\offline_basemap\basemap.pmtiles` on Windows,
alongside a `basemap.meta.json` sidecar). Downloading a new area overwrites
it - there's no multi-region storage in v1.

## Dev-only fast bootstrap: the script

```powershell
./scripts/fetch-offline-basemap.ps1
```

Requires the [go-pmtiles](https://github.com/protomaps/go-pmtiles) CLI. Pulls
a worldwide, low-zoom (0-6) extract - coastlines, borders, place names, just
enough to orient a flight anywhere in the world - and writes it to the same
app-data location the in-app downloader uses (plus a matching
`basemap.meta.json`), so the app can't tell the difference between the two.
This is a convenience for local dev (no need to launch the full Tauri app or
wait on a Rust compile just to get *some* basemap on screen); it is not a
second, parallel loading path in the app itself.

## Using a higher-detail regional extract manually

If you want street-level detail for a specific region without going through
the in-app form (e.g. scripting a batch of known survey sites), the same
`go-pmtiles` CLI works directly:

```sh
go-pmtiles extract https://build.protomaps.com/<date>.pmtiles basemap.pmtiles \
  --bbox=<min_lon>,<min_lat>,<max_lon>,<max_lat> --maxzoom=14
```

then place the result (plus a hand-written `basemap.meta.json` matching
`OfflineBasemapInfo`'s shape) at the app-data path above.

Attribution: basemap data is © [OpenStreetMap](https://www.openstreetmap.org/copyright)
contributors, rendered via [Protomaps](https://protomaps.com).
