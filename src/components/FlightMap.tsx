import { forwardRef, useEffect, useImperativeHandle, useRef, useState } from "react";
import * as maplibregl from "maplibre-gl";
import type { StyleSpecification } from "maplibre-gl";
import { convertFileSrc } from "@tauri-apps/api/core";
import "maplibre-gl/dist/maplibre-gl.css";
import { loadOfflineBasemapStyle } from "../lib/offlineBasemapStyle";
import type { BasemapBbox, GeoFeature, InspectionResult, OfflineBasemapInfo, StitchResult } from "../types/flight";
import { Notice } from "./Notice";
import type { LayerVisibility } from "./LayerControl";

export interface FlightMapHandle {
  flyTo: (lat: number, lon: number) => void;
  /** The map panel's current size in CSS px (null before layout). App uses
   * it to shape auto-downloaded basemaps to the panel's aspect ratio - see
   * fitBboxToAspect in lib/flightBbox.ts. */
  getViewportSize: () => { width: number; height: number } | null;
}

interface FlightMapProps {
  result: InspectionResult | null;
  visibility: LayerVisibility;
  stitchResult: StitchResult | null;
  mosaicOpacity: number;
  /** The offline basemap to draw under the flight geometry, or null for the
   * blank background. App only sets this while an inspected folder's result
   * is on screen, for a basemap known to cover that flight (see
   * ensureOfflineMapCoverage) - outside an inspection the main map is always
   * blank, and cached basemaps are only viewable via OfflineBasemapPreview.
   * Swapping it doesn't move the camera: the result-driven fitBounds effect
   * already put it over the flight area. */
  basemap: OfflineBasemapInfo | null;
}

const MOSAIC_SOURCE_ID = "mosaic";
const MOSAIC_LAYER_ID = "mosaic-layer";
/** How long the "edge of the offline map" toast stays up after the last hit. */
const EDGE_TOAST_MS = 2500;

// Starting style before any offline basemap is loaded (or if none is ever
// downloaded) - the flight geometry (path, footprints, heatmap) still
// renders fine on top of this with zero network requests.
const BLANK_STYLE: StyleSpecification = {
  version: 8,
  sources: {},
  layers: [{ id: "background", type: "background", paint: { "background-color": "#F9FAFB" } }],
};

const EMPTY_FC = { type: "FeatureCollection" as const, features: [] as GeoFeature[] };

function toFeatureCollection(features: GeoFeature[]) {
  return { type: "FeatureCollection" as const, features };
}

function boundsOf(result: InspectionResult): maplibregl.LngLatBoundsLike | null {
  let minLon = Infinity;
  let minLat = Infinity;
  let maxLon = -Infinity;
  let maxLat = -Infinity;
  let touched = false;
  for (const photo of result.photos) {
    if (photo.lat == null || photo.lon == null) continue; // GPS dropout - not placeable
    touched = true;
    minLon = Math.min(minLon, photo.lon);
    maxLon = Math.max(maxLon, photo.lon);
    minLat = Math.min(minLat, photo.lat);
    maxLat = Math.max(maxLat, photo.lat);
  }
  if (!touched) return null;
  return [
    [minLon, minLat],
    [maxLon, maxLat],
  ];
}

/** Adds the flight/footprints/heatmap sources+layers this app always draws
 * on top of whatever base style is active - shared between the initial map
 * load and a live style swap (`setStyle` wipes anything added outside the
 * style object, so this has to be re-run after one). */
function addBaseLayers(map: maplibregl.Map) {
  map.addSource("footprints", { type: "geojson", data: EMPTY_FC });
  map.addLayer({
    id: "footprints-fill",
    type: "fill",
    source: "footprints",
    paint: { "fill-color": "#4F46E5", "fill-opacity": 0.08 },
  });
  map.addLayer({
    id: "footprints-outline",
    type: "line",
    source: "footprints",
    paint: { "line-color": "#4F46E5", "line-width": 0.5, "line-opacity": 0.4 },
  });

  map.addSource("heatmap", { type: "geojson", data: EMPTY_FC });
  map.addLayer({
    id: "heatmap-fill",
    type: "fill",
    source: "heatmap",
    paint: {
      "fill-color": ["match", ["get", "level"], "red", "#EF4444", "yellow", "#F59E0B", "green", "#10B981", "#9CA3AF"],
      "fill-opacity": 0.45,
    },
  });

  map.addSource("flight", { type: "geojson", data: EMPTY_FC });
  map.addLayer({
    id: "flight-path-line",
    type: "line",
    source: "flight",
    filter: ["==", ["geometry-type"], "LineString"],
    paint: { "line-color": "#1F2937", "line-width": 2 },
  });
  map.addLayer({
    id: "photo-points",
    type: "circle",
    source: "flight",
    filter: ["==", ["geometry-type"], "Point"],
    paint: {
      "circle-radius": 4,
      "circle-color": ["case", ["get", "isBlurry"], "#EF4444", "#4F46E5"],
      "circle-stroke-color": "#ffffff",
      "circle-stroke-width": 1,
    },
  });
}

/** Adds (or re-adds) the Quick Stitch mosaic image source+layer - shared
 * between the stitchResult-driven effect and a live style swap, since
 * `setStyle` wipes this layer too even though `stitchResult` itself hasn't changed. */
function applyMosaic(map: maplibregl.Map, stitchResult: StitchResult | null, mosaicOpacity: number, visibility: LayerVisibility) {
  if (map.getLayer(MOSAIC_LAYER_ID)) map.removeLayer(MOSAIC_LAYER_ID);
  if (map.getSource(MOSAIC_SOURCE_ID)) map.removeSource(MOSAIC_SOURCE_ID);

  if (!stitchResult) return;
  map.addSource(MOSAIC_SOURCE_ID, {
    type: "image",
    url: convertFileSrc(stitchResult.previewPath),
    coordinates: stitchResult.previewCorners,
  });
  map.addLayer(
    {
      id: MOSAIC_LAYER_ID,
      type: "raster",
      source: MOSAIC_SOURCE_ID,
      paint: { "raster-opacity": mosaicOpacity },
      layout: { visibility: visibility.mosaic ? "visible" : "none" },
    },
    map.getLayer("footprints-fill") ? "footprints-fill" : undefined,
  );
}

export const FlightMap = forwardRef<FlightMapHandle, FlightMapProps>(function FlightMap(
  { result, visibility, stitchResult, mosaicOpacity, basemap },
  ref,
) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const mapRef = useRef<maplibregl.Map | null>(null);
  const loadedRef = useRef(false);
  // State twin of loadedRef, so the basemap effect re-runs once the map is
  // ready if a basemap was requested before then.
  const [mapLoaded, setMapLoaded] = useState(false);
  const attributionRef = useRef<maplibregl.AttributionControl | null>(null);
  // Latest props for the async style swap below, which re-adds the flight
  // layers after `setStyle` wipes them - by then the effect's own closure
  // could be several renders stale.
  const latestRef = useRef({ result, visibility, stitchResult, mosaicOpacity });
  latestRef.current = { result, visibility, stitchResult, mosaicOpacity };
  // Which basemap the map currently shows ("" = BLANK_STYLE), so a prop
  // change resolving to the same one doesn't trigger a pointless swap.
  const shownBasemapKeyRef = useRef("");
  // The offline basemap's extent while one is shown - the camera is locked
  // inside it (see applyBasemapBounds), and the edge toast only fires then.
  const boundsLockRef = useRef<BasemapBbox | null>(null);
  const [edgeToastVisible, setEdgeToastVisible] = useState(false);
  const edgeToastTimerRef = useRef<number | undefined>(undefined);

  function notifyEdgeReached() {
    setEdgeToastVisible(true);
    window.clearTimeout(edgeToastTimerRef.current);
    edgeToastTimerRef.current = window.setTimeout(() => setEdgeToastVisible(false), EDGE_TOAST_MS);
  }
  // Read by map event handlers registered once at init.
  const notifyEdgeReachedRef = useRef(notifyEdgeReached);
  notifyEdgeReachedRef.current = notifyEdgeReached;

  useImperativeHandle(ref, () => ({
    flyTo(lat, lon) {
      mapRef.current?.flyTo({ center: [lon, lat], zoom: 19, duration: 600 });
    },
    getViewportSize() {
      const container = mapRef.current?.getContainer();
      return container && container.clientWidth > 0 && container.clientHeight > 0
        ? { width: container.clientWidth, height: container.clientHeight }
        : null;
    },
  }));

  useEffect(() => {
    if (!containerRef.current) return;

    const map = new maplibregl.Map({
      container: containerRef.current,
      style: BLANK_STYLE,
      center: [0, 0],
      zoom: 2,
      attributionControl: false,
    });
    map.addControl(new maplibregl.NavigationControl({ showCompass: false }), "top-right");

    map.on("load", () => {
      addBaseLayers(map);
      loadedRef.current = true;
      applyData(map, result);
      applyVisibility(map, visibility);
      setMapLoaded(true);
    });

    // Edge detection while the camera is locked to the basemap. The lock
    // itself silently absorbs the gesture, so without these the user just
    // sees the map stop responding.
    const atMinZoom = () => map.getZoom() <= map.getMinZoom() + 0.01;
    map.on("wheel", (e) => {
      if (boundsLockRef.current && e.originalEvent.deltaY > 0 && atMinZoom()) notifyEdgeReachedRef.current();
    });
    // Dragging into an edge: the pointer keeps moving but the map (clamped
    // by maxBounds) moves much less than it along that axis.
    let lastDragCenter: maplibregl.LngLat | null = null;
    map.on("dragstart", () => {
      lastDragCenter = map.getCenter();
    });
    map.on("drag", (e) => {
      const center = map.getCenter();
      const prev = lastDragCenter;
      lastDragCenter = center;
      const pointer = e.originalEvent as MouseEvent | undefined;
      if (!boundsLockRef.current || !prev || !pointer || typeof pointer.movementX !== "number") return;
      const moved = map.project(prev);
      const now = map.project(center);
      const blocked = (pointerDelta: number, mapDelta: number) =>
        Math.abs(pointerDelta) > 2 && Math.abs(mapDelta) < Math.abs(pointerDelta) / 2;
      if (blocked(pointer.movementX, moved.x - now.x) || blocked(pointer.movementY, moved.y - now.y)) {
        notifyEdgeReachedRef.current();
      }
    });
    const container = containerRef.current;
    const onKeyDown = (e: KeyboardEvent) => {
      if (boundsLockRef.current && (e.key === "-" || e.key === "_") && atMinZoom()) notifyEdgeReachedRef.current();
    };
    container.addEventListener("keydown", onKeyDown);
    // The lowest zoom that still fits inside the basemap depends on the
    // panel's size, so it has to follow window resizes.
    map.on("resize", () => {
      if (boundsLockRef.current) map.setMinZoom(coverZoom(map, boundsLockRef.current));
    });

    mapRef.current = map;

    return () => {
      container.removeEventListener("keydown", onKeyDown);
      window.clearTimeout(edgeToastTimerRef.current);
      map.remove();
      mapRef.current = null;
      loadedRef.current = false;
      attributionRef.current = null;
      shownBasemapKeyRef.current = "";
      boundsLockRef.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Swaps the base style between BLANK_STYLE and the offline basemap.
  // `cancelled` covers a quick show-then-clear (e.g. "Scan another card"
  // while the archive is still being read into memory): the stale load must
  // not land on the map after the blank style already did.
  const basemapKey = basemap ? `${basemap.path}|${basemap.downloadedAt}` : "";
  useEffect(() => {
    const map = mapRef.current;
    if (!map || !mapLoaded || basemapKey === shownBasemapKeyRef.current) return;
    let cancelled = false;

    (async () => {
      let style: StyleSpecification = BLANK_STYLE;
      if (basemapKey) {
        const resolved = await loadOfflineBasemapStyle().catch(() => null);
        if (cancelled || !resolved) return;
        style = resolved.style;
      }

      map.once("style.load", () => {
        const latest = latestRef.current;
        addBaseLayers(map);
        applyData(map, latest.result);
        applyVisibility(map, latest.visibility);
        applyMosaic(map, latest.stitchResult, latest.mosaicOpacity, latest.visibility);
      });
      map.setStyle(style, { diff: false });
      shownBasemapKeyRef.current = basemapKey;
      applyBasemapBounds(map, basemap);
      if (!basemap) setEdgeToastVisible(false);

      // Attribution only while real third-party map data is on screen.
      if (basemapKey && !attributionRef.current) {
        attributionRef.current = new maplibregl.AttributionControl();
        map.addControl(attributionRef.current, "bottom-right");
      } else if (!basemapKey && attributionRef.current) {
        map.removeControl(attributionRef.current);
        attributionRef.current = null;
      }
    })();

    return () => {
      cancelled = true;
    };
    // `basemap` is fully identified by basemapKey.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [basemapKey, mapLoaded]);

  /** Locks the camera inside the basemap's extent (or unlocks it for null),
   * so the user can never pan or zoom out past the downloaded data into
   * blank background. maxBounds does the clamping; minZoom is set to the
   * matching zoom too, so the zoom-out button disables itself at the limit
   * and the wheel handler above can tell the limit was hit. */
  function applyBasemapBounds(map: maplibregl.Map, info: OfflineBasemapInfo | null) {
    boundsLockRef.current = info?.bbox ?? null;
    if (!info) {
      map.setMaxBounds(null);
      map.setMinZoom(null);
      return;
    }
    const { minLon, minLat, maxLon, maxLat } = info.bbox;
    map.setMinZoom(coverZoom(map, info.bbox));
    map.setMaxBounds([
      [minLon, minLat],
      [maxLon, maxLat],
    ]);
  }

  useEffect(() => {
    const map = mapRef.current;
    if (!map || !loadedRef.current) return;
    applyData(map, result);
    if (result) {
      const bounds = boundsOf(result);
      if (bounds) map.fitBounds(bounds, { padding: 60, duration: 400, maxZoom: 20 });
    }
  }, [result]);

  useEffect(() => {
    const map = mapRef.current;
    if (!map || !loadedRef.current) return;
    applyVisibility(map, visibility);
  }, [visibility]);

  useEffect(() => {
    const map = mapRef.current;
    if (!map || !loadedRef.current) return;
    applyMosaic(map, stitchResult, mosaicOpacity, visibility);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [stitchResult]);

  useEffect(() => {
    const map = mapRef.current;
    if (!map || !loadedRef.current || !map.getLayer(MOSAIC_LAYER_ID)) return;
    map.setPaintProperty(MOSAIC_LAYER_ID, "raster-opacity", mosaicOpacity);
    map.setLayoutProperty(MOSAIC_LAYER_ID, "visibility", visibility.mosaic ? "visible" : "none");
  }, [mosaicOpacity, visibility.mosaic]);

  return (
    <>
      <div ref={containerRef} className="flight-map" />
      {edgeToastVisible && (
        <div className="map-toast">
          <Notice variant="info">You've reached the edge of the offline map.</Notice>
        </div>
      )}
    </>
  );
});

/** Web Mercator tile size MapLibre uses for its world size (512 * 2^zoom px). */
const MERCATOR_TILE_PX = 512;

/** The lowest zoom at which the map's viewport still fits entirely inside
 * `bbox` - "cover", not "contain": at any lower zoom some of the viewport
 * would fall outside the downloaded data. */
function coverZoom(map: maplibregl.Map, bbox: BasemapBbox): number {
  const container = map.getContainer();
  const nw = maplibregl.MercatorCoordinate.fromLngLat([bbox.minLon, bbox.maxLat]);
  const se = maplibregl.MercatorCoordinate.fromLngLat([bbox.maxLon, bbox.minLat]);
  const zoomForWidth = Math.log2(container.clientWidth / (MERCATOR_TILE_PX * (se.x - nw.x)));
  const zoomForHeight = Math.log2(container.clientHeight / (MERCATOR_TILE_PX * (se.y - nw.y)));
  return Math.max(zoomForWidth, zoomForHeight, 0);
}

function applyData(map: maplibregl.Map, result: InspectionResult | null) {
  const flightSource = map.getSource("flight") as maplibregl.GeoJSONSource | undefined;
  const footprintSource = map.getSource("footprints") as maplibregl.GeoJSONSource | undefined;
  const heatmapSource = map.getSource("heatmap") as maplibregl.GeoJSONSource | undefined;
  if (!flightSource || !footprintSource || !heatmapSource) return;

  if (!result) {
    flightSource.setData(EMPTY_FC);
    footprintSource.setData(EMPTY_FC);
    heatmapSource.setData(EMPTY_FC);
    return;
  }

  flightSource.setData(toFeatureCollection(result.flightPath));
  heatmapSource.setData(toFeatureCollection(result.heatmap));

  const footprintFeatures: GeoFeature[] = result.photos
    .filter((p) => p.footprint)
    .map((p) => ({
      type: "Feature",
      geometry: { type: "Polygon", coordinates: [p.footprint as [number, number][]] },
      properties: { fileName: p.fileName },
    }));
  footprintSource.setData(toFeatureCollection(footprintFeatures));
}

function applyVisibility(map: maplibregl.Map, visibility: LayerVisibility) {
  const set = (id: string, visible: boolean) => {
    if (map.getLayer(id)) map.setLayoutProperty(id, "visibility", visible ? "visible" : "none");
  };
  set("flight-path-line", visibility.flightPath);
  set("photo-points", visibility.photoPoints);
  set("footprints-fill", visibility.footprints);
  set("footprints-outline", visibility.footprints);
  set("heatmap-fill", visibility.heatmap);
  set(MOSAIC_LAYER_ID, visibility.mosaic);
}
