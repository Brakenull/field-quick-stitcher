//! Reads GPS / altitude / gimbal-yaw / camera metadata from a JPEG without decoding
//! the full image: standard EXIF tags via `kamadak-exif`, plus DJI's `drone-dji`
//! XMP packet (RelativeAltitude, GimbalYawDegree, ...) via a lightweight manual
//! JPEG-segment scan that stops before the compressed scan data.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use exif::{In, Reader, Tag, Value};

use crate::models::photo_meta::PhotoMeta;

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
    match (focal_mm, focal_35mm, image_width_px, image_height_px) {
        (Some(f), Some(f35), Some(w), Some(h)) if f35 > 0 => {
            let sw = f * 36.0 / f35 as f64;
            let sh = sw * (h as f64 / w as f64);
            sensor_width_mm = Some(sw);
            sensor_height_mm = Some(sh);
        }
        _ => {
            if let Some(model) = &model {
                if let Some((_, w, h)) = CAMERA_DB.iter().find(|(needle, _, _)| model.contains(needle)) {
                    sensor_width_mm = Some(*w);
                    sensor_height_mm = Some(*h);
                    warnings.push("used_camera_db_fallback".to_string());
                }
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

    let xmp = read_xmp_packet(path);
    let relative_altitude = xmp.as_deref().and_then(|x| xmp_attr_f64(x, "drone-dji:RelativeAltitude"));
    if relative_altitude.is_none() {
        warnings.push("missing_altitude".to_string());
    }
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
        lat: lat.unwrap_or(0.0),
        lon: lon.unwrap_or(0.0),
        relative_altitude,
        yaw_deg,
        focal_mm,
        sensor_width_mm,
        sensor_height_mm,
        image_width_px,
        image_height_px,
        footprint: None,
        capture_time,
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
}
