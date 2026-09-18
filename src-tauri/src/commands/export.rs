use std::fs;
use std::fs::File;
use std::io::BufWriter;

use printpdf::{BuiltinFont, IndirectFontRef, Mm, PdfDocument, PdfDocumentReference, PdfLayerReference};
use tauri::State;

use crate::models::inspection_result::InspectionResult;
use crate::AppState;

// f32, not f64 - matches printpdf::Mm's own wrapped type.
const PAGE_WIDTH_MM: f32 = 210.0;
const PAGE_HEIGHT_MM: f32 = 297.0;
const PAGE_TOP_MM: f32 = 280.0;
const PAGE_LEFT_MM: f32 = 15.0;
/// Below this, a new page is started rather than letting content run off the
/// bottom - the bug this whole helper exists to fix (long gap/blur lists used
/// to just stop mid-page with no indication anything was cut off).
const PAGE_BOTTOM_MARGIN_MM: f32 = 20.0;

/// Wraps a `printpdf` document/layer pair with a running `y` cursor that
/// starts a new page (with a small "(cont'd)" header) once content would run
/// past `PAGE_BOTTOM_MARGIN_MM`, instead of the single fixed A4 page this
/// replaced - which silently dropped the tail of any gap/blur list long
/// enough to reach the bottom margin, with no indication to the pilot that
/// anything was cut off.
struct PdfWriter<'a> {
    doc: &'a PdfDocumentReference,
    font: &'a IndirectFontRef,
    bold: &'a IndirectFontRef,
    layer: PdfLayerReference,
    y: f32,
    page_number: usize,
}

impl<'a> PdfWriter<'a> {
    fn new(doc: &'a PdfDocumentReference, layer: PdfLayerReference, font: &'a IndirectFontRef, bold: &'a IndirectFontRef) -> Self {
        Self { doc, font, bold, layer, y: PAGE_TOP_MM, page_number: 1 }
    }

    fn ensure_space(&mut self, step_mm: f32) {
        if self.y - step_mm >= PAGE_BOTTOM_MARGIN_MM {
            return;
        }
        let (page, layer) = self.doc.add_page(Mm(PAGE_WIDTH_MM), Mm(PAGE_HEIGHT_MM), "Layer 1");
        self.layer = self.doc.get_page(page).get_layer(layer);
        self.page_number += 1;
        self.y = PAGE_TOP_MM;
        self.layer
            .use_text(format!("Flight Inspection Report (page {})", self.page_number), 11.0, Mm(PAGE_LEFT_MM), Mm(self.y), self.bold);
        self.y -= 10.0;
    }

    fn title(&mut self, text: impl Into<String>) {
        self.layer.use_text(text.into(), 18.0, Mm(PAGE_LEFT_MM), Mm(self.y), self.bold);
        self.y -= 12.0;
    }

    fn heading(&mut self, text: impl Into<String>) {
        self.ensure_space(12.0);
        self.layer.use_text(text.into(), 13.0, Mm(PAGE_LEFT_MM), Mm(self.y), self.bold);
        self.y -= 8.0;
    }

    fn line(&mut self, text: impl Into<String>) {
        self.ensure_space(6.0);
        self.layer.use_text(text.into(), 11.0, Mm(PAGE_LEFT_MM), Mm(self.y), self.font);
        self.y -= 6.0;
    }

    fn gap(&mut self, mm: f32) {
        self.y -= mm;
    }
}

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
    let (doc, page1, layer1) = PdfDocument::new("Flight Inspection Report", Mm(PAGE_WIDTH_MM), Mm(PAGE_HEIGHT_MM), "Layer 1");
    let font = doc
        .add_builtin_font(BuiltinFont::Helvetica)
        .map_err(|e| e.to_string())?;
    let bold = doc
        .add_builtin_font(BuiltinFont::HelveticaBold)
        .map_err(|e| e.to_string())?;
    let layer = doc.get_page(page1).get_layer(layer1);

    let mut w = PdfWriter::new(&doc, layer, &font, &bold);
    w.title("Flight Inspection Report");

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
        w.line(line);
    }

    w.gap(2.0);
    w.heading("Gaps (fly-back coordinates):");
    if result.gaps.is_empty() {
        w.line("None - full coverage.");
    } else {
        for gap in &result.gaps {
            w.line(format!("- {:.6}, {:.6}  (~{:.0} m^2)", gap.lat, gap.lon, gap.area_m2));
        }
    }

    w.gap(2.0);
    w.heading("Blurry photos:");
    if result.blur_alerts.is_empty() {
        w.line("None detected.");
    } else {
        for alert in &result.blur_alerts {
            w.line(format!(
                "- {} at {:.6}, {:.6} (score {:.0})",
                alert.file_name, alert.lat, alert.lon, alert.blur_score
            ));
        }
    }

    let file = File::create(path).map_err(|e| e.to_string())?;
    let mut writer = BufWriter::new(file);
    doc.save(&mut writer).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::inspection_result::{GapAlert, Metrics};

    fn sample_result(gap_count: usize) -> InspectionResult {
        InspectionResult {
            photos: Vec::new(),
            flight_path: Vec::new(),
            heatmap: Vec::new(),
            gaps: (0..gap_count)
                .map(|i| GapAlert { id: i as u32, lat: 10.0 + i as f64 * 0.0001, lon: 106.0, cell_count: 1, area_m2: 9.0 })
                .collect(),
            blur_alerts: Vec::new(),
            metrics: Metrics {
                total_photos: 10,
                photos_with_gps: 10,
                blurry_count: 0,
                gap_count,
                avg_altitude_m: Some(80.0),
                coverage_area_m2: 1000.0,
                scan_duration_ms: 42,
            },
        }
    }

    #[test]
    fn short_report_fits_on_one_page() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("short.pdf");
        export_pdf(&sample_result(2), path.to_str().unwrap()).expect("export");

        let doc = printpdf::lopdf::Document::load(&path).expect("load pdf");
        assert_eq!(doc.get_pages().len(), 1);
    }

    #[test]
    fn long_gap_list_paginates_instead_of_truncating() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("long.pdf");
        export_pdf(&sample_result(80), path.to_str().unwrap()).expect("export");

        let doc = printpdf::lopdf::Document::load(&path).expect("load pdf");
        assert!(doc.get_pages().len() > 1, "80 gaps at ~6mm/line should overflow a single A4 page");
    }
}
