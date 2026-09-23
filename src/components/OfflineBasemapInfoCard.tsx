import { useCallback, useRef, useState } from "react";
import type { OfflineBasemapInfo } from "../types/flight";
import { OfflineBasemapPreview } from "./OfflineBasemapPreview";

interface OfflineBasemapInfoCardProps {
  info: OfflineBasemapInfo | null;
}

interface PopoverPosition {
  top: number;
  left: number;
}

const POPOVER_GAP = 14;
const POPOVER_WIDTH = 280;
const POPOVER_HEIGHT = 180;

/** Shows what's cached on disk from the "Download Map" tab. Hovering renders
 * a small live preview (OfflineBasemapPreview) as a floating bubble next to
 * the card instead of wiring the basemap into the main FlightMap - previewing
 * is read-only and disposable, so there's no separate "load"/"loaded" state
 * to manage.
 *
 * The bubble is positioned with `position: fixed` from a measured
 * getBoundingClientRect() rather than living inline in the sidebar's normal
 * flow - the sidebar scrolls (overflow-y: auto, which makes overflow-x
 * compute to auto too) and would otherwise clip anything wider than itself,
 * and a fixed-position element isn't constrained by an ancestor's overflow
 * clipping the way an absolutely-positioned one inside it would be. */
export function OfflineBasemapInfoCard({ info }: OfflineBasemapInfoCardProps) {
  const cardRef = useRef<HTMLDivElement | null>(null);
  const [popoverPos, setPopoverPos] = useState<PopoverPosition | null>(null);

  const handleEnter = useCallback(() => {
    const rect = cardRef.current?.getBoundingClientRect();
    if (!rect) return;
    setPopoverPos({
      top: Math.min(rect.top, window.innerHeight - POPOVER_HEIGHT - 16),
      left: Math.min(rect.right + POPOVER_GAP, window.innerWidth - POPOVER_WIDTH - 16),
    });
  }, []);

  const handleLeave = useCallback(() => setPopoverPos(null), []);

  if (!info) {
    return (
      <div className="offline-basemap-info offline-basemap-info--empty">
        <p className="offline-basemap-info__hint">No offline map downloaded yet.</p>
      </div>
    );
  }

  return (
    <div className="offline-basemap-info" onMouseEnter={handleEnter} onMouseLeave={handleLeave}>
      <div className="offline-basemap-info__card" ref={cardRef}>
        <span className="offline-basemap-info__bbox">
          {info.bbox.minLon.toFixed(3)}, {info.bbox.minLat.toFixed(3)} to {info.bbox.maxLon.toFixed(3)},{" "}
          {info.bbox.maxLat.toFixed(3)}
        </span>
        <span className="offline-basemap-info__meta">
          Zoom {info.maxZoom} &middot; {(info.sizeBytes / (1024 * 1024)).toFixed(1)}MB &middot;{" "}
          {new Date(info.downloadedAt).toLocaleDateString()}
        </span>
        <span className="offline-basemap-info__action">Hover to preview</span>
      </div>

      {popoverPos && (
        <div className="offline-basemap-info__popover" style={{ top: popoverPos.top, left: popoverPos.left }}>
          <OfflineBasemapPreview />
        </div>
      )}

      {info.tilesFailed > 0 && (
        <p className="error-banner">
          {info.tilesFailed} of {info.tileCount + info.tilesFailed} tiles couldn't be downloaded (network issues) -
          the map may have gaps in this area. Try downloading again, ideally on a more stable connection.
        </p>
      )}
    </div>
  );
}
