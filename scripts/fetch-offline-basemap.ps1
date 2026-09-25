# Downloads a small (~45MB), worldwide, low-zoom (0-6) OpenStreetMap-derived
# vector basemap - a fast, no-Rust-compile-needed way to get *some* basemap
# on disk during dev (e.g. to try OfflineBasemapPreview without flying/
# inspecting anything first). Coastlines, borders, land use only - NOT street-
# level detail for a survey site.
#
# The app itself no longer works this way: download_offline_basemap
# (src-tauri/src/commands/offline_basemap.rs) fetches per-tile from the hosted
# Protomaps API (https://protomaps.com/api, key in the repo-root .env) for
# just the flight's bbox, at zoom 12 on auto-download. This script
# deliberately stays on a one-off `go-pmtiles extract` from Protomaps' public
# daily planet build instead:
#   - a worldwide z0-6 extract is ~5,500 tiles - a bulk pull the free,
#     non-commercial API isn't meant for, while the daily builds are exactly
#     what Protomaps points to for self-served extracts;
#   - writing a .pmtiles archive from individual API tiles needs a writer the
#     way the Rust side has one; go-pmtiles extract does it in one step.
# No PROTOMAP_KEY needed here. Expect it to be slow at times: that host's
# throughput swings a lot (it's why the app moved to the API).
#
# Writes to the SAME location the app itself reads from (Tauri's app-data
# dir, not public/ - that only worked pre-packaging, since public/ is baked
# read-only into a built app; see git history for the migration), plus a
# `basemap.meta.json` sidecar so get_offline_basemap_info describes it like
# an in-app download. Same single-slot REPLACE semantics: this overwrites any
# in-app download and vice versa - and since its maxZoom (6) is below the
# app's auto-download zoom (12), the next Inspect replaces it with a proper
# per-flight basemap anyway.
#
# Requires the go-pmtiles CLI: `go install github.com/protomaps/go-pmtiles@latest`
# (needs a Go toolchain). The extract uses HTTP range requests against the
# 100+GB planet build, so it only ever downloads ~45MB of it.

$ErrorActionPreference = "Stop"

$pmtiles = Get-Command go-pmtiles -ErrorAction SilentlyContinue
if (-not $pmtiles) {
    $candidate = Join-Path $env:USERPROFILE "go\bin\go-pmtiles.exe"
    if (Test-Path $candidate) {
        $pmtiles = $candidate
    } else {
        throw "go-pmtiles CLI not found. Install it with: go install github.com/protomaps/go-pmtiles@latest"
    }
} else {
    $pmtiles = $pmtiles.Source
}

# Daily builds are published as https://build.protomaps.com/<YYYYMMDD>.pmtiles
# with no stable "latest" alias, so probe backwards for the most recent one
# that actually exists - yesterday first, since today's usually isn't
# published yet.
$buildUrl = $null
$buildDate = $null
foreach ($daysAgo in @(1, 0, 2, 3, 4, 5, 6, 7, 8, 9)) {
    $date = (Get-Date).AddDays(-$daysAgo).ToString("yyyyMMdd")
    $candidateUrl = "https://build.protomaps.com/$date.pmtiles"
    try {
        $resp = Invoke-WebRequest -Uri $candidateUrl -Method Head -TimeoutSec 15
        if ($resp.StatusCode -eq 200) {
            $buildUrl = $candidateUrl
            $buildDate = $date
            break
        }
    } catch {
        continue
    }
}
if (-not $buildUrl) {
    throw "Could not find a recent Protomaps daily build under build.protomaps.com in the last 10 days."
}
Write-Host "Using build: $buildUrl"

# Same identifier as tauri.conf.json ("identifier") + the "offline_basemap"
# subdir the Rust side creates via app_data_dir() - keep these in sync if
# either changes.
$outDir = Join-Path $env:APPDATA "com.brake.main\offline_basemap"
New-Item -ItemType Directory -Force -Path $outDir | Out-Null
$outFile = Join-Path $outDir "basemap.pmtiles"
$metaFile = Join-Path $outDir "basemap.meta.json"
$maxZoom = 6

$extractOutput = & $pmtiles extract $buildUrl $outFile --maxzoom=$maxZoom 2>&1 | Tee-Object -Variable extractLines
$extractLines | ForEach-Object { Write-Host $_ }

$tileCount = 0
foreach ($line in $extractLines) {
    if ($line -match "result tile entries (\d+)") {
        $tileCount = [int]$Matches[1]
        break
    }
}

$meta = [ordered]@{
    path         = $outFile
    bbox         = [ordered]@{ minLon = -180.0; minLat = -85.0511; maxLon = 180.0; maxLat = 85.0511 }
    maxZoom      = $maxZoom
    tileCount    = $tileCount
    tilesFailed  = 0
    sizeBytes    = (Get-Item $outFile).Length
    sourceBuild  = $buildDate
    downloadedAt = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssK")
}
$meta | ConvertTo-Json | Set-Content -Path $metaFile -Encoding utf8

Write-Host "Wrote $outFile"
Write-Host "Wrote $metaFile"
