use std::fs;
use std::fs::File;
use std::io::BufWriter;

use printpdf::{BuiltinFont, Mm, PdfDocument};
use tauri::State;

use crate::models::inspection_result::InspectionResult;
use crate::AppState;

#[tauri::command]
pub fn export_report(state: State<'_, AppState>, format: String, path: String) -> Result<(), String> {
    let result = state
        .last_inspection
        .lock()
        .map_err(|e| e.to_string())?
        .clone()
        .ok_or_else(|| "No inspection result available. Run a scan first.".to_string())?;

    match format.as_str() {
        "json" => export_json(&result, &path),
        "pdf" => export_pdf(&result, &path),
        other => Err(format!("Unknown export format: {other}")),
    }
}

fn export_json(result: &InspectionResult, path: &str) -> Result<(), String> {
    let json = serde_json::to_string_pretty(result).map_err(|e| e.to_string())?;
    fs::write(path, json).map_err(|e| e.to_string())
}

fn export_pdf(result: &InspectionResult, path: &str) -> Result<(), String> {
    let (doc, page1, layer1) = PdfDocument::new("Flight Inspection Report", Mm(210.0), Mm(297.0), "Layer 1");
    let font = doc
        .add_builtin_font(BuiltinFont::Helvetica)
        .map_err(|e| e.to_string())?;
    let bold = doc
        .add_builtin_font(BuiltinFont::HelveticaBold)
        .map_err(|e| e.to_string())?;
    let layer = doc.get_page(page1).get_layer(layer1);

    let mut y = 280.0;
    layer.use_text("Flight Inspection Report", 18.0, Mm(15.0), Mm(y), &bold);
    y -= 12.0;

    let m = &result.metrics;
    let summary_lines = [
        format!("Total photos: {}", m.total_photos),
        format!("Photos with GPS: {}", m.photos_with_gps),
        format!("Blurry photos: {}", m.blurry_count),
        format!("Coverage gaps: {}", m.gap_count),
        format!(
            "Average altitude: {}",
            m.avg_altitude_m
                .map(|a| format!("{a:.1} m"))
                .unwrap_or_else(|| "n/a".to_string())
        ),
        format!("Coverage area: {:.0} m^2", m.coverage_area_m2),
        format!("Scan duration: {} ms", m.scan_duration_ms),
    ];
    for line in summary_lines {
        layer.use_text(line, 12.0, Mm(15.0), Mm(y), &font);
        y -= 7.0;
    }

    y -= 8.0;
    layer.use_text("Gaps (fly-back coordinates):", 13.0, Mm(15.0), Mm(y), &bold);
    y -= 8.0;
    if result.gaps.is_empty() {
        layer.use_text("None - full coverage.", 11.0, Mm(15.0), Mm(y), &font);
        y -= 6.0;
    } else {
        for gap in &result.gaps {
            if y < 15.0 {
                break;
            }
            layer.use_text(
                format!("- {:.6}, {:.6}  (~{:.0} m^2)", gap.lat, gap.lon, gap.area_m2),
                11.0,
                Mm(15.0),
                Mm(y),
                &font,
            );
            y -= 6.0;
        }
    }

    y -= 8.0;
    layer.use_text("Blurry photos:", 13.0, Mm(15.0), Mm(y), &bold);
    y -= 8.0;
    if result.blur_alerts.is_empty() {
        layer.use_text("None detected.", 11.0, Mm(15.0), Mm(y), &font);
    } else {
        for alert in &result.blur_alerts {
            if y < 15.0 {
                break;
            }
            layer.use_text(
                format!(
                    "- {} at {:.6}, {:.6} (score {:.0})",
                    alert.file_name, alert.lat, alert.lon, alert.blur_score
                ),
                11.0,
                Mm(15.0),
                Mm(y),
                &font,
            );
            y -= 6.0;
        }
    }

    let file = File::create(path).map_err(|e| e.to_string())?;
    let mut writer = BufWriter::new(file);
    doc.save(&mut writer).map_err(|e| e.to_string())
}
