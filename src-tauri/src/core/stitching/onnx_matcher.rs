//! Fused feature extraction + matching for one neighbor pair, via ONNX
//! Runtime (Quick Stitch spec 2.3 concept - see `.claude/phase/quick-stitch.md`
//! and `.claude/phase/overview.md`). The bundled model's actual contract
//! differs from `quick-stitch.md`'s sample code: it isn't a separate
//! SuperPoint-extractor-then-LightGlue-matcher pair, it's a single end-to-end
//! graph (`resources/superpoint-ort.onnx`) that takes exactly two images as
//! one batch and directly outputs matched point correspondences - there is no
//! separate per-image descriptor to cache, so this replaces both `features.rs`
//! and `matching.rs` for a pair in one inference call.
//!
//! Runs on ONNX Runtime's **CPU** execution provider, not DirectML, despite
//! this machine having a usable GPU - deliberately, not an oversight.
//! Matching needs the *original, full-resolution* photos (see below), and at
//! that resolution DirectML on this Iris Xe reliably runs out of GPU memory
//! partway through the SuperPoint backbone (`8007000E`, a real ceiling -
//! confirmed with isolated single-process probes at 2000-3200px, not a
//! session-reuse artifact). The resolution ceiling that *does* fit in this
//! GPU's memory (~2000px) sits below the resolution floor the model needs for
//! usable matches (~0 matches at 1600-2400px vs 550+ at native res, see
//! below) - those two constraints don't overlap on this hardware, so GPU
//! dispatch for this specific fused model isn't viable here. CPU is slower
//! per pair but actually works. Revisit if a smaller/quantized model variant
//! becomes available, or per the switching-mechanism-for-later plan.
//!
//! Deliberately matches on the **original, full-resolution** photos rather
//! than `downsample.rs`'s ~1600px-long-edge cache the rest of the pipeline
//! uses: this model reliably finds 500+ confident matches between two real
//! overlapping drone photos at native resolution, but found essentially none
//! (0-1) at 1600-2000px, independent of resize interpolation (`INTER_AREA`
//! and `INTER_LINEAR` both failed identically) and independent of crop
//! content (tried real photos and multiple synthetic textures) - this
//! specific fused export just needs more pixels than classical ORB does. The
//! returned match coordinates are rescaled into `target_size_a`/
//! `target_size_b` (the *same* downsampled space `homography.rs`/
//! `pose_graph.rs`/`mosaic.rs` all already operate in) before being returned,
//! so nothing downstream needs to know matching happened at a different
//! resolution.
//!
//! `ort::Session::run` takes `&mut self`, so unlike `features::detect` (which
//! is stateless per call and safely fanned out across every rayon worker
//! thread), one loaded session is shared behind a `Mutex` here - inference
//! calls serialize, but loading the model once per process instead of once
//! per thread matters more, since the model is ~50MB and session construction
//! isn't cheap. Parallelism for the surrounding pipeline stage still comes
//! from image I/O and preprocessing happening outside the lock.

use std::path::Path;
use std::sync::Mutex;

use ndarray::Array4;
use opencv::core::Point2f;
use ort::session::Session;
use ort::value::TensorRef;

pub struct OnnxMatcher {
    session: Mutex<Session>,
}

/// One matched point pair, already rescaled into each photo's target
/// (downsampled) coordinate space and filtered by confidence (see
/// `MIN_MATCH_SCORE`).
pub struct PointMatch {
    pub a: Point2f,
    pub b: Point2f,
}

/// Minimum LightGlue match confidence to keep, mirroring `matching.rs`'s
/// Lowe-ratio filter for the ORB path - RANSAC in `homography.rs` still does
/// the real outlier rejection, this just discards the obviously-bad tail
/// before that (LightGlue's own scores range from near-0 for spurious pairs
/// to ~1.0 for confident ones, unlike ORB's Hamming distance).
const MIN_MATCH_SCORE: f32 = 0.2;

impl OnnxMatcher {
    pub fn load(model_path: &Path) -> Result<Self, String> {
        // CPU, not DirectML - see module doc comment for why.
        let session = Session::builder().map_err(|e| e.to_string())?.commit_from_file(model_path).map_err(|e| e.to_string())?;
        Ok(Self { session: Mutex::new(session) })
    }

    /// Runs the fused extract+match graph on the *original* photos at
    /// `path_a`/`path_b` (not a downsampled cache - see module doc comment),
    /// returning matched points rescaled into `target_size_a`/`target_size_b`
    /// (typically `image_sizes[i]`/`image_sizes[j]` from `downsample.rs`'s
    /// cache, so callers work in one consistent coordinate space regardless
    /// of what resolution matching itself ran at). The two images are
    /// stacked into one batch of 2 for inference, so they must share pixel
    /// dimensions - `path_b` is resized to `path_a`'s native size first if
    /// they don't (typical surveys share a camera/aspect ratio and never hit
    /// this).
    pub fn match_pair(
        &self,
        path_a: &Path,
        path_b: &Path,
        target_size_a: (f64, f64),
        target_size_b: (f64, f64),
    ) -> Result<Vec<PointMatch>, String> {
        let img_a = image::open(path_a).map_err(|e| e.to_string())?.to_luma8();
        let img_b_native = image::open(path_b).map_err(|e| e.to_string())?.to_luma8();
        let (wa, ha) = img_a.dimensions();
        let (wb, hb) = img_b_native.dimensions();

        let img_b = if (wb, hb) == (wa, ha) { img_b_native } else { image::imageops::resize(&img_b_native, wa, ha, image::imageops::FilterType::Triangle) };

        let mut tensor = Array4::<f32>::zeros((2, 1, ha as usize, wa as usize));
        for (x, y, p) in img_a.enumerate_pixels() {
            tensor[[0, 0, y as usize, x as usize]] = p[0] as f32 / 255.0;
        }
        for (x, y, p) in img_b.enumerate_pixels() {
            tensor[[1, 0, y as usize, x as usize]] = p[0] as f32 / 255.0;
        }

        // `outputs` borrows from the locked session, so the guard must stay
        // alive for as long as `outputs` is used - no separate scope block.
        let mut session = self.session.lock().map_err(|_| "ONNX session lock poisoned".to_string())?;
        let input = TensorRef::from_array_view(&tensor).map_err(|e| e.to_string())?;
        let outputs = session.run(ort::inputs!["images" => input]).map_err(|e| e.to_string())?;

        let (kp_shape, kp_data) = outputs["keypoints"].try_extract_tensor::<i64>().map_err(|e| e.to_string())?;
        let (m_shape, m_data) = outputs["matches"].try_extract_tensor::<i64>().map_err(|e| e.to_string())?;
        let (_, s_data) = outputs["mscores"].try_extract_tensor::<f32>().map_err(|e| e.to_string())?;

        let keypoints_per_image = kp_shape[1] as usize;
        let num_matches = m_shape[0] as usize;
        let keypoint_xy = |image_idx: usize, kp_idx: usize| -> (f32, f32) {
            let base = (image_idx * keypoints_per_image + kp_idx) * 2;
            (kp_data[base] as f32, kp_data[base + 1] as f32)
        };

        // Both images' keypoints come out of the model in the same (wa, ha)
        // batch-tensor frame (image b was resized into that frame above, if
        // it didn't already match), so a single scale factor per image - from
        // that shared frame straight to each image's own target size - is all
        // that's needed; there's no separate "b's native size" step to
        // reintroduce here.
        let (scale_ax, scale_ay) = (target_size_a.0 / wa as f64, target_size_a.1 / ha as f64);
        let (scale_bx, scale_by) = (target_size_b.0 / wa as f64, target_size_b.1 / ha as f64);

        let mut result = Vec::with_capacity(num_matches);
        for i in 0..num_matches {
            if s_data[i] < MIN_MATCH_SCORE {
                continue;
            }
            let idx_a = m_data[i * 3 + 1] as usize;
            let idx_b = m_data[i * 3 + 2] as usize;
            let (ax, ay) = keypoint_xy(0, idx_a);
            let (bx, by) = keypoint_xy(1, idx_b);
            result.push(PointMatch {
                a: Point2f::new((ax as f64 * scale_ax) as f32, (ay as f64 * scale_ay) as f32),
                b: Point2f::new((bx as f64 * scale_bx) as f32, (by as f64 * scale_by) as f32),
            });
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model_path() -> std::path::PathBuf {
        std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/resources/superpoint-ort.onnx"))
    }

    /// Needs a real overlapping drone photo pair on disk (not available in
    /// CI/most dev machines) - run explicitly with `-- --ignored`, same as
    /// `real_data_bench::real_stitch_benchmark`.
    #[test]
    #[ignore]
    fn matches_two_real_overlapping_photos() {
        let matcher = OnnxMatcher::load(&model_path()).expect("load model");
        let dir = "C:/Users/brake/Local/Cowork/search-test_images/Result/Jablunkov_Pass_Fortifications_CZ";
        let a_path = std::path::PathBuf::from(format!("{dir}/dji_0001.jpg"));
        let b_path = std::path::PathBuf::from(format!("{dir}/dji_0002.jpg"));
        let (wa, ha) = image::image_dimensions(&a_path).expect("read a dimensions");
        let (wb, hb) = image::image_dimensions(&b_path).expect("read b dimensions");

        // No rescale for this test - target size is each image's own native size.
        let matches = matcher
            .match_pair(&a_path, &b_path, (wa as f64, ha as f64), (wb as f64, hb as f64))
            .expect("match_pair");

        assert!(matches.len() > 100, "two overlapping real photos should yield plenty of matches, got {}", matches.len());
    }

}
