# Downloads a small (~45MB), worldwide, low-zoom (0-6) OpenStreetMap-derived
# vector basemap for FlightMap.tsx's offline basemap layer - a fast, no-Rust-
# compile-needed way to bootstrap a coarse fallback during dev. For anything
# more detailed (street-level, a specific survey region), use the in-app
# "Offline map area" panel instead (OfflineBasemapPanel.tsx /
# download_offline_basemap) - same underlying mechanism, just driven from a
# bounding-box form instead of this script's fixed worldwide extract.
#
# Writes to the SAME location the app itself reads from (Tauri's app-data
# dir, not public/ - that only worked pre-packaging, since public/ is baked
# read-only into a built app; see git history for the migration), so the app
# picks this up identically to an in-app download, and a `basemap.meta.json`
# sidecar is written alongside it so get_offline_basemap_info can describe it
# in the UI even though a different tool (this script) produced it.
#
# This is deliberately coarse - coastlines, borders, place names - just enough
# geographic context to orient a flight anywhere in the world. It is NOT
# street-level detail for a specific survey site; use the in-app downloader
# with a tight bounding box for that instead.
#
# Requires the go-pmtiles CLI: `go install github.com/protomaps/go-pmtiles@latest`
# (needs a Go toolchain). The extract uses HTTP range requests against
# Protomaps' daily full-planet build, so it only ever downloads ~45MB despite
# the source file being 100+ GB.

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
# with no stable "latest" alias, so probe backwards from today for the most
# recent one that actually exists.
$buildUrl = $null
$buildDate = $null
for ($daysAgo = 0; $daysAgo -lt 10; $daysAgo++) {
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
