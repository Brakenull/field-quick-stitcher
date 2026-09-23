import * as maplibregl from "maplibre-gl";
import type { StyleSpecification } from "maplibre-gl";
import { PMTiles, Protocol, type RangeResponse, type Source } from "pmtiles";
import { layers as protomapsLayers, LIGHT } from "@protomaps/basemaps";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import type { BasemapBbox, OfflineBasemapInfo } from "../types/flight";

// The offline basemap is downloaded per-area by the user via
// OfflineBasemapPanel/download_offline_basemap into Tauri's app-data dir, then
// loaded here through the asset protocol like the Quick Stitch preview PNG.
// It's never wired into the main FlightMap - only OfflineBasemapPreview uses
// this, to render a hover preview from OfflineBasemapInfoCard.
const OFFLINE_BASEMAP_SOURCE_ID = "offline-basemap";
const OSM_ATTRIBUTION =
  '&copy; <a href="https://www.openstreetmap.org/copyright" target="_blank">OpenStreetMap</a> contributors';

let pmtilesProtocol: Protocol | null = null;
function ensurePmtilesProtocolRegistered(): Protocol {
  if (!pmtilesProtocol) {
    pmtilesProtocol = new Protocol();
    maplibregl.addProtocol("pmtiles", pmtilesProtocol.tile);
  }
  return pmtilesProtocol;
}

/** Reads tiles out of an in-memory copy of the archive instead of letting
 * `pmtiles` issue HTTP Range requests against Tauri's asset protocol -
 * that combination has known byte-serving issues in Tauri's custom-protocol
 * handler (https://github.com/orgs/tauri-apps/discussions/12243) and was
 * confirmed here too: a downloaded basemap "succeeded" but rendered as a
 * flat background with no vector tiles. A single whole-file GET through the
 * asset protocol is already proven to work in this app (the Quick Stitch
 * mosaic preview image uses the same `convertFileSrc` + `fetch` shape), so
 * this fetches once and serves tiles from the resulting buffer instead. */
class BufferSource implements Source {
  constructor(
    private key: string,
    private buffer: ArrayBuffer,
  ) {}
  getKey() {
    return this.key;
  }
  async getBytes(offset: number, length: number): Promise<RangeResponse> {
    return { data: this.buffer.slice(offset, offset + length) };
  }
}

function offlineBasemapStyle(key: string, maxZoom: number): StyleSpecification {
  // Keep only fill/line/background layers - symbol (text/icon) layers need a
  // glyphs/sprite service we don't bundle, and would otherwise just silently
  // fail to render labels while still costing a style-parse warning.
  const renderable = protomapsLayers(OFFLINE_BASEMAP_SOURCE_ID, LIGHT).filter(
    (l) => l.type === "fill" || l.type === "line" || l.type === "background",
  );
  return {
    version: 8,
    sources: {
      [OFFLINE_BASEMAP_SOURCE_ID]: {
        type: "vector",
        // Must match BufferSource's getKey() below exactly - the Protocol
        // resolves this URL to the matching registered PMTiles instance
        // rather than fetching it itself.
        url: `pmtiles://${key}`,
        attribution: OSM_ATTRIBUTION,
        // Without this, MapLibre requests tiles at whatever zoom the camera
        // reaches - the archive only has tiles up to the zoom it was
        // downloaded at, pmtiles.Protocol does no overzoom fallback (looks up
        // the exact z/x/y and returns nothing if absent), so those requests
        // silently come back empty and only the background paint shows.
        // Declaring maxzoom here tells MapLibre to stop requesting past it
        // and instead overzoom (upscale) the last available tile client-side.
        maxzoom: maxZoom,
      },
    },
    layers: renderable,
  } as StyleSpecification;
}

export interface ResolvedOfflineBasemap {
  style: StyleSpecification;
  bbox: BasemapBbox;
  maxZoom: number;
}

/** Fetches whatever offline basemap is cached on disk (if any) and builds a
 * standalone MapLibre style for it. Resolves null if nothing's downloaded. */
export async function loadOfflineBasemapStyle(): Promise<ResolvedOfflineBasemap | null> {
  const info = await invoke<OfflineBasemapInfo | null>("get_offline_basemap_info");
  if (!info) return null;

  const protocol = ensurePmtilesProtocolRegistered();
  const res = await fetch(convertFileSrc(info.path));
  if (!res.ok) throw new Error(`failed to read offline basemap file: ${res.status}`);
  const buffer = await res.arrayBuffer();
  protocol.add(new PMTiles(new BufferSource(info.path, buffer)));

  return { style: offlineBasemapStyle(info.path, info.maxZoom), bbox: info.bbox, maxZoom: info.maxZoom };
}
