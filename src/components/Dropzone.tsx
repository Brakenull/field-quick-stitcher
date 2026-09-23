import { useEffect, useRef, useState } from "react";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open } from "@tauri-apps/plugin-dialog";

interface DropzoneProps {
  disabled: boolean;
  onFolderSelected: (path: string) => void;
}

export function Dropzone({ disabled, onFolderSelected }: DropzoneProps) {
  const [isDragOver, setIsDragOver] = useState(false);
  const disabledRef = useRef(disabled);
  disabledRef.current = disabled;
  // App re-renders continuously while scanning (progress events), so a
  // fresh onFolderSelected reference each render would otherwise sit in this
  // effect's deps and make it unlisten+relisten repeatedly during a scan -
  // a drop landing in that async teardown/re-registration gap could be
  // delivered to both the outgoing and incoming listener, firing
  // onFolderSelected twice for one physical drop (confirmed: this raced two
  // concurrent download_offline_basemap calls onto the same fixed tmp
  // filename). Routing through a ref keeps the listener registered exactly
  // once for the component's lifetime instead.
  const onFolderSelectedRef = useRef(onFolderSelected);
  onFolderSelectedRef.current = onFolderSelected;

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    getCurrentWebview()
      .onDragDropEvent((event) => {
        if (event.payload.type === "enter" || event.payload.type === "over") {
          setIsDragOver(true);
        } else if (event.payload.type === "leave") {
          setIsDragOver(false);
        } else if (event.payload.type === "drop") {
          setIsDragOver(false);
          if (!disabledRef.current && event.payload.paths.length > 0) {
            onFolderSelectedRef.current(event.payload.paths[0]);
          }
        }
      })
      .then((fn) => {
        unlisten = fn;
      });
    return () => unlisten?.();
  }, []);

  async function browse() {
    const path = await open({ directory: true, multiple: false });
    if (typeof path === "string") {
      onFolderSelected(path);
    }
  }

  return (
    <div className={`dropzone ${isDragOver ? "dropzone--active" : ""}`}>
      <p className="dropzone__title">Drag the memory card folder here</p>
      <p className="dropzone__hint">or</p>
      <button className="dropzone__browse" onClick={browse} disabled={disabled}>
        Browse folder…
      </button>
    </div>
  );
}
