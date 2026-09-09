//! Guided descriptor matching between GPS-neighbor photo pairs only (Quick
//! Stitch spec 2b/4.2) - never all-pairs. Uses Hamming distance (ORB is a
//! binary descriptor) plus Lowe's ratio test to reject ambiguous matches.

use opencv::core::{DMatch, Vector, NORM_HAMMING};
use opencv::features2d::{BFMatcher, DescriptorMatcherTraitConst};
use opencv::prelude::*;
use opencv::Result;

use super::features::ImageFeatures;

/// Lowe's ratio test threshold: a match is kept only if the best candidate is
/// meaningfully closer than the second-best (rejects ambiguous matches).
const RATIO_THRESHOLD: f32 = 0.75;

/// Matches `a`'s descriptors against `b`'s, returning the accepted `DMatch`es
/// (each `query_idx`/`train_idx` indexes into `a`/`b`'s keypoints respectively).
pub fn match_pair(a: &ImageFeatures, b: &ImageFeatures) -> Result<Vec<DMatch>> {
    if a.descriptors.rows() == 0 || b.descriptors.rows() == 0 {
        return Ok(Vec::new());
    }

    let matcher = BFMatcher::create(NORM_HAMMING, false)?;
    let mut knn_matches: Vector<Vector<DMatch>> = Vector::new();
    matcher.knn_train_match_def(&a.descriptors, &b.descriptors, &mut knn_matches, 2)?;

    let mut good = Vec::new();
    for pair in &knn_matches {
        if pair.len() < 2 {
            continue;
        }
        let best = pair.get(0)?;
        let second = pair.get(1)?;
        if best.distance < RATIO_THRESHOLD * second.distance {
            good.push(best);
        }
    }
    Ok(good)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::stitching::features;
    use opencv::core::{Scalar, Vec3b, CV_8UC3};

    /// Pseudo-random noise texture: unlike a checkerboard, every patch is
    /// locally unique, so ORB matches are unambiguous - a checkerboard's
    /// repeating corners make Lowe's ratio test reject nearly everything even
    /// between identical images, which isn't the thing under test here.
    fn noisy_image(size: i32) -> opencv::core::Mat {
        let mut img = opencv::core::Mat::new_rows_cols_with_default(size, size, CV_8UC3, Scalar::all(128.0)).unwrap();
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
    fn matches_identical_images_almost_completely() {
        let img = noisy_image(240);
        let a = features::detect(&img).unwrap();
        let b = features::detect(&img).unwrap();

        let matches = match_pair(&a, &b).unwrap();
        assert!(!matches.is_empty(), "identical images should produce matches");
        // Every accepted match on identical images should be a near-perfect
        // (zero or near-zero Hamming distance) correspondence.
        assert!(matches.iter().all(|m| m.distance < 5.0), "identical images should match near-exactly");
    }

    #[test]
    fn empty_descriptors_produce_no_matches() {
        let blank = opencv::core::Mat::new_rows_cols_with_default(240, 240, CV_8UC3, Scalar::all(128.0)).unwrap();
        let a = features::detect(&blank).unwrap();
        let img = noisy_image(240);
        let b = features::detect(&img).unwrap();

        let matches = match_pair(&a, &b).unwrap();
        assert!(matches.is_empty());
    }
}
