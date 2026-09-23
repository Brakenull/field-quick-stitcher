import { useState } from "react";
import type { BasemapBbox, OfflineBasemapInfo, OfflineBasemapProgress, OfflineBasemapStatus } from "../types/flight";

interface OfflineBasemapPanelProps {
  status: OfflineBasemapStatus;
  progress: OfflineBasemapProgress;
  error: string | null;
  download: (bbox: BasemapBbox, maxZoom: number) => Promise<OfflineBasemapInfo | null>;
}

const STAGE_LABELS: Record<OfflineBasemapProgress["stage"], string> = {
  "locating-build": "Locating latest map build",
  "downloading-tiles": "Downloading tiles",
  finalizing: "Finalizing",
};

const DEFAULT_MAX_ZOOM = 12;

function validate(bbox: BasemapBbox, maxZoom: number): string | null {
  if (Object.values(bbox).some((v) => !Number.isFinite(v))) return "All coordinates are required.";
  if (bbox.minLon >= bbox.maxLon) return "Min longitude must be less than max longitude.";
  if (bbox.minLat >= bbox.maxLat) return "Min latitude must be less than max latitude.";
  if (bbox.minLon < -180 || bbox.maxLon > 180) return "Longitude must be within -180 to 180.";
  if (bbox.minLat < -85.06 || bbox.maxLat > 85.06) return "Latitude must be within -85.06 to 85.06.";
  if (!Number.isInteger(maxZoom) || maxZoom < 0 || maxZoom > 15) return "Max zoom must be an integer from 0 to 15.";
  return null;
}

export function OfflineBasemapPanel({ status, progress, error, download }: OfflineBasemapPanelProps) {
  const [minLon, setMinLon] = useState("");
  const [minLat, setMinLat] = useState("");
  const [maxLon, setMaxLon] = useState("");
  const [maxLat, setMaxLat] = useState("");
  const [maxZoom, setMaxZoom] = useState(DEFAULT_MAX_ZOOM);
  const [formError, setFormError] = useState<string | null>(null);

  const downloading = status === "downloading";

  async function handleSubmit(e: React.FormEvent) {
    e.preventDefault();
    const bbox: BasemapBbox = { minLon: Number(minLon), minLat: Number(minLat), maxLon: Number(maxLon), maxLat: Number(maxLat) };
    const validationError = validate(bbox, maxZoom);
    if (validationError) {
      setFormError(validationError);
      return;
    }
    setFormError(null);
    await download(bbox, maxZoom);
  }

  return (
    <div className="offline-basemap-panel">
      <h2 className="offline-basemap-panel__title">Offline map area</h2>
      <p className="offline-basemap-panel__hint">
        Download detailed map tiles for an area before flying somewhere with no signal.
      </p>

      <form onSubmit={handleSubmit} className="stitch-panel">
        <div className="offline-basemap-panel__grid">
          <label className="stitch-field">
            Min longitude
            <input
              type="number"
              step="any"
              placeholder="e.g. 18.6"
              value={minLon}
              onChange={(e) => setMinLon(e.currentTarget.value)}
              disabled={downloading}
              required
            />
          </label>
          <label className="stitch-field">
            Min latitude
            <input
              type="number"
              step="any"
              placeholder="e.g. 49.4"
              value={minLat}
              onChange={(e) => setMinLat(e.currentTarget.value)}
              disabled={downloading}
              required
            />
          </label>
          <label className="stitch-field">
            Max longitude
            <input
              type="number"
              step="any"
              placeholder="e.g. 18.9"
              value={maxLon}
              onChange={(e) => setMaxLon(e.currentTarget.value)}
              disabled={downloading}
              required
            />
          </label>
          <label className="stitch-field">
            Max latitude
            <input
              type="number"
              step="any"
              placeholder="e.g. 49.6"
              value={maxLat}
              onChange={(e) => setMaxLat(e.currentTarget.value)}
              disabled={downloading}
              required
            />
          </label>
        </div>
        <label className="stitch-field">
          Max zoom (0-15, higher = more detail &amp; larger download)
          <input
            type="number"
            min={0}
            max={15}
            step={1}
            value={maxZoom}
            onChange={(e) => setMaxZoom(Number(e.currentTarget.value))}
            disabled={downloading}
          />
        </label>
        <button type="submit" disabled={downloading}>
          {downloading ? "Downloading..." : "Download offline map"}
        </button>

        {downloading && (
          <div className="progress">
            <div className="progress__bar">
              <div className="progress__fill" style={{ width: `${progress.percent}%` }} />
            </div>
            <span>
              {STAGE_LABELS[progress.stage]}
              {progress.tilesTotal > 0 && ` (${progress.tilesDone}/${progress.tilesTotal} tiles)`} - {progress.percent}%
            </span>
          </div>
        )}

        {formError && <p className="error-banner">{formError}</p>}
        {status === "error" && error && <p className="error-banner">{error}</p>}
      </form>
    </div>
  );
}
