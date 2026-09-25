import type { BasemapBbox, PhotoMeta } from "../types/flight";

/** The tightest box around a survey's own GPS points hugs the camera
 * positions exactly, not the ground actually photographed (each photo's
 * footprint extends beyond its capture point) - pad it a bit so the
 * downloaded area comfortably covers the whole footprint, not just where the
 * drone happened to be. */
const PADDING_FRACTION = 0.15;
const MIN_PADDING_DEG = 0.01;

export function bboxOfPhotos(photos: PhotoMeta[]): BasemapBbox | null {
  let minLon = Infinity;
  let minLat = Infinity;
  let maxLon = -Infinity;
  let maxLat = -Infinity;
  let touched = false;

  for (const photo of photos) {
    if (photo.lat == null || photo.lon == null) continue;
    touched = true;
    minLon = Math.min(minLon, photo.lon);
    maxLon = Math.max(maxLon, photo.lon);
    minLat = Math.min(minLat, photo.lat);
    maxLat = Math.max(maxLat, photo.lat);
  }
  if (!touched) return null;

  const lonPad = Math.max((maxLon - minLon) * PADDING_FRACTION, MIN_PADDING_DEG);
  const latPad = Math.max((maxLat - minLat) * PADDING_FRACTION, MIN_PADDING_DEG);

  return {
    minLon: Math.max(minLon - lonPad, -180),
    maxLon: Math.min(maxLon + lonPad, 180),
    minLat: Math.max(minLat - latPad, -85.06),
    maxLat: Math.min(maxLat + latPad, 85.06),
  };
}

/** Whether `outer` fully contains `inner` - used to decide an already
 * downloaded offline basemap still covers a new flight's area. */
export function bboxContains(outer: BasemapBbox, inner: BasemapBbox): boolean {
  return (
    outer.minLon <= inner.minLon &&
    outer.minLat <= inner.minLat &&
    outer.maxLon >= inner.maxLon &&
    outer.maxLat >= inner.maxLat
  );
}

const MAX_MERCATOR_LAT = 85.0511;

// Web Mercator in normalized units (x, y both 0..1, y growing southward),
// the same projection MapLibre renders in.
const lonToX = (lon: number) => (lon + 180) / 360;
const xToLon = (x: number) => x * 360 - 180;
const latToY = (lat: number) => {
  const rad = (Math.max(-MAX_MERCATOR_LAT, Math.min(MAX_MERCATOR_LAT, lat)) * Math.PI) / 180;
  return (1 - Math.log(Math.tan(rad) + 1 / Math.cos(rad)) / Math.PI) / 2;
};
const yToLat = (y: number) => (Math.atan(Math.sinh(Math.PI * (1 - 2 * y))) * 180) / Math.PI;

/** Grows `bbox` (never shrinks it) around its own center until its on-screen
 * shape matches a `width` x `height` viewport. FlightMap locks the camera
 * inside the basemap's extent, so a basemap narrower than the map panel on
 * either axis would force the view to zoom in until it fits - cropping the
 * flight overview the result-driven fitBounds is trying to show. Shaping the
 * downloaded area to the panel keeps the whole flight viewable at once. */
export function fitBboxToAspect(bbox: BasemapBbox, width: number, height: number): BasemapBbox {
  if (width <= 0 || height <= 0) return bbox;
  let minX = lonToX(bbox.minLon);
  let maxX = lonToX(bbox.maxLon);
  let minY = latToY(bbox.maxLat);
  let maxY = latToY(bbox.minLat);
  const targetAspect = width / height;
  const w = maxX - minX;
  const h = maxY - minY;

  if (w / h < targetAspect) {
    const grow = (h * targetAspect - w) / 2;
    minX -= grow;
    maxX += grow;
  } else {
    const grow = (w / targetAspect - h) / 2;
    minY -= grow;
    maxY += grow;
  }

  return {
    minLon: Math.max(xToLon(minX), -180),
    maxLon: Math.min(xToLon(maxX), 180),
    minLat: Math.max(yToLat(maxY), -85.06),
    maxLat: Math.min(yToLat(minY), 85.06),
  };
}
