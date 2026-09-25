/** Thin indigo-to-amber progress track with a mono label above it (DESIGN.md 4.8). */
export function ProgressBar({ percent, label }: { percent: number; label: string }) {
  return (
    <div className="progress">
      <span className="progress__label" aria-live="polite">
        {label}
      </span>
      <div
        className="progress__bar"
        role="progressbar"
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={percent}
        aria-label={label}
      >
        <div className="progress__fill" style={{ width: `${percent}%` }} />
      </div>
    </div>
  );
}
