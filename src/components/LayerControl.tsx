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
    <section className="layer-control" aria-labelledby="layers-heading">
      <h2 className="section-title" id="layers-heading">
        Map layers
      </h2>
      {(Object.keys(LABELS) as (keyof LayerVisibility)[]).map((key) => {
        if (key === "mosaic" && !hasMosaic) return null;
        return (
          <label key={key} className="toggle">
            <span className="toggle__label">{LABELS[key]}</span>
            <input
              type="checkbox"
              role="switch"
              className="toggle__input"
              checked={visibility[key]}
              onChange={(e) => onChange({ ...visibility, [key]: e.currentTarget.checked })}
            />
            <span className="toggle__track" aria-hidden="true" />
          </label>
        );
      })}
      {hasMosaic && visibility.mosaic && onMosaicOpacityChange && (
        <label className="layer-control__opacity">
          <span className="layer-control__opacity-label">
            Mosaic opacity
            <span className="mono">{Math.round((mosaicOpacity ?? 1) * 100)}%</span>
          </span>
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
    </section>
  );
}
