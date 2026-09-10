//! RANSAC homography estimation between matched neighbor pairs (Quick Stitch
//! spec 4.2): finds H such that `p_j ~= H * p_i` for a nadir (flat-ground)
//! pair of overlapping shots.

use opencv::calib3d;
use opencv::core::{Mat, Point2f, Vector};
use opencv::prelude::*;
use opencv::Result;

/// Minimum matches required to even attempt a homography fit.
const MIN_MATCHES: usize = 8;
/// RANSAC reprojection error threshold, in downsampled-image pixels.
const RANSAC_REPROJ_THRESHOLD: f64 = 3.0;
/// Minimum fraction of matches RANSAC must accept as inliers to trust the edge.
const MIN_INLIER_RATIO: f64 = 0.3;

pub struct HomographyEdge {
    /// 3x3 homography mapping points from image `a` into image `b`'s frame.
    pub h: Mat,
    pub inlier_count: usize,
    pub match_count: usize,
}

/// Estimates `H` such that `b_point ~= H * a_point` for the given matched
/// point pairs (`a`'s pixel coords, `b`'s pixel coords), returning `None` if
/// there isn't enough evidence to trust the fit (too few matches/inliers).
/// Deliberately generic over how the correspondences were found (ORB+BFMatcher
/// or a fused ONNX extractor+matcher both just produce point pairs) rather
/// than taking a feature-detector-specific type.
pub fn estimate(matches: &[(Point2f, Point2f)]) -> Result<Option<HomographyEdge>> {
    if matches.len() < MIN_MATCHES {
        return Ok(None);
    }

    let mut src = Vector::<Point2f>::new();
    let mut dst = Vector::<Point2f>::new();
    for &(a, b) in matches {
        src.push(a);
        dst.push(b);
    }

    let mut mask = Mat::default();
    let h = calib3d::find_homography(&src, &dst, &mut mask, calib3d::RANSAC, RANSAC_REPROJ_THRESHOLD)?;
    if h.empty() {
        return Ok(None);
    }

    let inlier_count = count_inliers(&mask)?;
    if inlier_count < MIN_MATCHES || (inlier_count as f64) < MIN_INLIER_RATIO * matches.len() as f64 {
        return Ok(None);
    }

    Ok(Some(HomographyEdge { h, inlier_count, match_count: matches.len() }))
}

fn count_inliers(mask: &Mat) -> Result<usize> {
    let mut count = 0usize;
    for row in 0..mask.rows() {
        if *mask.at::<u8>(row)? != 0 {
            count += 1;
        }
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::stitching::features::ImageFeatures;
    use crate::core::stitching::{features, matching};
    use opencv::core::{DMatch, Scalar, Vec3b, CV_8UC3};

    fn to_point_pairs(a: &ImageFeatures, b: &ImageFeatures, matches: &[DMatch]) -> Vec<(Point2f, Point2f)> {
        matches.iter().map(|m| (a.keypoints[m.query_idx as usize], b.keypoints[m.train_idx as usize])).collect()
    }

    /// Pseudo-random noise texture: gives ORB unambiguous, unique local
    /// patches (a checkerboard's repeating corners would make Lowe's ratio
    /// test reject nearly everything, even between identical images).
    fn noisy_image(size: i32) -> Mat {
        let mut img = Mat::new_rows_cols_with_default(size, size, CV_8UC3, Scalar::all(128.0)).unwrap();
        for y in 0..size {
            for x in 0..size {
                let h = (x as u32).wrapping_mul(374_761_393) ^ (y as u32).wrapping_mul(668_265_263);
                let v = (h % 256) as u8;
                *img.at_2d_mut::<Vec3b>(y, x).unwrap() = Vec3b::from([v, v.wrapping_add(64), v.wrapping_add(128)]);
            }
        }
        img
    }

    #[test]
    fn identical_images_produce_a_near_identity_homography() {
        let img = noisy_image(240);
        let a = features::detect(&img).unwrap();
        let b = features::detect(&img).unwrap();
        let matches = matching::match_pair(&a, &b).unwrap();
        let pairs = to_point_pairs(&a, &b, &matches);

        let edge = estimate(&pairs).unwrap().expect("should find a confident homography");
        assert!(edge.inlier_count >= MIN_MATCHES);

        // H should be close to identity (up to homogeneous scale): check the
        // diagonal dominates and off-diagonal translation terms are small.
        let h00 = *edge.h.at_2d::<f64>(0, 0).unwrap();
        let h22 = *edge.h.at_2d::<f64>(2, 2).unwrap();
        assert!((h00 / h22 - 1.0).abs() < 0.1, "H[0,0]/H[2,2] should be ~1 for identical images");
    }

    #[test]
    fn too_few_matches_returns_none() {
        let img = noisy_image(240);
        let a = features::detect(&img).unwrap();
        let blank = Mat::new_rows_cols_with_default(240, 240, CV_8UC3, Scalar::all(128.0)).unwrap();
        let b = features::detect(&blank).unwrap();
        let matches = matching::match_pair(&a, &b).unwrap();
        let pairs = to_point_pairs(&a, &b, &matches);

        assert!(estimate(&pairs).unwrap().is_none());
    }
}
