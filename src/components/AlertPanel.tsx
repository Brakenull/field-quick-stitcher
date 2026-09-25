import type { BlurAlert, GapAlert } from "../types/flight";

interface AlertPanelProps {
  gaps: GapAlert[];
  blurAlerts: BlurAlert[];
  /** File names of photos with no usable GPS EXIF (a GPS dropout) - these
   * can't be plotted (no coordinates), so they're listed by name only. */
  missingGpsFiles: string[];
  onFocusPoint: (lat: number, lon: number) => void;
}

export function AlertPanel({ gaps, blurAlerts, missingGpsFiles, onFocusPoint }: AlertPanelProps) {
  return (
    <div className="alert-panel">
      <section>
        <h2 className="section-title">
          Coverage gaps <span className={`badge ${gaps.length ? "badge--danger" : "badge--success"}`}>{gaps.length}</span>
        </h2>
        {gaps.length === 0 ? (
          <p className="alert-panel__empty">No gaps. Every part of the area is covered.</p>
        ) : (
          <ul>
            {gaps.map((gap) => (
              <li key={gap.id}>
                <button className="alert-item alert-item--danger" onClick={() => onFocusPoint(gap.lat, gap.lon)}>
                  <span>
                    Gap #{gap.id + 1} — ~{gap.areaM2.toFixed(0)} m²
                  </span>
                  <span className="alert-item__coords">
                    {gap.lat.toFixed(6)}, {gap.lon.toFixed(6)}
                  </span>
                </button>
              </li>
            ))}
          </ul>
        )}
      </section>

      <section>
        <h2 className="section-title">
          Blurry photos{" "}
          <span className={`badge ${blurAlerts.length ? "badge--warning" : "badge--success"}`}>{blurAlerts.length}</span>
        </h2>
        {blurAlerts.length === 0 ? (
          <p className="alert-panel__empty">No blurry photos detected.</p>
        ) : (
          <ul>
            {blurAlerts.map((alert) => (
              <li key={alert.fileName}>
                <button className="alert-item alert-item--warning" onClick={() => onFocusPoint(alert.lat, alert.lon)}>
                  <span>{alert.fileName}</span>
                  <span className="alert-item__coords">
                    score {alert.blurScore.toFixed(0)} · {alert.lat.toFixed(6)}, {alert.lon.toFixed(6)}
                  </span>
                </button>
              </li>
            ))}
          </ul>
        )}
      </section>

      {missingGpsFiles.length > 0 && (
        <section>
          <h2 className="section-title">
            Missing GPS <span className="badge badge--warning">{missingGpsFiles.length}</span>
          </h2>
          <ul>
            {missingGpsFiles.map((fileName) => (
              <li key={fileName}>
                <span className="alert-item alert-item--static">{fileName}</span>
              </li>
            ))}
          </ul>
        </section>
      )}
    </div>
  );
}
