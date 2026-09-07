import { useRef, useState } from "react";
import { save } from "@tauri-apps/plugin-dialog";
import { Dropzone } from "./components/Dropzone";
import { MetricsBar } from "./components/MetricsBar";
import { AlertPanel } from "./components/AlertPanel";
import { LayerControl, type LayerVisibility } from "./components/LayerControl";
import { FlightMap, type FlightMapHandle } from "./components/FlightMap";
import { useFlightInspector } from "./hooks/useFlightInspector";
import "./App.css";

const DEFAULT_VISIBILITY: LayerVisibility = {
  flightPath: true,
  photoPoints: true,
  footprints: true,
  heatmap: true,
};

function App() {
  const { status, progress, result, error, inspect, exportReport, reset } = useFlightInspector();
  const [visibility, setVisibility] = useState<LayerVisibility>(DEFAULT_VISIBILITY);
  const [exportStatus, setExportStatus] = useState<string | null>(null);
  const mapRef = useRef<FlightMapHandle>(null);

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
          <Dropzone disabled={status === "scanning"} onFolderSelected={inspect} />
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
            <LayerControl visibility={visibility} onChange={setVisibility} />
            <AlertPanel
              gaps={result.gaps}
              blurAlerts={result.blurAlerts}
              onFocusPoint={(lat, lon) => mapRef.current?.flyTo(lat, lon)}
            />
            <div className="export-row">
              <button onClick={() => handleExport("json")}>Export JSON</button>
              <button onClick={() => handleExport("pdf")}>Export PDF</button>
              <button className="ghost" onClick={reset}>
                Scan another card
              </button>
            </div>
            {exportStatus && <p className="export-status">{exportStatus}</p>}
          </>
        )}
      </aside>

      <section className="map-panel">
        <FlightMap ref={mapRef} result={result} visibility={visibility} />
      </section>
    </main>
  );
}

export default App;
