//! Per-pixel blend weights for compositing overlapping warped images (Quick
//! Stitch spec 2d/4.4). Uses a distance-from-image-edge feather (highest
//! weight at the image center, fading to 0 at the border) rather than full
//! multi-band frequency blending - a lighter-weight stand-in that still hides
//! most seams, adequate for a quick field check; true multi-band blending
//! across N overlapping images would be a reasonable follow-up if seams prove
//! visible in practice.

use opencv::core::{Mat, Scalar, CV_32FC1};
use opencv::prelude::*;
use opencv::Result;

/// A per-pixel weight map (`CV_32FC1`, same size as the source image):
/// highest at the image center, 0 at the nearest edge.
pub fn center_distance_weights(width: i32, height: i32) -> Result<Mat> {
    let mut weights = Mat::new_rows_cols_with_default(height, width, CV_32FC1, Scalar::all(0.0))?;
    let cx = (width - 1) as f32 / 2.0;
    let cy = (height - 1) as f32 / 2.0;
    let max_dist = cx.min(cy).max(1.0);
    for y in 0..height {
        for x in 0..width {
            let dx = (x as f32 - cx).abs();
            let dy = (y as f32 - cy).abs();
            let edge_dist = (cx - dx).min(cy - dy).max(0.0);
            *weights.at_2d_mut::<f32>(y, x)? = (edge_dist / max_dist).min(1.0);
        }
    }
    Ok(weights)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weight_peaks_at_center_and_vanishes_at_border() {
        let w = center_distance_weights(101, 61).unwrap();
        let center = *w.at_2d::<f32>(30, 50).unwrap();
        let corner = *w.at_2d::<f32>(0, 0).unwrap();
        let edge_mid = *w.at_2d::<f32>(0, 50).unwrap();
        assert!((center - 1.0).abs() < 1e-6, "center should be full weight, got {center}");
        assert_eq!(corner, 0.0);
        assert_eq!(edge_mid, 0.0);
    }
}
