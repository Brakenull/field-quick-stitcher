import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { BasemapBbox, OfflineBasemapInfo, OfflineBasemapProgress, OfflineBasemapStatus } from "../types/flight";

const IDLE_PROGRESS: OfflineBasemapProgress = { stage: "connecting", tilesDone: 0, tilesTotal: 0, percent: 0 };

export function useOfflineBasemap() {
  const [status, setStatus] = useState<OfflineBasemapStatus>("idle");
  const [progress, setProgress] = useState<OfflineBasemapProgress>(IDLE_PROGRESS);
  const [info, setInfo] = useState<OfflineBasemapInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  const unlistenRef = useRef<null | (() => void)>(null);
  // A plain ref, not the `status` state - a second `download()` call can
  // start before React has committed the "downloading" state from the
  // first (e.g. an auto-download from Inspect racing a manual click before
  // the button's `disabled` prop re-renders), and checking `status` would
  // miss that. Two overlapping calls would otherwise both hit the backend,
  // and the loser's rejection ("already in progress") would overwrite this
  // shared state - stomping the still-running download's real status with
  // what looks like an error, even though it's fine and still in flight.
  const inFlightRef = useRef(false);

  // Reflects whatever's already on disk from a previous session (or a dev
  // bootstrap via fetch-offline-basemap.ps1) - also re-grants the webview's
  // asset-protocol access to it, since that scope is in-memory and resets on
  // every app launch (see get_offline_basemap_info's doc comment).
  useEffect(() => {
    invoke<OfflineBasemapInfo | null>("get_offline_basemap_info")
      .then(setInfo)
      .catch(() => {});
  }, []);

  const download = useCallback(async (bbox: BasemapBbox, maxZoom: number): Promise<OfflineBasemapInfo | null> => {
    if (inFlightRef.current) return null;
    inFlightRef.current = true;

    setStatus("downloading");
    setProgress(IDLE_PROGRESS);
    setError(null);

    unlistenRef.current?.();
    unlistenRef.current = await listen<OfflineBasemapProgress>("offline_basemap_progress", (event) => {
      setProgress(event.payload);
    });

    try {
      const result = await invoke<OfflineBasemapInfo>("download_offline_basemap", { bbox, maxZoom });
      setInfo(result);
      setStatus("done");
      return result;
    } catch (err) {
      setError(String(err));
      setStatus("error");
      return null;
    } finally {
      inFlightRef.current = false;
      unlistenRef.current?.();
      unlistenRef.current = null;
    }
  }, []);

  // Deliberately doesn't clear `info` - it reflects what's on disk, not
  // in-flight UI state, so it should stay visible even after dismissing an
  // error or a "done" banner.
  const reset = useCallback(() => {
    setStatus("idle");
    setProgress(IDLE_PROGRESS);
    setError(null);
  }, []);

  return { status, progress, info, error, download, reset };
}
