//! Dev-only tool: writes a folder of synthetic JPEGs (fabricated GPS/altitude/
//! gimbal-yaw EXIF+XMP, DJI-style) for manually exercising the app UI without a
//! real drone dataset. Usage: `cargo run --example gen_fixtures -- <output_dir>`

use std::env;
use std::fs;
use std::io::Cursor;
use std::path::PathBuf;

use exif::experimental::Writer as ExifWriter;
use exif::{Field, In, Rational, Tag, Value};
use image::codecs::jpeg::JpegEncoder;
use image::{ImageBuffer, Rgb};

const XMP_SIGNATURE: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";

struct FixtureSpec {
    file_name: String,
    lat: f64,
    lon: f64,
    relative_altitude_m: f64,
    yaw_deg: f64,
    capture_time: String,
    sharp_thumbnail: bool,
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
    JpegEncoder::new(&mut Cursor::new(&mut bytes)).encode_image(&img).expect("encode jpeg");
    bytes
}

fn build_fixture_jpeg(spec: &FixtureSpec) -> Vec<u8> {
    let thumb = encode_jpeg(96, 72, spec.sharp_thumbnail);
    let base = encode_jpeg(16, 16, true);

    let lat_ref = if spec.lat >= 0.0 { b"N".to_vec() } else { b"S".to_vec() };
    let lon_ref = if spec.lon >= 0.0 { b"E".to_vec() } else { b"W".to_vec() };
    let lat_dms = decimal_to_dms(spec.lat);
    let lon_dms = decimal_to_dms(spec.lon);

    let fields = vec![
        Field { tag: Tag::GPSLatitudeRef, ifd_num: In::PRIMARY, value: Value::Ascii(vec![lat_ref]) },
        Field { tag: Tag::GPSLatitude, ifd_num: In::PRIMARY, value: Value::Rational(lat_dms.to_vec()) },
        Field { tag: Tag::GPSLongitudeRef, ifd_num: In::PRIMARY, value: Value::Ascii(vec![lon_ref]) },
        Field { tag: Tag::GPSLongitude, ifd_num: In::PRIMARY, value: Value::Rational(lon_dms.to_vec()) },
        Field {
            tag: Tag::FocalLength,
            ifd_num: In::PRIMARY,
            value: Value::Rational(vec![Rational { num: 88, denom: 10 }]),
        },
        Field { tag: Tag::FocalLengthIn35mmFilm, ifd_num: In::PRIMARY, value: Value::Short(vec![24]) },
        Field { tag: Tag::Model, ifd_num: In::PRIMARY, value: Value::Ascii(vec![b"FC6310".to_vec()]) },
        Field { tag: Tag::PixelXDimension, ifd_num: In::PRIMARY, value: Value::Long(vec![5472]) },
        Field { tag: Tag::PixelYDimension, ifd_num: In::PRIMARY, value: Value::Long(vec![3648]) },
        Field {
            tag: Tag::DateTimeOriginal,
            ifd_num: In::PRIMARY,
            value: Value::Ascii(vec![spec.capture_time.as_bytes().to_vec()]),
        },
    ];

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

fn main() {
    let out_dir: PathBuf = env::args().nth(1).expect("usage: gen_fixtures <output_dir>").into();
    fs::create_dir_all(&out_dir).expect("create output dir");

    let mut specs = Vec::new();
    let mut i = 0;
    // Dense 4x4 grid with tight overlap (~12m spacing).
    for row in 0..4 {
        for col in 0..4 {
            let lat = 10.762_000 + row as f64 * 0.00011;
            let lon = 106.660_000 + col as f64 * 0.00011;
            specs.push(FixtureSpec {
                file_name: format!("DJI_{i:04}.jpg"),
                lat,
                lon,
                relative_altitude_m: 60.0,
                yaw_deg: 0.0,
                capture_time: format!("2024:03:15 08:{:02}:{:02}", i / 60, i % 60),
                sharp_thumbnail: i % 7 != 3, // scatter a few blurry ones in
            });
            i += 1;
        }
    }
    // A gap: skip a row in the middle of the grid (rows already 0..4, so shift a
    // block far away instead to guarantee a low-overlap zone).
    for col in 0..2 {
        let lat = 10.762_600;
        let lon = 106.660_400 + col as f64 * 0.00011;
        specs.push(FixtureSpec {
            file_name: format!("DJI_{i:04}.jpg"),
            lat,
            lon,
            relative_altitude_m: 60.0,
            yaw_deg: 90.0,
            capture_time: format!("2024:03:15 08:{:02}:{:02}", i / 60, i % 60),
            sharp_thumbnail: true,
        });
        i += 1;
    }

    for spec in &specs {
        let bytes = build_fixture_jpeg(spec);
        let path = out_dir.join(&spec.file_name);
        fs::write(&path, bytes).expect("write fixture");
    }

    println!("Wrote {} synthetic photos to {}", specs.len(), out_dir.display());
}
