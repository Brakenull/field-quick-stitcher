import type { Metrics } from "../types/flight";

function formatArea(m2: number): string {
  if (m2 >= 10_000) return `${(m2 / 10_000).toFixed(2)} ha`;
  return `${m2.toFixed(0)} m²`;
}

export function MetricsBar({ metrics }: { metrics: Metrics }) {
  const items: { label: string; value: string }[] = [
    { label: "Photos", value: String(metrics.totalPhotos) },
    { label: "Coverage area", value: formatArea(metrics.coverageAreaM2) },
    {
      label: "Avg. altitude",
      value: metrics.avgAltitudeM != null ? `${metrics.avgAltitudeM.toFixed(1)} m` : "—",
    },
    { label: "Gaps", value: String(metrics.gapCount) },
    { label: "Blurry photos", value: String(metrics.blurryCount) },
    { label: "Scan time", value: `${(metrics.scanDurationMs / 1000).toFixed(1)} s` },
  ];

  return (
    <div className="metrics-bar" role="group" aria-label="Scan summary">
      {items.map((item) => (
        <div className="metrics-bar__item" key={item.label}>
          <span className="metrics-bar__value">{item.value}</span>
          <span className="metrics-bar__label">{item.label}</span>
        </div>
      ))}
    </div>
  );
}
