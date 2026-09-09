// Mirrors the Rust DTOs in src-tauri/src/models/*.rs (serde `rename_all = "camelCase"`).

export type LonLat = [number, number];

export interface PhotoMeta {
  fileName: string;
  path: string;
  lat: number;
  lon: number;
  relativeAltitude: number | null;
  yawDeg: number | null;
  focalMm: number | null;
  sensorWidthMm: number | null;
  sensorHeightMm: number | null;
  imageWidthPx: number | null;
  imageHeightPx: number | null;
  footprint: LonLat[] | null;
  captureTime: string | null;
  isBlurry: boolean;
  blurScore: number | null;
  warnings: string[];
}

export interface GapAlert {
  id: number;
  lat: number;
  lon: number;
  cellCount: number;
  areaM2: number;
}

export interface BlurAlert {
  fileName: string;
  lat: number;
  lon: number;
  blurScore: number;
}

export interface Metrics {
  totalPhotos: number;
  photosWithGps: number;
  blurryCount: number;
  gapCount: number;
  avgAltitudeM: number | null;
  coverageAreaM2: number;
  scanDurationMs: number;
}

export type GeoGeometry =
  | { type: "Point"; coordinates: LonLat }
  | { type: "LineString"; coordinates: LonLat[] }
  | { type: "Polygon"; coordinates: LonLat[][] };

export interface GeoFeature {
  type: "Feature";
  geometry: GeoGeometry;
  properties: Record<string, unknown>;
}

export interface InspectionResult {
  photos: PhotoMeta[];
  flightPath: GeoFeature[];
  heatmap: GeoFeature[];
  gaps: GapAlert[];
  blurAlerts: BlurAlert[];
  metrics: Metrics;
}

export type ScanStatus = "idle" | "scanning" | "done" | "error";

export interface StitchResult {
  geotiffPath: string;
  previewPath: string;
  /** MapLibre `image` source order: top-left, top-right, bottom-right, bottom-left. */
  previewCorners: [LonLat, LonLat, LonLat, LonLat];
  photosUsed: number;
  photosSkipped: number;
  confidentPairs: number;
  durationMs: number;
  warnings: string[];
}

export type StitchStatus = "idle" | "stitching" | "done" | "error";

/** Mirrors `StitchStage` (`src-tauri/src/models/stitch_result.rs`), in pipeline order. */
export type StitchStage = "downsampling" | "detecting-features" | "matching-pairs" | "aligning-poses" | "compositing" | "exporting";

/** `percent` is 0-100 *within* `stage`, not overall pipeline progress. */
export interface StitchProgress {
  stage: StitchStage;
  percent: number;
}
