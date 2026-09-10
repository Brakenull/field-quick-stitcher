import { useRef, useState } from "react";
import { save } from "@tauri-apps/plugin-dialog";
import { Dropzone } from "./components/Dropzone";
import { MetricsBar } from "./components/MetricsBar";
import { AlertPanel } from "./components/AlertPanel";
import { LayerControl, type LayerVisibility } from "./components/LayerControl";
import { FlightMap, type FlightMapHandle } from "./components/FlightMap";
import { useFlightInspector } from "./hooks/useFlightInspector";
import { useQuickStitch } from "./hooks/useQuickStitch";
import type { StitchStage } from "./types/flight";
import "./App.css";

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

function App() {
  const { status, progress, result, error, inspect, exportReport, reset } = useFlightInspector();
  const stitchState = useQuickStitch();
  const [visibility, setVisibility] = useState<LayerVisibility>(DEFAULT_VISIBILITY);
  const [mosaicOpacity, setMosaicOpacity] = useState(0.85);
  const [exportStatus, setExportStatus] = useState<string | null>(null);
  const mapRef = useRef<FlightMapHandle>(null);

  async function handleInspect(path: string) {
    stitchState.reset();
    await inspect(path);
  }

  function handleReset() {
    stitchState.reset();
    reset();
  }

  async function handleQuickStitch() {
    const outputPath = await save({
      defaultPath: "quick_stitch.tif",
      filters: [{ name: "GeoTIFF", extensions: ["tif", "tiff"] }],
    });
    if (!outputPath) return;
    await stitchState.stitch(outputPath);
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
    <main className="app">
      <aside className="sidebar">
        <header className="sidebar__header">
          <h1>Field Stitch</h1>
          <p>Flight coverage &amp; blur inspector</p>
        </header>

        {status !== "done" && (
          <Dropzone disabled={status === "scanning"} onFolderSelected={handleInspect} />
        )}

        {status === "scanning" && (
          <div className="progress">
            <div className="progress__bar">
              <div className="progress__fill" style={{ width: `${progress}%` }} />
            </div>
            <span>{progress}% scanned</span>
          </div>
        )}

        {status === "error" && error && <p className="error-banner">{error}</p>}

        {status === "done" && result && (
          <>
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
              onFocusPoint={(lat, lon) => mapRef.current?.flyTo(lat, lon)}
            />

            <div className="stitch-panel">
              <button onClick={handleQuickStitch} disabled={stitchState.status === "stitching"}>
                {stitchState.status === "stitching" ? "Stitching..." : "Quick Stitch"}
              </button>
              {stitchState.status === "stitching" && (
                <div className="progress">
                  <div className="progress__bar">
                    <div className="progress__fill" style={{ width: `${stitchState.progress.percent}%` }} />
                  </div>
                  <span>
                    {STITCH_STAGE_LABELS[stitchState.progress.stage]} ({STITCH_STAGE_ORDER.indexOf(stitchState.progress.stage) + 1}/
                    {STITCH_STAGE_ORDER.length}) - {stitchState.progress.percent}%
                  </span>
                </div>
              )}
              {stitchState.status === "error" && stitchState.error && (
                <p className="error-banner">{stitchState.error}</p>
              )}
              {stitchState.status === "done" && stitchState.result && (
                <p className="export-status">
                  Stitched {stitchState.result.photosUsed} photos ({stitchState.result.confidentPairs} confident
                  pairs) in {(stitchState.result.durationMs / 1000).toFixed(1)}s -&gt; {stitchState.result.geotiffPath}
                </p>
              )}
            </div>

            <div className="export-row">
              <button onClick={() => handleExport("json")}>Export JSON</button>
              <button onClick={() => handleExport("pdf")}>Export PDF</button>
              <button className="ghost" onClick={handleReset}>
                Scan another card
              </button>
            </div>
            {exportStatus && <p className="export-status">{exportStatus}</p>}
          </>
        )}
      </aside>

      <section className="map-panel">
        <FlightMap
          ref={mapRef}
          result={result}
          visibility={visibility}
          stitchResult={stitchState.result}
          mosaicOpacity={mosaicOpacity}
        />
      </section>
    </main>
  );
}

export default App;
