# Downloads a small (~45MB), worldwide, low-zoom (0-6) OpenStreetMap-derived
# vector basemap into public/offline_tiles/basemap.pmtiles, for FlightMap.tsx's
# offline basemap layer (see overview.md / MVP.md's "offline vector/raster
# tiles" requirement).
#
# This is deliberately coarse - coastlines, borders, place names - just enough
# geographic context to orient a flight anywhere in the world, at a size small
# enough to bundle. It is NOT street-level detail for a specific survey site;
# for that, extract your own regional/local package with the same `pmtiles`
# CLI tool (see public/offline_tiles/README.md).
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
for ($daysAgo = 0; $daysAgo -lt 10; $daysAgo++) {
    $date = (Get-Date).AddDays(-$daysAgo).ToString("yyyyMMdd")
    $candidateUrl = "https://build.protomaps.com/$date.pmtiles"
    try {
        $resp = Invoke-WebRequest -Uri $candidateUrl -Method Head -TimeoutSec 15
        if ($resp.StatusCode -eq 200) {
            $buildUrl = $candidateUrl
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

$outDir = Join-Path $PSScriptRoot "..\public\offline_tiles"
New-Item -ItemType Directory -Force -Path $outDir | Out-Null
$outFile = Join-Path $outDir "basemap.pmtiles"

& $pmtiles extract $buildUrl $outFile --maxzoom=6
Write-Host "Wrote $outFile"
