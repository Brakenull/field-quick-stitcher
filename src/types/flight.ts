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
