//! Reads GPS / altitude / gimbal-yaw / camera metadata from a JPEG without decoding
//! the full image: standard EXIF tags via `kamadak-exif`, plus DJI's `drone-dji`
//! XMP packet (RelativeAltitude, GimbalYawDegree, ...) via a lightweight manual
//! JPEG-segment scan that stops before the compressed scan data.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::time::UNIX_EPOCH;

use exif::{In, Reader, Tag, Value};

use crate::models::photo_meta::{CaptureOrderKey, PhotoMeta};

/// Fallback sensor sizes (width_mm, height_mm) keyed by a substring of the EXIF
/// `Model` tag. Only used when `FocalLengthIn35mmFilm` isn't present, since the
/// 35mm-equivalent computation is accurate per-photo and needs no lookup table.
const CAMERA_DB: &[(&str, f64, f64)] = &[
    ("FC330", 6.3, 4.7),    // Phantom 4 (1/2.3")
    ("FC6310", 13.2, 8.8),  // Phantom 4 Pro (1")
    ("FC6360", 13.2, 8.8),  // Phantom 4 Pro V2.0
    ("FC6540", 13.2, 8.8),  // Phantom 4 RTK
    ("L1D-20c", 13.2, 8.8), // Mavic 2 Pro (Hasselblad, 1")
    ("M3E", 17.3, 13.0),    // Mavic 3 Enterprise (4/3")
    ("M3T", 17.3, 13.0),    // Mavic 3 Thermal wide cam
];

const XMP_SIGNATURE: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";
/// Safety cap on how many header bytes we'll scan looking for markers before giving up.
const MAX_HEADER_SCAN_BYTES: u64 = 4 * 1024 * 1024;

pub fn parse_photo(path: &Path) -> PhotoMeta {
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    let mut warnings = Vec::new();
    let exif_data = File::open(path)
        .ok()
        .and_then(|f| Reader::new().read_from_container(&mut BufReader::new(f)).ok());
    if exif_data.is_none() {
        warnings.push("no_exif_data".to_string());
    }

    let lat = exif_data
        .as_ref()
        .and_then(|e| gps_coord(e, Tag::GPSLatitude, Tag::GPSLatitudeRef));
    let lon = exif_data
        .as_ref()
        .and_then(|e| gps_coord(e, Tag::GPSLongitude, Tag::GPSLongitudeRef));
    if lat.is_none() || lon.is_none() {
        warnings.push("missing_gps".to_string());
    }

    let focal_mm = exif_data.as_ref().and_then(|e| rational(e, Tag::FocalLength, In::PRIMARY));
    let focal_35mm = exif_data
        .as_ref()
        .and_then(|e| uint(e, Tag::FocalLengthIn35mmFilm, In::PRIMARY));
    let model = exif_data.as_ref().and_then(|e| ascii(e, Tag::Model, In::PRIMARY));

    let mut image_width_px = exif_data
        .as_ref()
        .and_then(|e| uint(e, Tag::PixelXDimension, In::PRIMARY));
    let mut image_height_px = exif_data
        .as_ref()
        .and_then(|e| uint(e, Tag::PixelYDimension, In::PRIMARY));
    if image_width_px.is_none() || image_height_px.is_none() {
        if let Ok(dims) = image::image_dimensions(path) {
            image_width_px = image_width_px.or(Some(dims.0));
            image_height_px = image_height_px.or(Some(dims.1));
        }
    }

    let (mut sensor_width_mm, mut sensor_height_mm) = (None, None);
    if let (Some(f), Some(f35), Some(w), Some(h)) = (focal_mm, focal_35mm, image_width_px, image_height_px) {
        if f35 > 0 {
            let sw = f * 36.0 / f35 as f64;
            let sh = sw * (h as f64 / w as f64);
            sensor_width_mm = Some(sw);
            sensor_height_mm = Some(sh);
        }
    }
    // Tier 2: standard EXIF focal-plane resolution tags - most mirrorless
    // cameras and higher-end drones (Autel, Skydio, Wingtra, interchangeable-
    // lens bodies) write these even when they don't write the DJI-specific
    // FocalLengthIn35mmFilm-friendly combination above, so this covers
    // non-DJI hardware the hardcoded CAMERA_DB below never will.
    if sensor_width_mm.is_none() {
        if let Some((w, h)) = exif_data.as_ref().and_then(|e| focal_plane_sensor_size(e, image_width_px, image_height_px)) {
            sensor_width_mm = Some(w);
            sensor_height_mm = Some(h);
            warnings.push("used_focal_plane_resolution".to_string());
        }
    }
    // Tier 3: hardcoded DJI camera model lookup, last resort.
    if sensor_width_mm.is_none() {
        if let Some(model) = &model {
            if let Some((_, w, h)) = CAMERA_DB.iter().find(|(needle, _, _)| model.contains(needle)) {
                sensor_width_mm = Some(*w);
                sensor_height_mm = Some(*h);
                warnings.push("used_camera_db_fallback".to_string());
            }
        }
    }
    if sensor_width_mm.is_none() {
        warnings.push("missing_sensor_size".to_string());
    }
    if focal_mm.is_none() {
        warnings.push("missing_focal_length".to_string());
    }

    let capture_time = exif_data.as_ref().and_then(|e| {
        e.get_field(Tag::DateTimeOriginal, In::PRIMARY)
            .map(|f| f.display_value().to_string())
    });
    let sort_key = capture_order_key(exif_data.as_ref(), path, &file_name);

    let xmp = read_xmp_packet(path);
    let relative_altitude = xmp.as_deref().and_then(|x| xmp_attr_f64(x, "drone-dji:RelativeAltitude"));
    if relative_altitude.is_none() {
        warnings.push("missing_altitude".to_string());
    }
    // Not consumed anywhere yet (see `PhotoMeta::absolute_altitude`'s doc
    // comment) - captured now so a future DEM-based terrain-correction pass
    // has this available without needing another metadata-parsing change.
    let absolute_altitude = xmp.as_deref().and_then(|x| xmp_attr_f64(x, "drone-dji:AbsoluteAltitude"));
    let yaw_deg = xmp
        .as_deref()
        .and_then(|x| xmp_attr_f64(x, "drone-dji:GimbalYawDegree"))
        .or_else(|| xmp.as_deref().and_then(|x| xmp_attr_f64(x, "drone-dji:FlightYawDegree")));
    if yaw_deg.is_none() {
        warnings.push("missing_yaw".to_string());
    }

    PhotoMeta {
        file_name,
        path: path.to_string_lossy().to_string(),
        lat,
        lon,
        relative_altitude,
        absolute_altitude,
        yaw_deg,
        focal_mm,
        sensor_width_mm,
        sensor_height_mm,
        image_width_px,
        image_height_px,
        footprint: None,
        capture_time,
        sort_key,
        is_blurry: false,
        blur_score: None,
        warnings,
    }
}

fn rational(exif: &exif::Exif, tag: Tag, ifd: In) -> Option<f64> {
    let field = exif.get_field(tag, ifd)?;
    match &field.value {
        Value::Rational(v) => v.first().map(|r| r.to_f64()),
        Value::SRational(v) => v.first().map(|r| r.to_f64()),
        _ => None,
    }
}

fn uint(exif: &exif::Exif, tag: Tag, ifd: In) -> Option<u32> {
    let field = exif.get_field(tag, ifd)?;
    match &field.value {
        Value::Short(v) => v.first().map(|&x| x as u32),
        Value::Long(v) => v.first().copied(),
        _ => None,
    }
}

fn ascii(exif: &exif::Exif, tag: Tag, ifd: In) -> Option<String> {
    let field = exif.get_field(tag, ifd)?;
    match &field.value {
        Value::Ascii(v) => v
            .first()
            .map(|b| String::from_utf8_lossy(b).trim_matches(char::from(0)).trim().to_string()),
        _ => None,
    }
}

/// Standard-EXIF sensor-size derivation from the focal-plane resolution tags
/// (`FocalPlaneXResolution`/`YResolution`, in pixels per unit, plus
/// `FocalPlaneResolutionUnit`) - a tier between the 35mm-equivalent
/// derivation above and the hardcoded `CAMERA_DB` fallback below. Most
/// mirrorless cameras and non-DJI drones (Autel, Skydio, Wingtra,
/// interchangeable-lens bodies) write these even without
/// `FocalLengthIn35mmFilm`, so this covers hardware `CAMERA_DB` never will.
/// Only handles the two standard resolution units (2 = inches, 3 =
/// centimeters); an absent or non-standard unit is treated as "can't derive"
/// rather than guessed.
fn focal_plane_sensor_size(exif: &exif::Exif, width_px: Option<u32>, height_px: Option<u32>) -> Option<(f64, f64)> {
    let x_res = rational(exif, Tag::FocalPlaneXResolution, In::PRIMARY)?;
    let y_res = rational(exif, Tag::FocalPlaneYResolution, In::PRIMARY)?;
    let unit = uint(exif, Tag::FocalPlaneResolutionUnit, In::PRIMARY);
    sensor_size_from_focal_plane_resolution(width_px?, height_px?, x_res, y_res, unit)
}

/// Pure core of [`focal_plane_sensor_size`], split out for direct testing.
fn sensor_size_from_focal_plane_resolution(
    width_px: u32,
    height_px: u32,
    x_res: f64,
    y_res: f64,
    resolution_unit: Option<u32>,
) -> Option<(f64, f64)> {
    if x_res <= 0.0 || y_res <= 0.0 {
        return None;
    }
    let mm_per_unit = match resolution_unit {
        Some(2) => 25.4, // inches
        Some(3) => 10.0, // centimeters
        _ => return None,
    };

    let width_mm = width_px as f64 / x_res * mm_per_unit;
    let height_mm = height_px as f64 / y_res * mm_per_unit;
    if !width_mm.is_finite() || !height_mm.is_finite() || width_mm <= 0.0 || height_mm <= 0.0 {
        return None;
    }
    Some((width_mm, height_mm))
}

fn gps_coord(exif: &exif::Exif, coord_tag: Tag, ref_tag: Tag) -> Option<f64> {
    let field = exif.get_field(coord_tag, In::PRIMARY)?;
    let rationals = match &field.value {
        Value::Rational(v) => v,
        _ => return None,
    };
    if rationals.len() < 3 {
        return None;
    }
    let deg = rationals[0].to_f64();
    let min = rationals[1].to_f64();
    let sec = rationals[2].to_f64();
    let mut val = deg + min / 60.0 + sec / 3600.0;

    if let Some(r) = exif.get_field(ref_tag, In::PRIMARY) {
        if let Value::Ascii(ref bytes_vec) = r.value {
            if let Some(bytes) = bytes_vec.first() {
                if bytes.starts_with(b"S") || bytes.starts_with(b"W") {
                    val = -val;
                }
            }
        }
    }
    Some(val)
}

/// Scans a JPEG's header segments (stopping at Start-Of-Scan, well before the
/// compressed pixel data) for the APP1 XMP packet, returning its raw XML text.
fn read_xmp_packet(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let mut soi = [0u8; 2];
    file.read_exact(&mut soi).ok()?;
    if soi != [0xFF, 0xD8] {
        return None;
    }

    let mut scanned: u64 = 2;
    loop {
        if scanned > MAX_HEADER_SCAN_BYTES {
            return None;
        }
        let mut marker = [0u8; 2];
        if file.read_exact(&mut marker).is_err() {
            return None;
        }
        scanned += 2;
        if marker[0] != 0xFF {
            return None;
        }
        let m = marker[1];
        // Markers with no payload/length field.
        if m == 0x01 || (0xD0..=0xD8).contains(&m) {
            continue;
        }
        if m == 0xD9 || m == 0xDA {
            // EOI or Start-Of-Scan: header segments are over.
            return None;
        }

        let mut len_buf = [0u8; 2];
        if file.read_exact(&mut len_buf).is_err() {
            return None;
        }
        scanned += 2;
        let seg_len = u16::from_be_bytes(len_buf) as usize;
        if seg_len < 2 {
            return None;
        }
        let payload_len = seg_len - 2;
        scanned += payload_len as u64;

        if m == 0xE1 {
            let mut payload = vec![0u8; payload_len];
            if file.read_exact(&mut payload).is_err() {
                return None;
            }
            if payload.starts_with(XMP_SIGNATURE) {
                return String::from_utf8(payload[XMP_SIGNATURE.len()..].to_vec()).ok();
            }
            continue;
        }
        if file.seek(SeekFrom::Current(payload_len as i64)).is_err() {
            return None;
        }
    }
}

/// Extracts a `name="value"` XMP/RDF attribute as f64 (DJI writes values like `+50.00`).
fn xmp_attr_f64(xml: &str, name: &str) -> Option<f64> {
    let needle = format!("{name}=\"");
    let start = xml.find(&needle)? + needle.len();
    let end = start + xml[start..].find('"')?;
    xml[start..end].trim().parse::<f64>().ok()
}

/// Best-effort chronological ordering key (see [`CaptureOrderKey`]): a parsed
/// capture timestamp with sub-second precision, else a numeric sequence
/// pulled from the file name, else the file's last-modified time, else
/// `Unresolved`. Replaces sorting on the raw EXIF display string, which
/// breaks on non-standard timestamp formats and can't disambiguate
/// same-second burst-mode shots, and on `Option`'s default ordering, which
/// would otherwise push every unresolved photo to the *front* of the flight
/// path instead of the back.
fn capture_order_key(exif: Option<&exif::Exif>, path: &Path, file_name: &str) -> CaptureOrderKey {
    if let Some(millis) = exif.and_then(parse_capture_timestamp_millis) {
        return CaptureOrderKey::Timestamp(millis);
    }
    if let Some(seq) = filename_sequence(file_name) {
        return CaptureOrderKey::FilenameSequence(seq);
    }
    if let Some(millis) = file_modified_millis(path) {
        return CaptureOrderKey::FileModified(millis);
    }
    CaptureOrderKey::Unresolved
}

/// Parses `DateTimeOriginal` (+ `SubSecTimeOriginal`, if present) into
/// milliseconds since the Unix epoch, via the raw EXIF ASCII value (not
/// `Field::display_value`'s reformatted string) so this doesn't depend on
/// `kamadak-exif`'s display formatting. Treated as a naive local timestamp -
/// EXIF carries no timezone - which is fine since this is only ever used to
/// order photos relative to each other within one flight, never compared
/// across flights or against wall-clock time.
fn parse_capture_timestamp_millis(exif: &exif::Exif) -> Option<i64> {
    let raw = ascii(exif, Tag::DateTimeOriginal, In::PRIMARY)?;
    let subsec = ascii(exif, Tag::SubSecTimeOriginal, In::PRIMARY);
    combine_datetime_and_subsec(&raw, subsec.as_deref())
}

/// Pure core of [`parse_capture_timestamp_millis`], split out so it's
/// testable without needing a real `exif::Exif` (its fields are private -
/// there's no way to construct one synthetically outside this crate's own
/// reader).
fn combine_datetime_and_subsec(raw_datetime: &str, subsec: Option<&str>) -> Option<i64> {
    let dt = exif::DateTime::from_ascii(raw_datetime.as_bytes()).ok()?;
    let date = chrono::NaiveDate::from_ymd_opt(dt.year as i32, dt.month as u32, dt.day as u32)?;
    let base = date.and_hms_opt(dt.hour as u32, dt.minute as u32, dt.second as u32)?;
    let millis = base.and_utc().timestamp_millis();
    let subsec_millis = subsec.map(subsec_str_to_millis).unwrap_or(0);
    Some(millis + subsec_millis as i64)
}

/// EXIF sub-second tags are a decimal-fraction *string*, not a place-value
/// integer - `"5"` means 0.5s (500ms), not 5ms. Extra digits beyond
/// millisecond precision are truncated, not rounded.
fn subsec_str_to_millis(s: &str) -> u32 {
    let digits: String = s.chars().filter(|c| c.is_ascii_digit()).take(3).collect();
    if digits.is_empty() {
        return 0;
    }
    format!("{digits:0<3}").parse().unwrap_or(0)
}

/// The last contiguous run of ASCII digits in the file name's stem (e.g.
/// `DJI_0123.JPG` -> `123`), used as a chronological proxy when no EXIF
/// timestamp is available - most drone/camera naming schemes end in a
/// monotonically increasing shot counter.
fn filename_sequence(file_name: &str) -> Option<u64> {
    let stem = Path::new(file_name).file_stem()?.to_str()?;
    let bytes = stem.as_bytes();
    let mut last_run: Option<&str> = None;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            last_run = Some(&stem[start..i]);
        } else {
            i += 1;
        }
    }
    last_run?.parse().ok()
}

fn file_modified_millis(path: &Path) -> Option<i64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let since_epoch = modified.duration_since(UNIX_EPOCH).ok()?;
    Some(since_epoch.as_millis() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xmp_attr_parses_signed_values() {
        let xml = r#"<rdf:Description drone-dji:RelativeAltitude="+50.30" drone-dji:GimbalYawDegree="-12.50">"#;
        assert_eq!(xmp_attr_f64(xml, "drone-dji:RelativeAltitude"), Some(50.30));
        assert_eq!(xmp_attr_f64(xml, "drone-dji:GimbalYawDegree"), Some(-12.50));
        assert_eq!(xmp_attr_f64(xml, "drone-dji:Missing"), None);
    }

    #[test]
    fn subsec_string_is_a_decimal_fraction_not_a_place_value() {
        // "5" means 0.5s (500ms), not 5ms - and extra digits are truncated.
        assert_eq!(subsec_str_to_millis("5"), 500);
        assert_eq!(subsec_str_to_millis("50"), 500);
        assert_eq!(subsec_str_to_millis("500"), 500);
        assert_eq!(subsec_str_to_millis("123456"), 123);
        assert_eq!(subsec_str_to_millis(""), 0);
    }

    #[test]
    fn filename_sequence_picks_the_last_digit_run_in_the_stem() {
        assert_eq!(filename_sequence("DJI_0123.JPG"), Some(123));
        assert_eq!(filename_sequence("IMG_20240501_0007.jpg"), Some(7));
        assert_eq!(filename_sequence("no_digits_here.jpg"), None);
    }

    #[test]
    fn capture_timestamp_orders_burst_mode_shots_by_subsecond() {
        let t1 = combine_datetime_and_subsec("2024:01:01 10:00:00", Some("1")).expect("parses");
        let t2 = combine_datetime_and_subsec("2024:01:01 10:00:00", Some("5")).expect("parses");
        assert!(t1 < t2, "same-second burst shots should still order by subsecond");
    }

    #[test]
    fn capture_timestamp_without_subsec_still_parses() {
        let t1 = combine_datetime_and_subsec("2024:01:01 10:00:00", None).expect("parses");
        let t2 = combine_datetime_and_subsec("2024:01:01 10:00:01", None).expect("parses");
        assert_eq!(t2 - t1, 1000, "one second apart should be 1000ms apart");
    }

    #[test]
    fn malformed_datetime_falls_back_gracefully() {
        assert_eq!(combine_datetime_and_subsec("not-a-date", None), None);
    }

    #[test]
    fn focal_plane_resolution_derives_a_plausible_aps_c_sized_sensor() {
        // A common mirrorless APS-C sensor: ~23.5mm x 15.6mm at 6000x4000px,
        // expressed as pixels-per-inch (unit 2) - values chosen to round-trip
        // back to roughly that sensor size.
        let (w_mm, h_mm) = sensor_size_from_focal_plane_resolution(6000, 4000, 6417.0, 6417.0, Some(2)).expect("derives a size");
        assert!((w_mm - 23.75).abs() < 0.5, "width should be ~23.5-24mm, got {w_mm}");
        assert!((h_mm - 15.8).abs() < 0.5, "height should be ~15.6-16mm, got {h_mm}");
    }

    #[test]
    fn focal_plane_resolution_rejects_unrecognized_unit() {
        // Unit 4 isn't a standard EXIF FocalPlaneResolutionUnit value - don't guess.
        assert_eq!(sensor_size_from_focal_plane_resolution(6000, 4000, 6417.0, 6417.0, Some(4)), None);
        assert_eq!(sensor_size_from_focal_plane_resolution(6000, 4000, 6417.0, 6417.0, None), None);
    }

    #[test]
    fn focal_plane_resolution_rejects_non_positive_resolution() {
        assert_eq!(sensor_size_from_focal_plane_resolution(6000, 4000, 0.0, 6417.0, Some(2)), None);
        assert_eq!(sensor_size_from_focal_plane_resolution(6000, 4000, 6417.0, -1.0, Some(2)), None);
    }
}
