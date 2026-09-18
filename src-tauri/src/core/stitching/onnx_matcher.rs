//! Two-stage feature extraction + matching via ONNX Runtime: a standalone
//! SuperPoint extractor (`resources/superpoint.onnx`) run once per photo, and
//! a separate LightGlue matcher (`resources/superpoint_lightglue.trt.onnx`)
//! run once per neighbor pair on the cached keypoints/descriptors - both from
//! [LightGlue-ONNX v1.0.0](https://github.com/fabio-sim/LightGlue-ONNX/releases/tag/v1.0.0),
//! matching the extractor-then-matcher split `quick-stitch.md` originally
//! described.
//!
//! This module previously used v2.0's *fused* single-graph export that took
//! two raw images in and matches out directly. That fused graph had two
//! compounding problems on a real multi-hundred-photo survey (see
//! `real_data_bench.rs`'s "Jablunkov" dataset): it only produced usable
//! matches at full native photo resolution (essentially zero matches at
//! `downsample.rs`'s normal ~1600px cache, independent of interpolation or
//! content), and because extraction wasn't separable from matching, every
//! neighbor pair re-ran the *entire* feature-extraction backbone from
//! scratch - redundantly, once per pair a photo appeared in, rather than once
//! per photo. Together those made the matching stage take hours instead of
//! minutes.
//!
//! The standalone extractor doesn't share that resolution floor - confirmed
//! on the same real Jablunkov photos, it finds 2000+ keypoints per image and
//! 1000+ confident matches per overlapping pair at the normal 1600px
//! downsampled cache - so extraction can run directly on `downsample.rs`'s
//! output (see `pipeline.rs`), once per photo, and the matcher itself only
//! ever touches small keypoint/descriptor tensors, not pixels, making it
//! cheap to re-run per pair.
//!
//! The extractor runs on DirectML (GPU) - unlike the old fused model, this
//! runs on the 1600px downsampled cache rather than full native resolution,
//! and that's the difference that matters: the fused model's DirectML OOM
//! (`8007000E`) failed in the SuperPoint backbone's very first conv layer at
//! full native res (~4000x3000); at 1600px the same backbone fits comfortably
//! and extraction drops from ~7-8 minutes to well under 2 minutes across the
//! full 156-photo Jablunkov survey. The matcher stays on CPU - it only ever
//! touches small keypoint/descriptor tensors, never pixels, so GPU dispatch
//! wouldn't meaningfully help and isn't worth the added complexity there.
//!
//! TRIED AND REVERTED: pooling the matcher across several independent
//! sessions (one `Mutex<Session>` each, round-robin) to let multiple pairs'
//! inference calls run concurrently instead of serializing through one
//! session. It needed each pooled session's CPU memory arena disabled
//! (`ep::CPU::default().with_arena_allocator(false)`) to stay memory-safe -
//! by default every session gets its own arena that (like the memory-pattern
//! cache below) grows to a high-water-mark and never shrinks, and multiplied
//! by several independent sessions instead of one, that pushed the process
//! past 6GB and this machine to under 400MB free within minutes on the real
//! Jablunkov survey. Disabling the arena fixed the memory problem, but
//! measured slower end-to-end than the single-session version below on three
//! real full-survey runs (~93 min pooled vs. ~59 min single-session) - arenas
//! exist because they're faster than plain malloc/free for many small
//! repeated allocations, and 6 threads all doing raw malloc/free concurrently
//! apparently cost more in allocator contention than the pooling gained in
//! parallelism. Back to one shared session; revisit only with a way to pool
//! without disabling each session's arena.
//!
//! The extractor and matcher are two separate types (`FeatureExtractor`,
//! `PairMatcher`), not one struct holding both sessions, specifically so
//! `pipeline.rs`'s `match_pairs_onnx` can drop the extractor - and with it,
//! its DirectML (GPU) session's VRAM/driver allocations - as soon as the
//! DetectingFeatures stage finishes, instead of holding that GPU session
//! open (idle) through the whole CPU-only MatchingPairs stage that follows.
//!
//! `FeatureExtractor::extract` also caps keypoints per photo at
//! [`MAX_KEYPOINTS_PER_IMAGE`] by SuperPoint's own confidence score (see that
//! constant's doc comment) - LightGlue's matching cost scales worse than
//! linearly with keypoint count, so a densely/repetitively textured survey
//! (quarries, gravel, rooftops) can otherwise turn a ~1 hour stitch into
//! several hours with no visible warning, confirmed on two real surveys.

use std::path::Path;
use std::sync::Mutex;

use ndarray::{Array3, Array4};
use opencv::core::Point2f;
use ort::session::Session;
use ort::value::TensorRef;

/// One photo's extracted SuperPoint features, cached once and reused for
/// every neighbor pair that photo appears in.
pub struct FeatureSet {
    /// Pixel-space keypoints in the same (downsampled) image coordinate
    /// space the rest of the pipeline (homography/pose graph/mosaic) already
    /// works in - no rescaling needed downstream.
    keypoints: Vec<Point2f>,
    /// Keypoints normalized into LightGlue's expected input range (see
    /// `normalize_keypoints`), shape `[1, N, 2]`.
    normalized: Array3<f32>,
    /// Raw descriptors, shape `[1, N, 256]`.
    descriptors: Array3<f32>,
}

/// One matched point pair, already filtered by confidence (see
/// `MIN_MATCH_SCORE`) and in the same downsampled coordinate space the
/// corresponding `FeatureSet`s were extracted from.
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

/// Hard cap on keypoints kept per photo, by SuperPoint's own per-keypoint
/// confidence (the `scores` output - see `cap_by_score`). LightGlue's
/// cross-attention cost scales worse than linearly with keypoint count:
/// measured on two real surveys, a densely/repetitively textured quarry site
/// (`Wietrznia_Quarry_PL`) produced ~2.2x the keypoints per photo of a normal
/// survey (`Jablunkov_Pass_Fortifications_CZ`, both via `real_data_bench.rs`),
/// and its `match_pair` calls measured ~4.6x slower - close to the ~2.2^2 a
/// quadratic cost would predict, and enough to turn a ~1 hour stitch into a
/// 6+ hour one. `homography.rs`'s RANSAC only ever needs a few dozen good
/// correspondences per pair, so this trades a bit of match density (dropping
/// the least-confident keypoints first) for a matching-time cap that holds
/// regardless of how texture-dense the terrain is. 2000 sits just under a
/// typical/normal survey's own keypoint count, so it rarely truncates
/// anything on non-adversarial terrain.
const MAX_KEYPOINTS_PER_IMAGE: usize = 2000;

/// Owns just the SuperPoint extractor's DirectML session - kept separate from
/// `PairMatcher` so it can be dropped (freeing its GPU session) the moment
/// the DetectingFeatures stage finishes, before the CPU-only MatchingPairs
/// stage starts (see module doc comment).
pub struct FeatureExtractor {
    session: Mutex<Session>,
}

/// Owns just the LightGlue matcher's CPU session - loaded separately from,
/// and outliving, `FeatureExtractor` (see module doc comment).
pub struct PairMatcher {
    session: Mutex<Session>,
}

/// Normalizes pixel-space keypoints into LightGlue's expected input range,
/// mirroring the reference `onnx_runner`'s `normalize_keypoints`: centered on
/// the image and scaled by half its longer edge.
fn normalize_keypoints(keypoints: &[Point2f], width: u32, height: u32) -> Array3<f32> {
    let (shift_x, shift_y) = (width as f32 / 2.0, height as f32 / 2.0);
    let scale = width.max(height) as f32 / 2.0;
    let mut normalized = Array3::<f32>::zeros((1, keypoints.len(), 2));
    for (i, kp) in keypoints.iter().enumerate() {
        normalized[[0, i, 0]] = (kp.x - shift_x) / scale;
        normalized[[0, i, 1]] = (kp.y - shift_y) / scale;
    }
    normalized
}

impl FeatureExtractor {
    /// Loads just the SuperPoint extractor onto DirectML - see module doc
    /// comment for why this is safe now (1600px, not full native res).
    pub fn load(extractor_path: &Path) -> Result<Self, String> {
        // Memory pattern optimization is disabled - it caches a distinct
        // allocation plan per input shape combination, which never stops
        // growing here since keypoint counts (and therefore tensor shapes)
        // differ on essentially every photo; confirmed on the real
        // 156-photo Jablunkov survey (see `real_data_bench.rs`), where
        // leaving it enabled grew the process past 4GB and climbing well
        // before the matching stage finished. `ort`'s own docs recommend
        // disabling it for exactly this "input size varies" case.
        let session = Session::builder()
            .map_err(|e| e.to_string())?
            .with_execution_providers([ort::ep::DirectML::default().build()])
            .map_err(|e| e.to_string())?
            .with_memory_pattern(false)
            .map_err(|e| e.to_string())?
            .commit_from_file(extractor_path)
            .map_err(|e| e.to_string())?;

        Ok(Self { session: Mutex::new(session) })
    }

    /// Extracts SuperPoint keypoints + descriptors from `image_path` (the
    /// pipeline's downsampled cache - see module doc comment for why not the
    /// original full-resolution photo). Image loading/preprocessing happens
    /// outside the session lock so it can run in parallel across photos;
    /// only the ONNX Runtime call itself, and the tensor data it returns,
    /// need the lock held.
    pub fn extract(&self, image_path: &Path) -> Result<FeatureSet, String> {
        let img = image::open(image_path).map_err(|e| e.to_string())?.to_luma8();
        let (width, height) = img.dimensions();
        let mut tensor = Array4::<f32>::zeros((1, 1, height as usize, width as usize));
        for (x, y, p) in img.enumerate_pixels() {
            tensor[[0, 0, y as usize, x as usize]] = p[0] as f32 / 255.0;
        }

        let (keypoint_count, kp_data, desc_data, score_data) = {
            let mut session = self.session.lock().map_err(|_| "SuperPoint session lock poisoned".to_string())?;
            let input = TensorRef::from_array_view(&tensor).map_err(|e| e.to_string())?;
            let outputs = session.run(ort::inputs!["image" => input]).map_err(|e| e.to_string())?;
            let (kp_shape, kp_data) = outputs["keypoints"].try_extract_tensor::<i64>().map_err(|e| e.to_string())?;
            let (_, desc_data) = outputs["descriptors"].try_extract_tensor::<f32>().map_err(|e| e.to_string())?;
            let (_, score_data) = outputs["scores"].try_extract_tensor::<f32>().map_err(|e| e.to_string())?;
            (kp_shape[1] as usize, kp_data.to_vec(), desc_data.to_vec(), score_data.to_vec())
        };

        let keypoints: Vec<Point2f> =
            (0..keypoint_count).map(|i| Point2f::new(kp_data[i * 2] as f32, kp_data[i * 2 + 1] as f32)).collect();
        let (keypoints, desc_data, keypoint_count) = cap_by_score(keypoints, desc_data, &score_data, MAX_KEYPOINTS_PER_IMAGE);

        let normalized = normalize_keypoints(&keypoints, width, height);
        let descriptors = Array3::from_shape_vec((1, keypoint_count, 256), desc_data).map_err(|e| e.to_string())?;

        Ok(FeatureSet { keypoints, normalized, descriptors })
    }
}

/// Keeps only the `max_keypoints` highest-confidence keypoints (and their
/// matching descriptor rows, kept in the same relative order as `scores`
/// gives them - only the *set* kept matters, not the order), when the
/// extractor found more than that. A no-op (returns everything unchanged)
/// when `keypoints.len() <= max_keypoints` - see [`MAX_KEYPOINTS_PER_IMAGE`]
/// for why this exists.
fn cap_by_score(keypoints: Vec<Point2f>, descriptors: Vec<f32>, scores: &[f32], max_keypoints: usize) -> (Vec<Point2f>, Vec<f32>, usize) {
    let n = keypoints.len();
    if n <= max_keypoints {
        return (keypoints, descriptors, n);
    }

    let mut order: Vec<usize> = (0..n).collect();
    order.sort_unstable_by(|&a, &b| scores[b].partial_cmp(&scores[a]).unwrap_or(std::cmp::Ordering::Equal));
    order.truncate(max_keypoints);

    let kept_keypoints: Vec<Point2f> = order.iter().map(|&i| keypoints[i]).collect();
    let mut kept_descriptors = Vec::with_capacity(max_keypoints * 256);
    for &i in &order {
        kept_descriptors.extend_from_slice(&descriptors[i * 256..(i + 1) * 256]);
    }
    (kept_keypoints, kept_descriptors, max_keypoints)
}

impl PairMatcher {
    /// Loads just the LightGlue matcher, on the default CPU execution
    /// provider - see module doc comment for why GPU dispatch wouldn't help
    /// here.
    pub fn load(matcher_path: &Path) -> Result<Self, String> {
        let session = Session::builder()
            .map_err(|e| e.to_string())?
            .with_memory_pattern(false)
            .map_err(|e| e.to_string())?
            .commit_from_file(matcher_path)
            .map_err(|e| e.to_string())?;

        Ok(Self { session: Mutex::new(session) })
    }

    /// Matches two already-extracted feature sets. Cheap relative to
    /// `FeatureExtractor::extract` - only keypoint/descriptor tensors cross
    /// the ONNX boundary, not pixels - but still serializes behind the
    /// matcher session's lock (see module doc comment for why this isn't
    /// pooled).
    pub fn match_pair(&self, a: &FeatureSet, b: &FeatureSet) -> Result<Vec<PointMatch>, String> {
        let (match_count, m_data, s_data) = {
            let mut session = self.session.lock().map_err(|_| "LightGlue session lock poisoned".to_string())?;
            let outputs = session
                .run(ort::inputs![
                    "kpts0" => TensorRef::from_array_view(&a.normalized).map_err(|e| e.to_string())?,
                    "kpts1" => TensorRef::from_array_view(&b.normalized).map_err(|e| e.to_string())?,
                    "desc0" => TensorRef::from_array_view(&a.descriptors).map_err(|e| e.to_string())?,
                    "desc1" => TensorRef::from_array_view(&b.descriptors).map_err(|e| e.to_string())?,
                ])
                .map_err(|e| e.to_string())?;
            let (m_shape, m_data) = outputs["matches0"].try_extract_tensor::<i64>().map_err(|e| e.to_string())?;
            let (_, s_data) = outputs["mscores0"].try_extract_tensor::<f32>().map_err(|e| e.to_string())?;
            (m_shape[0] as usize, m_data.to_vec(), s_data.to_vec())
        };

        let mut result = Vec::with_capacity(match_count);
        for i in 0..match_count {
            if s_data[i] < MIN_MATCH_SCORE {
                continue;
            }
            let idx_a = m_data[i * 2] as usize;
            let idx_b = m_data[i * 2 + 1] as usize;
            result.push(PointMatch { a: a.keypoints[idx_a], b: b.keypoints[idx_b] });
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::stitching::downsample;

    #[test]
    fn cap_by_score_keeps_the_highest_scoring_keypoints() {
        let keypoints = vec![Point2f::new(0.0, 0.0), Point2f::new(1.0, 1.0), Point2f::new(2.0, 2.0), Point2f::new(3.0, 3.0)];
        let descriptors: Vec<f32> = (0..4).flat_map(|i| vec![i as f32; 256]).collect();
        let scores = [0.1, 0.9, 0.3, 0.5]; // index 1 best, then 3, then 2, then 0

        let (kept_kps, kept_desc, kept_n) = cap_by_score(keypoints, descriptors, &scores, 2);

        assert_eq!(kept_n, 2);
        assert_eq!(kept_kps, vec![Point2f::new(1.0, 1.0), Point2f::new(3.0, 3.0)]);
        // Descriptor rows must stay aligned with their keypoint after reordering.
        assert_eq!(kept_desc, [vec![1.0f32; 256], vec![3.0f32; 256]].concat());
    }

    #[test]
    fn cap_by_score_is_a_no_op_under_the_cap() {
        let keypoints = vec![Point2f::new(0.0, 0.0), Point2f::new(1.0, 1.0)];
        let descriptors: Vec<f32> = vec![0.0; 512];
        let scores = [0.1, 0.9];

        let (kept_kps, kept_desc, kept_n) = cap_by_score(keypoints.clone(), descriptors.clone(), &scores, 10);

        assert_eq!(kept_n, 2);
        assert_eq!(kept_kps, keypoints);
        assert_eq!(kept_desc, descriptors);
    }

    fn extractor_path() -> std::path::PathBuf {
        std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/resources/superpoint.onnx"))
    }

    fn matcher_path() -> std::path::PathBuf {
        std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/resources/superpoint_lightglue.trt.onnx"))
    }

    /// Needs a real overlapping drone photo pair on disk (not available in
    /// CI/most dev machines) - run explicitly with `-- --ignored`, same as
    /// `real_data_bench::real_stitch_benchmark`. Downsamples first via
    /// `downsample::downsample_one`, exercising the exact same code path
    /// `pipeline.rs` does rather than matching at native resolution.
    #[test]
    #[ignore]
    fn extracts_and_matches_two_real_overlapping_photos() {
        let dir = "C:/Users/brake/Local/Cowork/search-test_images/Result/Jablunkov_Pass_Fortifications_CZ";
        let a_path = std::path::PathBuf::from(format!("{dir}/dji_0001.jpg"));
        let b_path = std::path::PathBuf::from(format!("{dir}/dji_0002.jpg"));

        let workspace = tempfile::tempdir().expect("tempdir");
        let a_downsampled = downsample::downsample_one(&a_path, &workspace.path().join("a.jpg")).expect("downsample a");
        let b_downsampled = downsample::downsample_one(&b_path, &workspace.path().join("b.jpg")).expect("downsample b");

        let extractor = FeatureExtractor::load(&extractor_path()).expect("load extractor");
        let features_a = extractor.extract(&a_downsampled.path).expect("extract a");
        let features_b = extractor.extract(&b_downsampled.path).expect("extract b");
        drop(extractor); // mirrors pipeline.rs dropping it before matching starts

        let matcher = PairMatcher::load(&matcher_path()).expect("load matcher");
        let matches = matcher.match_pair(&features_a, &features_b).expect("match_pair");
        assert!(matches.len() > 100, "two overlapping real photos should yield plenty of matches, got {}", matches.len());
    }
}
