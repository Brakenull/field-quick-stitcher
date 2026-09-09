# Offline basemap tiles

`FlightMap.tsx` loads `basemap.pmtiles` from this folder (via the `pmtiles`
protocol + `@protomaps/basemaps` styling) as an offline base layer under the
flight path/footprints/heatmap. If the file isn't present, the map falls back
to a blank background - the flight geometry still renders fine, there's just
no geographic context underneath it.

`basemap.pmtiles` itself is **gitignored** (it's ~45MB) - regenerate it with:

```powershell
./scripts/fetch-offline-basemap.ps1
```

That pulls a worldwide, low-zoom (0-6) OpenStreetMap-derived extract from
Protomaps' daily build - coastlines, borders, place names. It's deliberately
coarse: enough to orient a flight anywhere in the world, not street-level
detail for a specific survey site (a survey's own photo footprints/mosaic are
the detail that matters there anyway).

## Using a higher-detail regional extract instead

If you'd rather have real street-level detail for a specific region (where
most of your flights happen), extract a bounding box at a higher zoom with the
same [go-pmtiles](https://github.com/protomaps/go-pmtiles) CLI instead of
running the fetch script, e.g.:

```sh
go-pmtiles extract https://build.protomaps.com/<date>.pmtiles basemap.pmtiles \
  --bbox=<min_lon>,<min_lat>,<max_lon>,<max_lat> --maxzoom=14
```

then drop the result in this folder as `basemap.pmtiles`. Attribution:
basemap data is © [OpenStreetMap](https://www.openstreetmap.org/copyright)
contributors, rendered via [Protomaps](https://protomaps.com).
