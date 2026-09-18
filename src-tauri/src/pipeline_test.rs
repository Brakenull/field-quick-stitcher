//! End-to-end pipeline test using synthetic JPEGs with hand-built EXIF (GPS,
//! FocalLength, FocalLengthIn35mmFilm, pixel dims, embedded thumbnail) and a
//! hand-built DJI `drone-dji` XMP packet (RelativeAltitude, GimbalYawDegree) -
//! there's no real drone dataset available in this environment, so this fixture
//! generator stands in for one to exercise the full parse -> footprint -> overlap
//! pipeline without mocking any of the pipeline's own code.
#![cfg(test)]

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use exif::experimental::Writer as ExifWriter;
use exif::{Field, In, Rational, Tag, Value};
use image::codecs::jpeg::JpegEncoder;
use image::{ImageBuffer, Rgb};

use crate::core::{blur_detector, geometry, metadata_parser, overlap_engine};

const XMP_SIGNATURE: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";

struct FixtureSpec {
    file_name: &'static str,
    lat: f64,
    lon: f64,
    relative_altitude_m: f64,
    yaw_deg: f64,
    capture_time: &'static str,
    sharp_thumbnail: bool,
    /// `false` omits the GPS EXIF tags entirely, simulating a GPS dropout.
    has_gps: bool,
}

fn decimal_to_dms(decimal: f64) -> [Rational; 3] {
    let abs = decimal.abs();
    let deg = abs.floor();
    let min_full = (abs - deg) * 60.0;
    let min = min_full.floor();
    let sec = (min_full - min) * 60.0;
    [
        Rational { num: deg as u32, denom: 1 },
        Rational { num: min as u32, denom: 1 },
        Rational { num: (sec * 1000.0).round() as u32, denom: 1000 },
    ]
}

fn encode_jpeg(width: u32, height: u32, sharp: bool) -> Vec<u8> {
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(width, height, |x, y| {
        if sharp {
            if (x + y) % 2 == 0 {
                Rgb([10u8, 10, 10])
            } else {
                Rgb([245u8, 245, 245])
            }
        } else {
            Rgb([128u8, 128, 128])
        }
    });
    let mut bytes = Vec::new();
    JpegEncoder::new(&mut Cursor::new(&mut bytes))
        .encode_image(&img)
        .expect("encode jpeg");
    bytes
}

fn build_fixture_jpeg(spec: &FixtureSpec) -> Vec<u8> {
    let thumb = encode_jpeg(96, 72, spec.sharp_thumbnail);
    let base = encode_jpeg(16, 16, true);

    let mut fields = Vec::new();
    if spec.has_gps {
        let lat_ref = if spec.lat >= 0.0 { b"N".to_vec() } else { b"S".to_vec() };
        let lon_ref = if spec.lon >= 0.0 { b"E".to_vec() } else { b"W".to_vec() };
        let lat_dms = decimal_to_dms(spec.lat);
        let lon_dms = decimal_to_dms(spec.lon);
        fields.push(Field { tag: Tag::GPSLatitudeRef, ifd_num: In::PRIMARY, value: Value::Ascii(vec![lat_ref]) });
        fields.push(Field { tag: Tag::GPSLatitude, ifd_num: In::PRIMARY, value: Value::Rational(lat_dms.to_vec()) });
        fields.push(Field { tag: Tag::GPSLongitudeRef, ifd_num: In::PRIMARY, value: Value::Ascii(vec![lon_ref]) });
        fields.push(Field { tag: Tag::GPSLongitude, ifd_num: In::PRIMARY, value: Value::Rational(lon_dms.to_vec()) });
    }
    fields.extend(vec![
        Field {
            tag: Tag::FocalLength,
            ifd_num: In::PRIMARY,
            value: Value::Rational(vec![Rational { num: 88, denom: 10 }]), // 8.8mm
        },
        Field {
            tag: Tag::FocalLengthIn35mmFilm,
            ifd_num: In::PRIMARY,
            value: Value::Short(vec![24]),
        },
        Field {
            tag: Tag::Model,
            ifd_num: In::PRIMARY,
            value: Value::Ascii(vec![b"FC6310".to_vec()]),
        },
        Field { tag: Tag::PixelXDimension, ifd_num: In::PRIMARY, value: Value::Long(vec![5472]) },
        Field { tag: Tag::PixelYDimension, ifd_num: In::PRIMARY, value: Value::Long(vec![3648]) },
        Field {
            tag: Tag::DateTimeOriginal,
            ifd_num: In::PRIMARY,
            value: Value::Ascii(vec![spec.capture_time.as_bytes().to_vec()]),
        },
    ]);

    let mut writer = ExifWriter::new();
    for f in &fields {
        writer.push_field(f);
    }
    writer.set_jpeg(&thumb, In::THUMBNAIL);
    let mut tiff_buf = Cursor::new(Vec::new());
    writer.write(&mut tiff_buf, false).expect("write exif");
    let tiff_bytes = tiff_buf.into_inner();

    let mut exif_app1 = Vec::new();
    exif_app1.extend_from_slice(&[0xFF, 0xE1]);
    let exif_len = (2 + 6 + tiff_bytes.len()) as u16;
    exif_app1.extend_from_slice(&exif_len.to_be_bytes());
    exif_app1.extend_from_slice(b"Exif\0\0");
    exif_app1.extend_from_slice(&tiff_bytes);

    let xmp_xml = format!(
        concat!(
            r#"<?xpacket begin="" id="w"?><x:xmpmeta xmlns:x="adobe:ns:meta/">"#,
            r#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">"#,
            r#"<rdf:Description rdf:about="" xmlns:drone-dji="http://www.dji.com/drone-dji/1.0/" "#,
            r#"drone-dji:RelativeAltitude="+{:.2}" drone-dji:GimbalYawDegree="+{:.2}"/>"#,
            r#"</rdf:RDF></x:xmpmeta><?xpacket end="w"?>"#
        ),
        spec.relative_altitude_m, spec.yaw_deg
    );
    let mut xmp_app1 = Vec::new();
    xmp_app1.extend_from_slice(&[0xFF, 0xE1]);
    let xmp_len = (2 + XMP_SIGNATURE.len() + xmp_xml.len()) as u16;
    xmp_app1.extend_from_slice(&xmp_len.to_be_bytes());
    xmp_app1.extend_from_slice(XMP_SIGNATURE);
    xmp_app1.extend_from_slice(xmp_xml.as_bytes());

    let mut out = Vec::new();
    out.extend_from_slice(&[0xFF, 0xD8]);
    out.extend_from_slice(&exif_app1);
    out.extend_from_slice(&xmp_app1);
    out.extend_from_slice(&base[2..]);
    out
}

fn write_fixture_dir(specs: &[FixtureSpec]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for spec in specs {
        let bytes = build_fixture_jpeg(spec);
        fs::write(dir.path().join(spec.file_name), bytes).expect("write fixture");
    }
    dir
}

/// Mirrors `commands::inspect::parse_one`'s per-photo pipeline without depending on
/// a Tauri `AppHandle`.
fn parse_one_for_test(path: &Path) -> crate::models::photo_meta::PhotoMeta {
    let mut meta = metadata_parser::parse_photo(path);
    let blur = blur_detector::analyze(path);
    meta.is_blurry = blur.is_blurry;
    meta.blur_score = blur.score;

    if let (Some(lat), Some(lon), Some(rel_alt), Some(yaw), Some(focal), Some(sw), Some(sh)) =
        (meta.lat, meta.lon, meta.relative_altitude, meta.yaw_deg, meta.focal_mm, meta.sensor_width_mm, meta.sensor_height_mm)
    {
        meta.footprint = geometry::compute_footprint(geometry::FootprintInput {
            lat,
            lon,
            relative_altitude_m: rel_alt,
            focal_mm: focal,
            sensor_width_mm: sw,
            sensor_height_mm: sh,
            yaw_deg: yaw,
        });
    }
    meta
}

#[test]
fn full_pipeline_on_synthetic_flight() {
    // A 3x2 grid of shots ~15m apart (tight overlap) plus one shot far away on its
    // own (guaranteed low-overlap / gap), and one deliberately blurry thumbnail.
    let mut specs = Vec::new();
    let mut i = 0;
    for row in 0..2 {
        for col in 0..3 {
            let lat = 10.000_000 + row as f64 * 0.00014; // ~15.5m
            let lon = 106.000_000 + col as f64 * 0.00014;
            specs.push(FixtureSpec {
                file_name: Box::leak(format!("grid_{i:02}.jpg").into_boxed_str()),
                lat,
                lon,
                relative_altitude_m: 80.0,
                yaw_deg: 0.0,
                capture_time: Box::leak(format!("2024:01:01 10:00:{i:02}").into_boxed_str()),
                sharp_thumbnail: i != 1, // make the 2nd photo blurry
                has_gps: true,
            });
            i += 1;
        }
    }
    specs.push(FixtureSpec {
        file_name: "isolated.jpg",
        lat: 10.005_000,
        lon: 106.005_000,
        relative_altitude_m: 80.0,
        yaw_deg: 45.0,
        capture_time: "2024:01:01 10:01:00",
        sharp_thumbnail: true,
        has_gps: true,
    });

    let dir = write_fixture_dir(&specs);
    let paths: Vec<PathBuf> = fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(paths.len(), specs.len());

    let photos: Vec<_> = paths.iter().map(|p| parse_one_for_test(p)).collect();

    for p in &photos {
        let lat = p.lat.expect("fixture always has GPS");
        let lon = p.lon.expect("fixture always has GPS");
        assert!(lat > 9.0 && lat < 11.0, "lat parsed: {lat}");
        assert!(lon > 105.0 && lon < 107.0, "lon parsed: {lon}");
        assert_eq!(p.relative_altitude, Some(80.0));
        assert!(p.warnings.iter().all(|w| w != "missing_gps"));
        assert!((p.sensor_width_mm.unwrap() - 13.2).abs() < 1e-6, "sensor width via 35mm-equiv calc");
        assert!(p.footprint.is_some(), "footprint should compute with full metadata");
        assert_eq!(p.footprint.as_ref().unwrap().len(), 5);
    }

    let blurry: Vec<_> = photos.iter().filter(|p| p.is_blurry).collect();
    assert_eq!(blurry.len(), 1, "exactly one fixture used a flat (blurry) thumbnail");

    let footprints: Vec<_> = photos.iter().filter_map(|p| p.footprint.clone()).collect();
    let overlap = overlap_engine::analyze(&footprints, 3.0);
    assert!(!overlap.heatmap.is_empty());
    let max_overlap = overlap
        .heatmap
        .iter()
        .filter_map(|f| f.properties.get("overlapCount").and_then(|v| v.as_u64()))
        .max()
        .unwrap();
    assert!(max_overlap >= 3, "tightly packed grid should overlap in the middle, got max {max_overlap}");
    assert!(!overlap.gaps.is_empty(), "the isolated far-away photo should show up as a low-coverage gap");
}

/// A GPS dropout (no GPSLatitude/GPSLongitude tags at all) should still parse
/// to a usable `PhotoMeta` - `lat`/`lon` are `None`, the `missing_gps`
/// warning is set, and no footprint is computed - rather than the parse step
/// itself failing. `inspect_directory` (not exercised here - see this file's
/// module doc comment) is what decides to keep such a photo in the result;
/// this test covers the parsing foundation that decision depends on.
#[test]
fn photo_without_gps_parses_with_none_coordinates_and_no_footprint() {
    let spec = FixtureSpec {
        file_name: "no_gps.jpg",
        lat: 10.0,
        lon: 106.0,
        relative_altitude_m: 80.0,
        yaw_deg: 0.0,
        capture_time: "2024:01:01 10:00:00",
        sharp_thumbnail: true,
        has_gps: false,
    };
    let dir = write_fixture_dir(&[spec]);
    let path = fs::read_dir(dir.path()).unwrap().next().unwrap().unwrap().path();

    let photo = parse_one_for_test(&path);
    assert_eq!(photo.lat, None);
    assert_eq!(photo.lon, None);
    assert!(photo.warnings.iter().any(|w| w == "missing_gps"));
    assert!(photo.footprint.is_none(), "a photo with no GPS can't have a footprint");
    // Everything else should still parse normally - a GPS dropout shouldn't
    // corrupt or block the rest of the metadata read.
    assert_eq!(photo.relative_altitude, Some(80.0));
}
