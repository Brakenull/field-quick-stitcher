import { useEffect, useRef, useState } from "react";
import * as maplibregl from "maplibre-gl";
import "maplibre-gl/dist/maplibre-gl.css";
import { loadOfflineBasemapStyle } from "../lib/offlineBasemapStyle";

type PreviewState = "loading" | "ready" | "empty" | "error";

/** A small, standalone MapLibre instance that previews the downloaded
 * offline basemap - deliberately its own map rather than reusing the main
 * FlightMap, so hovering OfflineBasemapInfoCard never disturbs whatever the
 * main map is currently showing. Mounted only while the info card is
 * hovered (see OfflineBasemapInfoCard), so it re-fetches and rebuilds on
 * every hover rather than staying resident. */
export function OfflineBasemapPreview() {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const mapRef = useRef<maplibregl.Map | null>(null);
  const [state, setState] = useState<PreviewState>("loading");

  useEffect(() => {
    if (!containerRef.current) return;
    let cancelled = false;

    loadOfflineBasemapStyle()
      .then((resolved) => {
        if (cancelled || !containerRef.current) return;
        if (!resolved) {
          setState("empty");
          return;
        }
        const map = new maplibregl.Map({
          container: containerRef.current,
          style: resolved.style,
          bounds: [
            [resolved.bbox.minLon, resolved.bbox.minLat],
            [resolved.bbox.maxLon, resolved.bbox.maxLat],
          ],
          fitBoundsOptions: { padding: 8, maxZoom: resolved.maxZoom },
          interactive: false,
          attributionControl: false,
        });
        mapRef.current = map;
        map.on("load", () => {
          if (!cancelled) setState("ready");
        });
      })
      .catch(() => {
        if (!cancelled) setState("error");
      });

    return () => {
      cancelled = true;
      mapRef.current?.remove();
      mapRef.current = null;
    };
  }, []);

  return (
    <div className="offline-basemap-preview">
      <div ref={containerRef} className="offline-basemap-preview__map" />
      {state === "loading" && <div className="offline-basemap-preview__status">Loading preview...</div>}
      {state === "empty" && <div className="offline-basemap-preview__status">No offline map downloaded.</div>}
      {state === "error" && <div className="offline-basemap-preview__status">Couldn't load preview.</div>}
    </div>
  );
}
