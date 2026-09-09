export interface LayerVisibility {
  flightPath: boolean;
  photoPoints: boolean;
  footprints: boolean;
  heatmap: boolean;
  mosaic: boolean;
}

interface LayerControlProps {
  visibility: LayerVisibility;
  onChange: (visibility: LayerVisibility) => void;
  /** Only shown once a Quick Stitch mosaic exists to toggle/fade. */
  hasMosaic?: boolean;
  mosaicOpacity?: number;
  onMosaicOpacityChange?: (opacity: number) => void;
}

const LABELS: Record<keyof LayerVisibility, string> = {
  flightPath: "Flight path",
  photoPoints: "Photo points",
  footprints: "Footprints",
  heatmap: "Overlap heatmap",
  mosaic: "Quick Stitch mosaic",
};

export function LayerControl({ visibility, onChange, hasMosaic, mosaicOpacity, onMosaicOpacityChange }: LayerControlProps) {
  return (
    <div className="layer-control">
      {(Object.keys(LABELS) as (keyof LayerVisibility)[]).map((key) => {
        if (key === "mosaic" && !hasMosaic) return null;
        return (
          <label key={key} className="layer-control__item">
            <input
              type="checkbox"
              checked={visibility[key]}
              onChange={(e) => onChange({ ...visibility, [key]: e.currentTarget.checked })}
            />
            {LABELS[key]}
          </label>
        );
      })}
      {hasMosaic && visibility.mosaic && onMosaicOpacityChange && (
        <label className="layer-control__item layer-control__opacity">
          Mosaic opacity
          <input
            type="range"
            min={0}
            max={1}
            step={0.05}
            value={mosaicOpacity ?? 1}
            onChange={(e) => onMosaicOpacityChange(Number(e.currentTarget.value))}
          />
        </label>
      )}
    </div>
  );
}
