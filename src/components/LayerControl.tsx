export interface LayerVisibility {
  flightPath: boolean;
  photoPoints: boolean;
  footprints: boolean;
  heatmap: boolean;
}

interface LayerControlProps {
  visibility: LayerVisibility;
  onChange: (visibility: LayerVisibility) => void;
}

const LABELS: Record<keyof LayerVisibility, string> = {
  flightPath: "Flight path",
  photoPoints: "Photo points",
  footprints: "Footprints",
  heatmap: "Overlap heatmap",
};

export function LayerControl({ visibility, onChange }: LayerControlProps) {
  return (
    <div className="layer-control">
      {(Object.keys(LABELS) as (keyof LayerVisibility)[]).map((key) => (
        <label key={key} className="layer-control__item">
          <input
            type="checkbox"
            checked={visibility[key]}
            onChange={(e) => onChange({ ...visibility, [key]: e.currentTarget.checked })}
          />
          {LABELS[key]}
        </label>
      ))}
    </div>
  );
}
