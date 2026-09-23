import { useCallback, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { InspectionResult, ScanStatus } from "../types/flight";

export function useFlightInspector() {
  const [status, setStatus] = useState<ScanStatus>("idle");
  const [progress, setProgress] = useState(0);
  const [result, setResult] = useState<InspectionResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const unlistenRef = useRef<null | (() => void)>(null);

  const inspect = useCallback(async (path: string): Promise<InspectionResult | null> => {
    setStatus("scanning");
    setProgress(0);
    setError(null);

    unlistenRef.current?.();
    unlistenRef.current = await listen<number>("scan_progress", (event) => {
      setProgress(event.payload);
    });

    try {
      const inspection = await invoke<InspectionResult>("inspect_directory", { path });
      setResult(inspection);
      setStatus("done");
      return inspection;
    } catch (err) {
      setError(String(err));
      setStatus("error");
      return null;
    } finally {
      unlistenRef.current?.();
      unlistenRef.current = null;
    }
  }, []);

  const exportReport = useCallback(async (format: "json" | "pdf", path: string) => {
    await invoke("export_report", { format, path });
  }, []);

  const reset = useCallback(() => {
    setStatus("idle");
    setProgress(0);
    setResult(null);
    setError(null);
  }, []);

  return { status, progress, result, error, inspect, exportReport, reset };
}
