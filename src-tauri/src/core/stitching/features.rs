//! ORB feature detection on downsampled grayscale images (Quick Stitch spec 2c):
//! a binary-descriptor detector, ~10x faster than SIFT/SURF and royalty-free.

use opencv::core::{AlgorithmHint, KeyPoint, KeyPointTraitConst, Mat, Point2f, Vector};
use opencv::features2d::{Feature2DTrait, ORB};
use opencv::{imgproc, Result};

/// `keypoints` is plain `Point2f` coordinates (not the ORB detector's
/// `Vector<KeyPoint>` boxed handles) specifically so `ImageFeatures` is
/// `Sync` - matching runs one pair per worker thread, each reading two
/// `ImageFeatures` shared by reference, and `KeyPoint`'s underlying C++
/// handle isn't `Sync` (only `Send`).
pub struct ImageFeatures {
    pub keypoints: Vec<Point2f>,
    pub descriptors: Mat,
}

/// Detects ORB keypoints + descriptors on `image` (converted to grayscale first).
pub fn detect(image: &Mat) -> Result<ImageFeatures> {
    let mut gray = Mat::default();
    imgproc::cvt_color(image, &mut gray, imgproc::COLOR_BGR2GRAY, 0, AlgorithmHint::ALGO_HINT_DEFAULT)?;

    let mut orb = ORB::create_def()?;
    let mut keypoints: Vector<KeyPoint> = Vector::new();
    let mut descriptors = Mat::default();
    let mask = Mat::default();
    orb.detect_and_compute_def(&gray, &mask, &mut keypoints, &mut descriptors)?;

    let keypoints = keypoints.iter().map(|kp| kp.pt()).collect();
    Ok(ImageFeatures { keypoints, descriptors })
}

#[cfg(test)]
mod tests {
    use super::*;
    use opencv::core::{Scalar, CV_8UC3};
    use opencv::prelude::*;

    #[test]
    fn detects_keypoints_on_a_textured_image() {
        // A checkerboard has plenty of corners for ORB to find; a blank image
        // should yield essentially none.
        let mut img = Mat::new_rows_cols_with_default(200, 200, CV_8UC3, Scalar::all(128.0)).unwrap();
        for y in (0..200).step_by(20) {
            for x in (0..200).step_by(20) {
                if (x / 20 + y / 20) % 2 == 0 {
                    imgproc::rectangle(
                        &mut img,
                        opencv::core::Rect::new(x, y, 20, 20),
                        Scalar::all(0.0),
                        -1,
                        imgproc::LINE_8,
                        0,
                    )
                    .unwrap();
                }
            }
        }

        let features = detect(&img).expect("detect");
        assert!(!features.keypoints.is_empty(), "checkerboard should have detectable corners");
        assert_eq!(features.descriptors.rows() as usize, features.keypoints.len());

        let blank = Mat::new_rows_cols_with_default(200, 200, CV_8UC3, Scalar::all(128.0)).unwrap();
        let blank_features = detect(&blank).expect("detect blank");
        assert!(
            blank_features.keypoints.len() < features.keypoints.len(),
            "flat image should yield far fewer keypoints than a textured one"
        );
    }
}
