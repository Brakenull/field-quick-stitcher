import { forwardRef, useEffect, useImperativeHandle, useRef } from "react";
import * as maplibregl from "maplibre-gl";
import type { StyleSpecification } from "maplibre-gl";
import "maplibre-gl/dist/maplibre-gl.css";
import type { GeoFeature, InspectionResult } from "../types/flight";
import type { LayerVisibility } from "./LayerControl";

export interface FlightMapHandle {
  flyTo: (lat: number, lon: number) => void;
}

interface FlightMapProps {
  result: InspectionResult | null;
  visibility: LayerVisibility;
}

// No offline basemap tiles are bundled yet, so the "map" is a blank canvas — the
// flight geometry (path, footprints, heatmap) is what actually matters here, and
// it renders with zero network requests either way.
const BLANK_STYLE: StyleSpecification = {
  version: 8,
  sources: {},
  layers: [{ id: "background", type: "background", paint: { "background-color": "#eef1f2" } }],
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

export const FlightMap = forwardRef<FlightMapHandle, FlightMapProps>(function FlightMap(
  { result, visibility },
  ref,
) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const mapRef = useRef<maplibregl.Map | null>(null);
  const loadedRef = useRef(false);

  useImperativeHandle(ref, () => ({
    flyTo(lat, lon) {
      mapRef.current?.flyTo({ center: [lon, lat], zoom: 19, duration: 600 });
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
      map.addSource("footprints", { type: "geojson", data: EMPTY_FC });
      map.addLayer({
        id: "footprints-fill",
        type: "fill",
        source: "footprints",
        paint: { "fill-color": "#3b82f6", "fill-opacity": 0.08 },
      });
      map.addLayer({
        id: "footprints-outline",
        type: "line",
        source: "footprints",
        paint: { "line-color": "#3b82f6", "line-width": 0.5, "line-opacity": 0.4 },
      });

      map.addSource("heatmap", { type: "geojson", data: EMPTY_FC });
      map.addLayer({
        id: "heatmap-fill",
        type: "fill",
        source: "heatmap",
        paint: {
          "fill-color": ["match", ["get", "level"], "red", "#ef4444", "yellow", "#eab308", "green", "#22c55e", "#999999"],
          "fill-opacity": 0.45,
        },
      });

      map.addSource("flight", { type: "geojson", data: EMPTY_FC });
      map.addLayer({
        id: "flight-path-line",
        type: "line",
        source: "flight",
        filter: ["==", ["geometry-type"], "LineString"],
        paint: { "line-color": "#1e293b", "line-width": 2 },
      });
      map.addLayer({
        id: "photo-points",
        type: "circle",
        source: "flight",
        filter: ["==", ["geometry-type"], "Point"],
        paint: {
          "circle-radius": 4,
          "circle-color": ["case", ["get", "isBlurry"], "#ef4444", "#2563eb"],
          "circle-stroke-color": "#ffffff",
          "circle-stroke-width": 1,
        },
      });

      loadedRef.current = true;
      applyData(map, result);
      applyVisibility(map, visibility);
    });

    mapRef.current = map;
    return () => {
      map.remove();
      mapRef.current = null;
      loadedRef.current = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

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

  return <div ref={containerRef} className="flight-map" />;
});

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
}
