//! Fast blur detection: decodes only the small thumbnail already embedded in a
//! JPEG's EXIF block (never the full 20-45MP image) and flags it blurry when the
//! variance of its Laplacian response falls below a threshold.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use exif::{In, Reader, Tag, Value};
use image::GrayImage;

/// Laplacian-variance score below this is flagged as blurry/shaken. Tuned for the
/// small (~160-640px) embedded thumbnails, not full-resolution images.
pub const DEFAULT_BLUR_THRESHOLD: f64 = 100.0;

pub struct BlurResult {
    pub is_blurry: bool,
    pub score: Option<f64>,
}

pub fn analyze(path: &Path) -> BlurResult {
    analyze_with_threshold(path, DEFAULT_BLUR_THRESHOLD)
}

pub fn analyze_with_threshold(path: &Path, threshold: f64) -> BlurResult {
    let score = thumbnail_laplacian_variance(path);
    match score {
        Some(s) => BlurResult {
            is_blurry: s < threshold,
            score: Some(s),
        },
        None => BlurResult {
            is_blurry: false,
            score: None,
        },
    }
}

fn thumbnail_laplacian_variance(path: &Path) -> Option<f64> {
    let file = File::open(path).ok()?;
    let exif = Reader::new().read_from_container(&mut BufReader::new(file)).ok()?;
    let thumb_bytes = extract_thumbnail(&exif)?;
    let img = image::load_from_memory(thumb_bytes).ok()?;
    let gray = img.to_luma8();
    Some(laplacian_variance(&gray))
}

/// The embedded IFD1 thumbnail isn't exposed as a convenience method by `kamadak-exif`;
/// it's stored as raw bytes in the TIFF buffer, located via the
/// JPEGInterchangeFormat(Length) offset/length pair on the THUMBNAIL IFD.
fn extract_thumbnail<'a>(exif: &'a exif::Exif) -> Option<&'a [u8]> {
    let offset = match &exif.get_field(Tag::JPEGInterchangeFormat, In::THUMBNAIL)?.value {
        Value::Long(v) => *v.first()? as usize,
        _ => return None,
    };
    let length = match &exif.get_field(Tag::JPEGInterchangeFormatLength, In::THUMBNAIL)?.value {
        Value::Long(v) => *v.first()? as usize,
        _ => return None,
    };
    exif.buf().get(offset..offset + length)
}

fn laplacian_variance(img: &GrayImage) -> f64 {
    let (w, h) = img.dimensions();
    if w < 3 || h < 3 {
        return 0.0;
    }

    let mut responses: Vec<f64> = Vec::with_capacity(((w - 2) * (h - 2)) as usize);
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let center = img.get_pixel(x, y)[0] as i32;
            let up = img.get_pixel(x, y - 1)[0] as i32;
            let down = img.get_pixel(x, y + 1)[0] as i32;
            let left = img.get_pixel(x - 1, y)[0] as i32;
            let right = img.get_pixel(x + 1, y)[0] as i32;
            let lap = (up + down + left + right - 4 * center) as f64;
            responses.push(lap);
        }
    }

    let n = responses.len() as f64;
    let mean = responses.iter().sum::<f64>() / n;
    responses.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Luma};

    #[test]
    fn flat_image_has_zero_variance() {
        let img: GrayImage = ImageBuffer::from_pixel(32, 32, Luma([128u8]));
        assert_eq!(laplacian_variance(&img), 0.0);
    }

    #[test]
    fn checkerboard_has_high_variance() {
        let img: GrayImage = ImageBuffer::from_fn(32, 32, |x, y| {
            if (x + y) % 2 == 0 {
                Luma([0u8])
            } else {
                Luma([255u8])
            }
        });
        assert!(laplacian_variance(&img) > DEFAULT_BLUR_THRESHOLD * 10.0);
    }

    #[test]
    fn tiny_image_returns_zero_not_panic() {
        let img: GrayImage = ImageBuffer::from_pixel(1, 1, Luma([10u8]));
        assert_eq!(laplacian_variance(&img), 0.0);
    }
}
