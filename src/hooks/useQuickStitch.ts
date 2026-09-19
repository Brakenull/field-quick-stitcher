import { useCallback, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { NeighborCap, StitchBackend, StitchProgress, StitchResult, StitchStatus } from "../types/flight";

const IDLE_PROGRESS: StitchProgress = { stage: "downsampling", percent: 0 };

export function useQuickStitch() {
  const [status, setStatus] = useState<StitchStatus>("idle");
  const [progress, setProgress] = useState<StitchProgress>(IDLE_PROGRESS);
  const [result, setResult] = useState<StitchResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const unlistenRef = useRef<null | (() => void)>(null);

  const stitch = useCallback(async (outputPath: string, backend: StitchBackend, neighborCap: NeighborCap) => {
    setStatus("stitching");
    setProgress(IDLE_PROGRESS);
    setError(null);

    unlistenRef.current?.();
    unlistenRef.current = await listen<StitchProgress>("stitch_progress", (event) => {
      setProgress(event.payload);
    });

    try {
      const stitchResult = await invoke<StitchResult>("quick_stitch", { outputPath, backend, neighborCap });
      setResult(stitchResult);
      setStatus("done");
    } catch (err) {
      setError(String(err));
      setStatus("error");
    } finally {
      unlistenRef.current?.();
      unlistenRef.current = null;
    }
  }, []);

  const reset = useCallback(() => {
    setStatus("idle");
    setProgress(IDLE_PROGRESS);
    setResult(null);
    setError(null);
  }, []);

  return { status, progress, result, error, stitch, reset };
}
