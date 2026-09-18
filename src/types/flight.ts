// Mirrors the Rust DTOs in src-tauri/src/models/*.rs (serde `rename_all = "camelCase"`).

export type LonLat = [number, number];

export interface PhotoMeta {
  fileName: string;
  path: string;
  /** `null` when the photo has no usable GPS EXIF (a GPS dropout) - it's still
   * kept in `photos` rather than dropped, so check `warnings` for
   * `"missing_gps"` or just this being `null` to find such photos. */
  lat: number | null;
  lon: number | null;
  relativeAltitude: number | null;
  /** Not currently used by any UI - captured for a future DEM-based terrain
   * correction (see `.claude/docs/1-inspect.md` section 7). */
  absoluteAltitude: number | null;
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

/** Mirrors `StitchBackend` (`src-tauri/src/models/stitch_result.rs`). */
export type StitchBackend = "onnx" | "orb";

export interface StitchResult {
  geotiffPath: string;
  previewPath: string;
  /** MapLibre `image` source order: top-left, top-right, bottom-right, bottom-left. */
  previewCorners: [LonLat, LonLat, LonLat, LonLat];
  backend: StitchBackend;
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
