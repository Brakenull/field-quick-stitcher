//! Global 2D alignment (Quick Stitch spec 4.1/4.3): combines each photo's
//! GPS/yaw-derived initial pose with the pairwise visual homographies from
//! `homography.rs` into one consistent "photo pixel -> local world meters"
//! transform per photo, via sparse 2D pose-graph optimization (spec 4.3.2).
//!
//! Each photo is a 3-DOF pose (tx, ty, theta) - a `factrs` `SE2` variable;
//! ground scale (meters/pixel) is fixed from GPS altitude/focal/sensor rather
//! than optimized, since that's trusted sensor data, not something the visual
//! matches should be allowed to distort. Two kinds of factors feed the graph:
//! a `PriorResidual` per photo pulling its pose back toward the GPS/yaw
//! estimate (so photos with few or no confident edges don't drift
//! unconstrained), and a custom per-edge residual (below) for every confident
//! homography (`homography::estimate` already filtered these by inlier
//! ratio/count), Huber-robustified against false matches (spec 4.3.2.D).
//!
//! Unlike the naive dense/finite-difference least-squares solve this replaced,
//! `factrs` (spec 4.3.3) only ever forms Jacobian blocks for the two photos a
//! given factor actually touches, and its `LevenMarquardt` optimizer solves
//! the resulting sparse system (via `faer`) - so cost scales with the number
//! of GPS-neighbor edges, not the square/cube of the photo count. See
//! `real_data_bench.rs` for why this matters: the previous dense solver never
//! finished on a real ~550-photo survey.

use factrs::containers::{FactorBuilder, Graph, Values};
use factrs::linalg::{Const, ForwardProp, Numeric, VectorX};
use factrs::noise::GaussianNoise;
use factrs::optimizers::{LevenMarquardt, LevenParams, Optimizer};
use factrs::residuals::{PriorResidual, Residual2};
use factrs::robust::Huber;
use factrs::variables::SE2;
use factrs::assign_symbols;
use opencv::core::Mat;
use opencv::prelude::*;

use crate::core::geometry;
use crate::models::photo_meta::PhotoMeta;

use super::homography::HomographyEdge;

assign_symbols!(X: SE2);

const EARTH_RADIUS_M: f64 = 6_378_137.0;
/// GPS position accuracy assumed for consumer/DJI-class receivers (spec
/// 4.3.2.A's `sigma_x`/`sigma_y`): how strongly each photo's pose is pulled
/// back toward its GPS/yaw estimate.
const PRIOR_POSITION_SIGMA_M: f64 = 3.0;
/// Compass/gimbal-yaw accuracy assumed for the rotation prior (`sigma_theta`).
const PRIOR_ROTATION_SIGMA_RAD: f64 = 5.0_f64.to_radians();
/// Expected ground-position agreement (meters) between two GPS-neighbor
/// photos at a point their homography claims corresponds - i.e. how much a
/// *good* match's implied position can plausibly disagree with GPS/prior
/// edges before it's noise rather than signal. `homography::estimate` already
/// enforces a minimum inlier ratio/count before an edge is admitted here at
/// all, so this is deliberately a fixed physical noise floor, not scaled by
/// inlier count - Huber's threshold (spec 4.3.2.D) is what separates a
/// trustworthy edge from a false match (repetitive terrain, grass, rooftops),
/// and it only works if sigma reflects real expected noise rather than an
/// artificially shrunk "confidence".
const VISUAL_SIGMA_M: f64 = 2.0;
/// Hard iteration cap (spec 4.3.2.D: 10-15) so a bad dataset degrades
/// gracefully to "best effort after N iterations" instead of hanging - the
/// failure mode this whole module exists to eliminate.
const MAX_ITERATIONS: usize = 12;

/// A "photo pixel -> local world meters" transform, as a 3x3 homogeneous matrix.
#[derive(Clone, Copy, Debug)]
pub struct Pose(pub [[f64; 3]; 3]);

impl Pose {
    pub fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        apply_matrix(&self.0, x, y)
    }
}

pub(crate) fn apply_matrix(m: &[[f64; 3]; 3], x: f64, y: f64) -> (f64, f64) {
    let wx = m[0][0] * x + m[0][1] * y + m[0][2];
    let wy = m[1][0] * x + m[1][1] * y + m[1][2];
    let w = m[2][0] * x + m[2][1] * y + m[2][2];
    (wx / w, wy / w)
}

pub(crate) fn matmul3(a: &[[f64; 3]; 3], b: &[[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let mut out = [[0.0; 3]; 3];
    for r in 0..3 {
        for c in 0..3 {
            out[r][c] = a[r][0] * b[0][c] + a[r][1] * b[1][c] + a[r][2] * b[2][c];
        }
    }
    out
}

fn mat_to_array3(m: &Mat) -> Result<[[f64; 3]; 3], String> {
    let mut a = [[0.0; 3]; 3];
    for r in 0..3 {
        for c in 0..3 {
            a[r][c] = *m.at_2d::<f64>(r as i32, c as i32).map_err(|e| e.to_string())?;
        }
    }
    Ok(a)
}

fn to_local_meters(lat: f64, lon: f64, origin: (f64, f64)) -> (f64, f64) {
    let mid_lat_rad = origin.0.to_radians();
    let m_per_deg_lat = EARTH_RADIUS_M.to_radians();
    let m_per_deg_lon = EARTH_RADIUS_M.to_radians() * mid_lat_rad.cos();
    ((lon - origin.1) * m_per_deg_lon, (lat - origin.0) * m_per_deg_lat)
}

/// A shared local-meters origin for a set of photos (their centroid), so every
/// photo's pose lands in one consistent world frame.
pub fn world_origin(photos: &[PhotoMeta]) -> (f64, f64) {
    let n = (photos.len().max(1)) as f64;
    (
        photos.iter().map(|p| p.lat).sum::<f64>() / n,
        photos.iter().map(|p| p.lon).sum::<f64>() / n,
    )
}

/// GPS/yaw/altitude-derived similarity-transform parameters for one photo:
/// ground scale (meters/pixel, isotropic - the average of the x/y ground
/// sampling distance, since a similarity transform can't represent
/// anisotropic scale), image center, and initial translation+rotation.
struct SimilarityParams {
    scale: f64,
    cx: f64,
    cy: f64,
    tx: f64,
    ty: f64,
    theta: f64,
}

fn similarity_params(photo: &PhotoMeta, image_width_px: f64, image_height_px: f64, origin: (f64, f64)) -> Option<SimilarityParams> {
    let (w_m, h_m) = geometry::ground_coverage_m(
        photo.relative_altitude?,
        photo.focal_mm?,
        photo.sensor_width_mm?,
        photo.sensor_height_mm?,
    );
    if !w_m.is_finite() || !h_m.is_finite() || w_m <= 0.0 || h_m <= 0.0 {
        return None;
    }
    if image_width_px <= 0.0 || image_height_px <= 0.0 {
        return None;
    }

    let scale = (w_m / image_width_px + h_m / image_height_px) / 2.0;
    let theta = photo.yaw_deg?.to_radians();
    let (tx, ty) = to_local_meters(photo.lat, photo.lon, origin);
    Some(SimilarityParams { scale, cx: image_width_px / 2.0, cy: image_height_px / 2.0, tx, ty, theta })
}

/// Builds the "photo pixel -> world meters" matrix for a similarity transform
/// with image "up" = north at theta = 0 (matches
/// `core::geometry::compute_footprint`'s corner convention).
fn similarity_matrix(p: &SimilarityParams) -> [[f64; 3]; 3] {
    similarity_matrix_raw(p.tx, p.ty, p.theta, p.scale, p.cx, p.cy)
}

fn similarity_matrix_raw(tx: f64, ty: f64, theta: f64, scale: f64, cx: f64, cy: f64) -> [[f64; 3]; 3] {
    let (sin_t, cos_t) = theta.sin_cos();
    let m00 = scale * cos_t;
    let m01 = -scale * sin_t;
    let m10 = -scale * sin_t;
    let m11 = -scale * cos_t;
    [
        [m00, m01, tx - cx * m00 - cy * m01],
        [m10, m11, ty - cx * m10 - cy * m11],
        [0.0, 0.0, 1.0],
    ]
}

/// The GPS/yaw/altitude-derived initial pose for one photo (spec 4.1), with no
/// visual information. `image_width_px`/`image_height_px` must be the
/// dimensions of the image actually used for feature extraction (i.e. the
/// downsampled size), so the resulting pose operates in the same pixel space
/// as the homography edges.
pub fn initial_pose(photo: &PhotoMeta, image_width_px: f64, image_height_px: f64, origin: (f64, f64)) -> Option<Pose> {
    similarity_params(photo, image_width_px, image_height_px, origin).map(|p| Pose(similarity_matrix(&p)))
}

/// One directed visual edge: `h` maps pixel coordinates in photo `from` into
/// photo `to`'s pixel coordinates (as produced by `homography::estimate(from, to, ..)`).
pub struct Edge {
    pub from: usize,
    pub to: usize,
    pub h: HomographyEdge,
}

/// World-position disagreement between two photos' poses at a handful of
/// matched pixel locations (image corners + center), given a homography that
/// maps photo `i` pixels into photo `j` pixels. This is the same "project
/// sample points through each photo's similarity transform and compare"
/// formulation the pipeline has always used - only the differentiation
/// (factrs' forward-mode autodiff, spec 4.3.2.C) and the solve (factrs' sparse
/// per-factor linearization, spec 4.3.2.B) are new. Fixed per-photo constants
/// (`scale`/`cx`/`cy`, trusted GPS/altitude-derived values, not part of the
/// optimization) and the sample points themselves are precomputed once when
/// the edge is built, not recomputed per solver iteration.
#[derive(Clone, Debug)]
struct VisualEdgeResidual {
    scale_i: f64,
    cx_i: f64,
    cy_i: f64,
    scale_j: f64,
    cx_j: f64,
    cy_j: f64,
    /// Sample points in photo i's pixel space and their corresponding points
    /// in photo j's pixel space (mapped through the edge homography), always
    /// 5 entries (4 corners + center) - fixing this residual's output
    /// dimension at a compile-time constant (`DimOut = Const<10>`).
    samples_i: [(f64, f64); 5],
    samples_j: [(f64, f64); 5],
}

impl VisualEdgeResidual {
    /// Projects pixel `(x, y)` of a photo with the given (fixed) scale/center
    /// and (optimizable) pose `(theta, tx, ty)` into world meters. Mirrors
    /// `similarity_matrix_raw` exactly, generalized to any dual/autodiff-
    /// compatible numeric type so factrs can differentiate through it.
    fn project<T: Numeric>(theta: T, tx: T, ty: T, scale: f64, cx: f64, cy: f64, x: f64, y: f64) -> (T, T) {
        let (sin_t, cos_t) = (theta.sin(), theta.cos());
        let scale = T::from(scale);
        let u = (T::from(x) - T::from(cx)) * scale;
        let v = (T::from(y) - T::from(cy)) * scale;
        let wx = tx + cos_t * u - sin_t * v;
        let wy = ty - sin_t * u - cos_t * v;
        (wx, wy)
    }
}

#[factrs::mark]
impl Residual2 for VisualEdgeResidual {
    type V1 = SE2;
    type V2 = SE2;
    type DimIn = Const<6>;
    type DimOut = Const<10>;
    type Differ = ForwardProp<Const<6>>;

    fn residual2<T: Numeric>(&self, v1: SE2<T>, v2: SE2<T>) -> VectorX<T> {
        let (theta_i, tx_i, ty_i) = (v1.theta(), v1.x(), v1.y());
        let (theta_j, tx_j, ty_j) = (v2.theta(), v2.x(), v2.y());

        let mut out = Vec::with_capacity(10);
        for (&(xi, yi), &(xj, yj)) in self.samples_i.iter().zip(self.samples_j.iter()) {
            let (wi_x, wi_y) = Self::project(theta_i, tx_i, ty_i, self.scale_i, self.cx_i, self.cy_i, xi, yi);
            let (wj_x, wj_y) = Self::project(theta_j, tx_j, ty_j, self.scale_j, self.cx_j, self.cy_j, xj, yj);
            out.push(wi_x - wj_x);
            out.push(wi_y - wj_y);
        }
        VectorX::from_vec(out)
    }
}

/// Aligns every photo into one shared world frame via sparse 2D pose-graph
/// optimization (spec 4.3). `photos[i]`/`image_sizes[i]` must correspond (same
/// indexing `edges` uses). Returns `None` for any photo lacking enough
/// metadata for even a GPS pose.
pub fn align(photos: &[PhotoMeta], image_sizes: &[(f64, f64)], edges: &[Edge]) -> Result<Vec<Option<Pose>>, String> {
    let n = photos.len();
    let origin = world_origin(photos);

    let mut param_block: Vec<Option<usize>> = vec![None; n];
    let mut scale = Vec::new();
    let mut cx = Vec::new();
    let mut cy = Vec::new();
    let mut initial = Vec::new();
    for i in 0..n {
        let Some(p) = similarity_params(&photos[i], image_sizes[i].0, image_sizes[i].1, origin) else { continue };
        param_block[i] = Some(scale.len());
        scale.push(p.scale);
        cx.push(p.cx);
        cy.push(p.cy);
        initial.push((p.tx, p.ty, p.theta));
    }

    if initial.is_empty() {
        return Ok(vec![None; n]);
    }

    let mut values = Values::new();
    let mut graph = Graph::new();
    for (block, &(tx, ty, theta)) in initial.iter().enumerate() {
        values.insert(X(block as u32), SE2::new(theta, tx, ty));

        let prior = PriorResidual::new(SE2::new(theta, tx, ty));
        // SE2's residual order is (theta, x, y) - rotation always first.
        let noise = GaussianNoise::<3>::from_diag_sigmas(PRIOR_ROTATION_SIGMA_RAD, PRIOR_POSITION_SIGMA_M, PRIOR_POSITION_SIGMA_M);
        graph.add_factor(FactorBuilder::new1(prior, X(block as u32)).noise(noise).build());
    }

    for edge in edges {
        let (Some(bi), Some(bj)) = (param_block[edge.from], param_block[edge.to]) else { continue };
        let h = mat_to_array3(&edge.h.h)?;
        let (w, ht) = image_sizes[edge.from];
        let samples_i = [(0.0, 0.0), (w, 0.0), (w, ht), (0.0, ht), (w / 2.0, ht / 2.0)];
        let samples_j = samples_i.map(|(x, y)| apply_matrix(&h, x, y));

        let residual = VisualEdgeResidual {
            scale_i: scale[bi],
            cx_i: cx[bi],
            cy_i: cy[bi],
            scale_j: scale[bj],
            cx_j: cx[bj],
            cy_j: cy[bj],
            samples_i,
            samples_j,
        };
        let noise = GaussianNoise::<10>::from_scalar_sigma(VISUAL_SIGMA_M);
        graph.add_factor(FactorBuilder::new2(residual, X(bi as u32), X(bj as u32)).noise(noise).robust(Huber::default()).build());
    }

    // A hand-rolled bounded loop rather than `Optimizer::optimize` (spec
    // 4.3.2.D: hard iteration cap, and never fail - always return the best
    // pose found so far). `optimize`'s built-in stopping can't do that: right
    // at convergence it's common for the damping search to exhaust `lambda`
    // trying to find a further-improving step (`OptError::FailedToStep`),
    // which carries no `Values` - there would be nothing to fall back to.
    // Stepping manually means a failed/non-improving step just ends the loop
    // with the last good `current` still in hand.
    let mut opt = LevenMarquardt::new(LevenParams::default(), graph);
    opt.init(&values);
    let mut current = values;
    let mut prev_error = opt.error(&current);
    for i in 1..=MAX_ITERATIONS {
        let Ok((next, _info)) = opt.step(current.clone(), i) else { break };
        let new_error = opt.error(&next);
        let decrease_rel = (prev_error - new_error) / prev_error.max(1e-12);
        current = next;
        prev_error = new_error;
        if new_error <= 1e-9 || (decrease_rel >= 0.0 && decrease_rel < 1e-4) {
            break;
        }
    }
    let solved = current;

    let mut result = vec![None; n];
    for i in 0..n {
        let Some(block) = param_block[i] else { continue };
        let pose: &SE2 = solved.get(X(block as u32)).expect("every optimizable photo should have a solved pose");
        result[i] = Some(Pose(similarity_matrix_raw(pose.x(), pose.y(), pose.theta(), scale[block], cx[block], cy[block])));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use opencv::core::CV_64F;

    fn photo_at(lat: f64, lon: f64, yaw_deg: f64) -> PhotoMeta {
        PhotoMeta {
            file_name: String::new(),
            path: String::new(),
            lat,
            lon,
            relative_altitude: Some(80.0),
            yaw_deg: Some(yaw_deg),
            focal_mm: Some(8.8),
            sensor_width_mm: Some(13.2),
            sensor_height_mm: Some(8.8),
            image_width_px: Some(5472),
            image_height_px: Some(3648),
            footprint: None,
            capture_time: None,
            is_blurry: false,
            blur_score: None,
            warnings: Vec::new(),
        }
    }

    fn identity_mat() -> Mat {
        Mat::eye(3, 3, CV_64F).unwrap().to_mat().unwrap()
    }

    #[test]
    fn initial_pose_maps_image_center_to_own_gps_position() {
        let photo = photo_at(10.0, 106.0, 0.0);
        let origin = (10.0, 106.0);
        let pose = initial_pose(&photo, 1000.0, 800.0, origin).expect("pose");
        let (e, n) = pose.apply(500.0, 400.0); // image center
        assert!(e.abs() < 1e-6 && n.abs() < 1e-6, "own center should map to own origin-relative position, got ({e},{n})");
    }

    #[test]
    fn missing_metadata_yields_no_pose() {
        let mut photo = photo_at(10.0, 106.0, 0.0);
        photo.relative_altitude = None;
        assert!(initial_pose(&photo, 1000.0, 800.0, (10.0, 106.0)).is_none());
    }

    #[test]
    fn no_edges_keeps_every_photo_near_its_gps_pose() {
        let photos = vec![photo_at(10.0, 106.0, 0.0), photo_at(10.0001, 106.0001, 0.0)];
        let sizes = vec![(1000.0, 800.0), (1000.0, 800.0)];
        let refined = align(&photos, &sizes, &[]).unwrap();

        let origin = world_origin(&photos);
        for (i, photo) in photos.iter().enumerate() {
            let expected = initial_pose(photo, sizes[i].0, sizes[i].1, origin).unwrap();
            let got = refined[i].unwrap();
            // With zero edges, only the GPS-prior residual exists, and it's
            // already zero at the initial guess - the solver should leave it there.
            assert!((got.0[0][2] - expected.0[0][2]).abs() < 1e-3, "photo {i} east drifted with no edges");
            assert!((got.0[1][2] - expected.0[1][2]).abs() < 1e-3, "photo {i} north drifted with no edges");
        }
    }

    #[test]
    fn identity_edge_pulls_both_photos_toward_each_other() {
        // Two photos with a small, realistic GPS/vision disagreement (a few
        // meters - the scale of ordinary consumer-GPS drift, not a gross
        // outlier) and an identity homography claiming their pixel spaces
        // coincide exactly: the optimizer should compromise, landing well
        // short of the full GPS separation for the disagreeing pair. This
        // deliberately stays within Huber's inlier regime (spec 4.3.2.D) -
        // a *huge* disagreement is exactly what the robust kernel exists to
        // discount, so it wouldn't exercise this "trust a good edge" behavior.
        let east_shift_deg = 12.0 / (EARTH_RADIUS_M.to_radians() * 10f64.to_radians().cos());
        let photos = vec![photo_at(10.0, 106.0, 0.0), photo_at(10.0, 106.0 + east_shift_deg, 0.0)];
        let sizes = vec![(1000.0, 800.0), (1000.0, 800.0)];
        let edge = Edge { from: 0, to: 1, h: HomographyEdge { h: identity_mat(), inlier_count: 100, match_count: 100 } };
        let refined = align(&photos, &sizes, std::slice::from_ref(&edge)).unwrap();

        let (e0, n0) = refined[0].unwrap().apply(500.0, 400.0);
        let (e1, n1) = refined[1].unwrap().apply(500.0, 400.0);
        let gap = ((e0 - e1).powi(2) + (n0 - n1).powi(2)).sqrt();

        let origin = world_origin(&photos);
        let gps0 = initial_pose(&photos[0], sizes[0].0, sizes[0].1, origin).unwrap().apply(500.0, 400.0);
        let gps1 = initial_pose(&photos[1], sizes[1].0, sizes[1].1, origin).unwrap().apply(500.0, 400.0);
        let gps_gap = ((gps0.0 - gps1.0).powi(2) + (gps0.1 - gps1.1).powi(2)).sqrt();

        assert!(gap < gps_gap * 0.5, "identity edge should pull the two centers much closer together (gap {gap} vs gps gap {gps_gap})");
    }

    #[test]
    fn consistent_edge_and_gps_agree_and_stay_put() {
        // Two photos exactly 72m apart (east) with a homography that's exactly
        // consistent with that GPS placement: both residual sources agree, so
        // the solved poses should match the GPS-only poses closely.
        let approx_m_per_deg_lon = EARTH_RADIUS_M.to_radians() * 10f64.to_radians().cos();
        let east_shift_deg = 72.0 / approx_m_per_deg_lon;
        let photos = vec![photo_at(10.0, 106.0, 0.0), photo_at(10.0, 106.0 + east_shift_deg, 0.0)];
        let sizes = vec![(1200.0, 800.0), (1200.0, 800.0)];

        // 80m alt / 8.8mm focal / 13.2x8.8mm sensor over 1200x800 -> 0.1 m/px;
        // 72m -> 720px pure-translation homography (photo0 -> photo1).
        let mut h = Mat::eye(3, 3, CV_64F).unwrap().to_mat().unwrap();
        *h.at_2d_mut::<f64>(0, 2).unwrap() = -720.0;
        let edge = Edge { from: 0, to: 1, h: HomographyEdge { h, inlier_count: 100, match_count: 100 } };

        let refined = align(&photos, &sizes, std::slice::from_ref(&edge)).unwrap();
        let origin = world_origin(&photos);
        let gps = initial_pose(&photos[1], sizes[1].0, sizes[1].1, origin).unwrap();
        let solved = refined[1].unwrap();
        assert!((solved.0[0][2] - gps.0[0][2]).abs() < 0.5, "consistent edge shouldn't move an already-agreeing pose");
    }
}
