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
