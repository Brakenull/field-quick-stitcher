import * as maplibregl from "maplibre-gl";
import type { StyleSpecification } from "maplibre-gl";
import { PMTiles, Protocol, type RangeResponse, type Source } from "pmtiles";
import { layers as protomapsLayers, LIGHT } from "@protomaps/basemaps";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import type { BasemapBbox, OfflineBasemapInfo } from "../types/flight";

// The offline basemap is downloaded per-area by the user via
// OfflineBasemapPanel/download_offline_basemap into Tauri's app-data dir, then
// loaded here through the asset protocol like the Quick Stitch preview PNG.
// Used by FlightMap (only while an inspected flight is on screen, via its
// `basemap` prop) and by OfflineBasemapPreview (the Download map tab's hover
// preview - the only way to see a basemap outside an inspection).
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

function offlineBasemapStyle(key: string, maxZoom: number, bbox: BasemapBbox): StyleSpecification {
  // Only fill/line layers - both:
  // - "background"-type layers deliberately excluded: unlike fill/line, a
  //   background layer doesn't read from any source at all, so it paints one
  //   flat color across the *entire* viewport regardless of zoom or where
  //   real tile data exists. Keeping it made a single small downloaded area
  //   look like full worldwide coverage once zoomed out far enough to see
  //   past it - it's not real map data out there, just a flat fill. Our own
  //   single neutral background layer below replaces it, honestly, the same
  //   color everywhere whether or not that area was actually downloaded.
  // - symbol (text/icon) layers excluded too: they need a glyphs/sprite
  //   service we don't bundle, and would otherwise just silently fail to
  //   render labels while still costing a style-parse warning.
  const renderable = protomapsLayers(OFFLINE_BASEMAP_SOURCE_ID, LIGHT).filter((l) => l.type === "fill" || l.type === "line");
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
        // Tells MapLibre the archive only has data in this area, so it
        // doesn't bother requesting tiles anywhere outside it.
        bounds: [bbox.minLon, bbox.minLat, bbox.maxLon, bbox.maxLat],
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
    layers: [
      { id: "background", type: "background", paint: { "background-color": "#F9FAFB" } },
      ...renderable,
    ],
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

  return { style: offlineBasemapStyle(info.path, info.maxZoom, info.bbox), bbox: info.bbox, maxZoom: info.maxZoom };
}
