import { useRef, useState } from "react";
import { save } from "@tauri-apps/plugin-dialog";
import { Dropzone } from "./components/Dropzone";
import { MetricsBar } from "./components/MetricsBar";
import { AlertPanel } from "./components/AlertPanel";
import { LayerControl, type LayerVisibility } from "./components/LayerControl";
import { OfflineBasemapPanel } from "./components/OfflineBasemapPanel";
import { OfflineBasemapInfoCard } from "./components/OfflineBasemapInfoCard";
import { Notice } from "./components/Notice";
import { ProgressBar } from "./components/ProgressBar";
import { FlightMap, type FlightMapHandle } from "./components/FlightMap";
import { useFlightInspector } from "./hooks/useFlightInspector";
import { useQuickStitch } from "./hooks/useQuickStitch";
import { useOfflineBasemap } from "./hooks/useOfflineBasemap";
import { checkInternetConnection } from "./lib/network";
import { bboxContains, bboxOfPhotos, fitBboxToAspect } from "./lib/flightBbox";
import type { InspectionResult, NeighborCap, OfflineBasemapInfo, StitchBackend, StitchStage } from "./types/flight";
import "@fontsource-variable/inter";
import "@fontsource/fira-code/400.css";
import "./App.css";

// Matches OfflineBasemapPanel's own manual-entry default - a reasonable
// street-ish level of detail without the user having to pick one themselves
// for this automatic, no-dialog download.
const AUTO_DOWNLOAD_MAX_ZOOM = 12;

const STITCH_STAGE_ORDER: StitchStage[] = [
  "downsampling",
  "detecting-features",
  "matching-pairs",
  "aligning-poses",
  "compositing",
  "exporting",
];

const STITCH_STAGE_LABELS: Record<StitchStage, string> = {
  downsampling: "Downsampling photos",
  "detecting-features": "Detecting features",
  "matching-pairs": "Matching overlapping photos",
  "aligning-poses": "Aligning poses",
  compositing: "Compositing mosaic",
  exporting: "Writing GeoTIFF",
};

const DEFAULT_VISIBILITY: LayerVisibility = {
  flightPath: true,
  photoPoints: true,
  footprints: true,
  heatmap: true,
  mosaic: true,
};

type SidebarTab = "inspect" | "offline-map";

const SIDEBAR_TABS: { id: SidebarTab; label: string }[] = [
  { id: "inspect", label: "Inspect" },
  { id: "offline-map", label: "Download map" },
];

function App() {
  const { status, progress, result, error, inspect, exportReport, reset } = useFlightInspector();
  const stitchState = useQuickStitch();
  const offlineBasemap = useOfflineBasemap();
  const [activeTab, setActiveTab] = useState<SidebarTab>("inspect");
  const [visibility, setVisibility] = useState<LayerVisibility>(DEFAULT_VISIBILITY);
  const [mosaicOpacity, setMosaicOpacity] = useState(0.85);
  const [exportStatus, setExportStatus] = useState<string | null>(null);
  const [stitchBackend, setStitchBackend] = useState<StitchBackend>("onnx");
  const [neighborCap, setNeighborCap] = useState<NeighborCap>("capped");
  const [mapDataNotice, setMapDataNotice] = useState<string | null>(null);
  const [mapDownloadNotice, setMapDownloadNotice] = useState<string | null>(null);
  // The basemap under the main map. Only ever non-null while an inspected
  // folder's result is on screen, for a basemap that covers that flight -
  // outside an inspection the main map stays blank, and cached basemaps are
  // only viewable through the Download map tab's preview.
  const [mainBasemap, setMainBasemap] = useState<OfflineBasemapInfo | null>(null);
  // Bumped by every new scan and every reset: ensureOfflineMapCoverage
  // outlives both (a download can take a while), and must not put a basemap
  // or banner on screen for a scan that's no longer the current one.
  const scanIdRef = useRef(0);
  const mapRef = useRef<FlightMapHandle>(null);

  // Inspect itself is a purely local EXIF scan - it needs neither internet
  // nor a map. This is about the *next* step: heading out to fly (or fly
  // back to a gap), which is a lot easier with an offline map already on
  // disk. So once the scan resolves the survey's actual GPS bbox, opportunistically
  // grab a fresh one for that exact area while a connection is available, or
  // flag it if there's neither a connection nor one already cached.
  async function ensureOfflineMapCoverage(inspection: InspectionResult, scanId: number) {
    const isCurrent = () => scanIdRef.current === scanId;
    const bbox = bboxOfPhotos(inspection.photos);

    // Re-downloading is pointless (and the slowest part of this whole flow)
    // when the basemap already on disk covers this flight at the zoom we'd
    // fetch anyway - e.g. re-scanning the same card, or a second flight over
    // the same area. A partial download (some tiles failed) doesn't count, so
    // it gets another chance to fill in. Checked before the connectivity
    // probe, since this path needs no network at all.
    const cached = offlineBasemap.info;
    if (
      bbox &&
      cached &&
      cached.tilesFailed === 0 &&
      cached.maxZoom >= AUTO_DOWNLOAD_MAX_ZOOM &&
      bboxContains(cached.bbox, bbox)
    ) {
      setMainBasemap(cached);
      setMapDownloadNotice("Using the offline map already downloaded for this area.");
      return;
    }

    const online = await checkInternetConnection();
    if (!isCurrent()) return;
    if (online) {
      if (bbox) {
        // Download a little more than the flight needs: shaped to the map
        // panel, since FlightMap locks the camera inside the basemap and a
        // mismatched shape would crop the flight overview. The coverage check
        // above deliberately uses the unshaped bbox, so resizing the window
        // doesn't invalidate an otherwise-covering cache.
        const viewport = mapRef.current?.getViewportSize();
        const downloadBbox = viewport ? fitBboxToAspect(bbox, viewport.width, viewport.height) : bbox;
        const downloaded = await offlineBasemap.download(downloadBbox, AUTO_DOWNLOAD_MAX_ZOOM);
        // The download itself still finishes (and replaces the cache) after
        // a reset or a newer scan - only showing it is skipped.
        if (downloaded && isCurrent()) {
          setMapDownloadNotice(
            `Offline map downloaded for this flight area (zoom ${downloaded.maxZoom}, ${(downloaded.sizeBytes / (1024 * 1024)).toFixed(1)}MB).`,
          );
          // Shown under the flight path/footprints/heatmap the user is
          // already looking at - it was just downloaded for this exact
          // flight's own bbox.
          setMainBasemap(downloaded);
        }
      }
    } else if (!offlineBasemap.info) {
      setMapDataNotice(
        "You're offline and don't have any offline map data downloaded yet - inspection results will show without map context until you're back online.",
      );
    }
  }

  async function handleInspect(path: string) {
    const scanId = ++scanIdRef.current;
    stitchState.reset();
    setMapDataNotice(null);
    setMapDownloadNotice(null);
    setMainBasemap(null);
    const inspection = await inspect(path);
    if (inspection && scanIdRef.current === scanId) await ensureOfflineMapCoverage(inspection, scanId);
  }

  // Back to "no folder": the main map returns to the blank background too -
  // the basemap belonged to the flight that was just cleared.
  function handleReset() {
    scanIdRef.current++;
    stitchState.reset();
    setMapDataNotice(null);
    setMapDownloadNotice(null);
    setMainBasemap(null);
    reset();
  }

  // Arrow/Home/End move between tabs (WAI-ARIA tablist pattern) - only the
  // active tab sits in the Tab order, so the arrows are the way across.
  function handleTabKeyDown(e: React.KeyboardEvent<HTMLButtonElement>) {
    const index = SIDEBAR_TABS.findIndex((t) => t.id === activeTab);
    let next: number | null = null;
    if (e.key === "ArrowRight") next = (index + 1) % SIDEBAR_TABS.length;
    else if (e.key === "ArrowLeft") next = (index - 1 + SIDEBAR_TABS.length) % SIDEBAR_TABS.length;
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = SIDEBAR_TABS.length - 1;
    if (next == null) return;
    e.preventDefault();
    setActiveTab(SIDEBAR_TABS[next].id);
    document.getElementById(`tab-${SIDEBAR_TABS[next].id}`)?.focus();
  }

  async function handleQuickStitch() {
    const outputPath = await save({
      defaultPath: "quick_stitch.tif",
      filters: [{ name: "GeoTIFF", extensions: ["tif", "tiff"] }],
    });
    if (!outputPath) return;
    await stitchState.stitch(outputPath, stitchBackend, neighborCap);
  }

  async function handleExport(format: "json" | "pdf") {
    const defaultName = format === "json" ? "flight_report.json" : "flight_report.pdf";
    const filters =
      format === "json" ? [{ name: "JSON", extensions: ["json"] }] : [{ name: "PDF", extensions: ["pdf"] }];
    const path = await save({ defaultPath: defaultName, filters });
    if (!path) return;
    try {
      await exportReport(format, path);
      setExportStatus(`Saved to ${path}`);
    } catch (err) {
      setExportStatus(`Export failed: ${err}`);
    }
  }

  return (
    <div className="app">
      <aside className="sidebar">
        <header className="sidebar__header">
          <h1>Field Stitch</h1>
          <p>Flight coverage &amp; blur inspector</p>
        </header>

        <nav className="sidebar-tabs" role="tablist" aria-label="Sidebar sections" data-active={activeTab}>
          {SIDEBAR_TABS.map((tab) => (
            <button
              key={tab.id}
              type="button"
              role="tab"
              id={`tab-${tab.id}`}
              aria-selected={activeTab === tab.id}
              aria-controls="sidebar-panel"
              tabIndex={activeTab === tab.id ? 0 : -1}
              className={`sidebar-tabs__item ${activeTab === tab.id ? "sidebar-tabs__item--active" : ""}`}
              onClick={() => setActiveTab(tab.id)}
              onKeyDown={handleTabKeyDown}
            >
              {tab.label}
            </button>
          ))}
          <span className="sidebar-tabs__indicator" aria-hidden="true" />
        </nav>

        <div className="sidebar__panel" id="sidebar-panel" role="tabpanel" aria-labelledby={`tab-${activeTab}`}>

        {activeTab === "offline-map" && (
          <>
            <OfflineBasemapPanel
              status={offlineBasemap.status}
              progress={offlineBasemap.progress}
              error={offlineBasemap.error}
              download={offlineBasemap.download}
            />
            <OfflineBasemapInfoCard info={offlineBasemap.info} />
          </>
        )}

        {activeTab === "inspect" && (
          <>
            {status !== "done" && (
              <Dropzone disabled={status === "scanning"} onFolderSelected={handleInspect} />
            )}

            {status === "scanning" && <ProgressBar percent={progress} label={`Scanning photos... ${progress}%`} />}

            {status === "error" && error && <Notice variant="error">{error}</Notice>}

            {mapDataNotice && <Notice variant="warning">{mapDataNotice}</Notice>}

            {status === "done" && result && (
              <>
                {offlineBasemap.status === "downloading" && (
                  <ProgressBar
                    percent={offlineBasemap.progress.percent}
                    label={`Downloading offline map for this area... ${offlineBasemap.progress.percent}%`}
                  />
                )}
                {offlineBasemap.status === "error" && offlineBasemap.error && (
                  <Notice variant="warning">Couldn't download an offline map for this area: {offlineBasemap.error}</Notice>
                )}
                {mapDownloadNotice && <Notice variant="success">{mapDownloadNotice}</Notice>}
                <MetricsBar metrics={result.metrics} />
                <LayerControl
                  visibility={visibility}
                  onChange={setVisibility}
                  hasMosaic={stitchState.status === "done"}
                  mosaicOpacity={mosaicOpacity}
                  onMosaicOpacityChange={setMosaicOpacity}
                />
                <AlertPanel
                  gaps={result.gaps}
                  blurAlerts={result.blurAlerts}
                  missingGpsFiles={result.photos.filter((p) => p.lat == null || p.lon == null).map((p) => p.fileName)}
                  onFocusPoint={(lat, lon) => mapRef.current?.flyTo(lat, lon)}
                />

                <section className="stitch-panel" aria-labelledby="stitch-heading">
                  <h2 className="section-title" id="stitch-heading">
                    Quick Stitch
                  </h2>
                  <label className="stitch-field">
                    Backend
                    <select
                      value={stitchBackend}
                      onChange={(e) => setStitchBackend(e.target.value as StitchBackend)}
                      disabled={stitchState.status === "stitching"}
                    >
                      <option value="onnx">ONNX (SuperPoint + LightGlue, GPU)</option>
                      <option value="orb">ORB (CPU fallback)</option>
                    </select>
                  </label>
                  <label className="stitch-field" title="Capped: fast, bounded time, trims some pose-graph edge redundancy. Uncapped: every overlapping pair, more robust, can take hours on a densely-overlapping survey.">
                    Neighbor matching
                    <select
                      value={neighborCap}
                      onChange={(e) => setNeighborCap(e.target.value as NeighborCap)}
                      disabled={stitchState.status === "stitching"}
                    >
                      <option value="capped">Capped (fast, ~10 neighbors/photo)</option>
                      <option value="uncapped">Uncapped (every overlapping pair, slower)</option>
                    </select>
                  </label>
                  <button className="btn btn--primary" onClick={handleQuickStitch} disabled={stitchState.status === "stitching"}>
                    {stitchState.status === "stitching" ? "Stitching..." : "Quick Stitch"}
                  </button>
                  {stitchState.status === "stitching" && (
                    <ProgressBar
                      percent={stitchState.progress.percent}
                      label={`${STITCH_STAGE_LABELS[stitchState.progress.stage]} (${STITCH_STAGE_ORDER.indexOf(stitchState.progress.stage) + 1}/${STITCH_STAGE_ORDER.length}) ${stitchState.progress.percent}%`}
                    />
                  )}
                  {stitchState.status === "error" && stitchState.error && (
                    <Notice variant="error">{stitchState.error}</Notice>
                  )}
                  {stitchState.status === "done" && stitchState.result && (
                    <Notice variant="success">
                      Stitched {stitchState.result.photosUsed} photos via {stitchState.result.backend.toUpperCase()} (
                      {stitchState.result.neighborCap}, {stitchState.result.confidentPairs} confident pairs) in{" "}
                      {(stitchState.result.durationMs / 1000).toFixed(1)}s. Saved to{" "}
                      <code className="path">{stitchState.result.geotiffPath}</code>
                    </Notice>
                  )}
                </section>

                <section className="export-panel" aria-labelledby="export-heading">
                  <h2 className="section-title" id="export-heading">
                    Report
                  </h2>
                  <div className="export-row">
                    <button className="btn btn--secondary" onClick={() => handleExport("json")}>
                      Export JSON
                    </button>
                    <button className="btn btn--secondary" onClick={() => handleExport("pdf")}>
                      Export PDF
                    </button>
                  </div>
                  {exportStatus && (
                    <Notice variant={exportStatus.startsWith("Export failed") ? "error" : "success"}>{exportStatus}</Notice>
                  )}
                  <button className="btn btn--ghost" onClick={handleReset}>
                    Scan another card
                  </button>
                </section>
              </>
            )}
          </>
        )}
        </div>
      </aside>

      <main className="map-panel" aria-label="Flight map">
        <FlightMap
          ref={mapRef}
          result={result}
          visibility={visibility}
          stitchResult={stitchState.result}
          mosaicOpacity={mosaicOpacity}
          basemap={mainBasemap}
        />
      </main>
    </div>
  );
}

export default App;
