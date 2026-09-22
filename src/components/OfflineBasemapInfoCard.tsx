import { useEffect, useState } from "react";
import type { OfflineBasemapInfo } from "../types/flight";

interface OfflineBasemapInfoCardProps {
  info: OfflineBasemapInfo | null;
  /** Resolves true if the basemap was applied to the map, false if there
   * was nothing on disk to load (or applying it failed). */
  onLoad: () => Promise<boolean>;
}

type LoadState = "idle" | "loading" | "loaded" | "error";

/** Shows what's cached on disk from the "Download Map" tab and lets the user
 * decide when to actually wire it into FlightMap - loading is deliberate
 * (click this card) rather than automatic on download or app start, since an
 * old/huge cached basemap silently swapping in would be surprising. */
export function OfflineBasemapInfoCard({ info, onLoad }: OfflineBasemapInfoCardProps) {
  const [loadState, setLoadState] = useState<LoadState>("idle");

  useEffect(() => {
    setLoadState("idle");
  }, [info?.downloadedAt]);

  if (!info) {
    return (
      <div className="offline-basemap-info offline-basemap-info--empty">
        <p className="offline-basemap-info__hint">No offline map downloaded yet.</p>
      </div>
    );
  }

  async function handleClick() {
    setLoadState("loading");
    try {
      const applied = await onLoad();
      setLoadState(applied ? "loaded" : "error");
    } catch {
      setLoadState("error");
    }
  }

  const actionLabel =
    loadState === "loading" ? "Loading on map..." : loadState === "loaded" ? "Loaded on map ✓" : "Click to show on map";

  return (
    <div className="offline-basemap-info">
      <button
        type="button"
        className="offline-basemap-info__card"
        onClick={handleClick}
        disabled={loadState === "loading"}
      >
        <span className="offline-basemap-info__bbox">
          {info.bbox.minLon.toFixed(3)}, {info.bbox.minLat.toFixed(3)} to {info.bbox.maxLon.toFixed(3)},{" "}
          {info.bbox.maxLat.toFixed(3)}
        </span>
        <span className="offline-basemap-info__meta">
          Zoom {info.maxZoom} &middot; {(info.sizeBytes / (1024 * 1024)).toFixed(1)}MB &middot;{" "}
          {new Date(info.downloadedAt).toLocaleDateString()}
        </span>
        <span className={`offline-basemap-info__action offline-basemap-info__action--${loadState}`}>{actionLabel}</span>
      </button>

      {info.tilesFailed > 0 && (
        <p className="error-banner">
          {info.tilesFailed} of {info.tileCount + info.tilesFailed} tiles couldn't be downloaded (network issues) -
          the map may have gaps in this area. Try downloading again, ideally on a more stable connection.
        </p>
      )}
      {loadState === "error" && <p className="error-banner">Couldn't load this map onto the view.</p>}
    </div>
  );
}
