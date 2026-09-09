//! Warps aligned photos into one shared raster and composites them with
//! center-distance-weighted blending (Quick Stitch spec 4.4). Each photo is
//! warped only into the canvas-pixel rectangle its own footprint actually
//! covers - not the whole output canvas - since `world_to_raster` has no
//! rotation, so a per-photo warp-to-full-canvas call (and the accumulate pass
//! over it) did canvas-sized work N times instead of photo-sized work once
//! per photo. That cropping is also what makes it safe to warp every photo in
//! parallel: each writes into its own freshly-allocated ROI buffer, and only
//! the (cheap, ROI-sized) accumulation into the shared canvas needs to run
//! sequentially.

use std::path::Path;

use opencv::core::{Mat, Rect, Scalar, Size, Vec3b, Vec3f, BORDER_CONSTANT, CV_32FC1, CV_32FC3, CV_64F, CV_8UC3};
use opencv::imgcodecs;
use opencv::imgproc;
use opencv::prelude::*;
use opencv::Result;

use crate::utils::thread_pool::par_map_with_progress;

use super::blending::center_distance_weights;
use super::pose_graph::{apply_matrix, matmul3, Pose};

pub struct Mosaic {
    pub image: Mat,
    /// World meters (relative to the pose-graph origin) of the raster's
    /// top-left pixel, and the raster's meters-per-pixel resolution.
    pub origin_east: f64,
    pub origin_north: f64,
    pub meters_per_pixel: f64,
}

struct Bounds {
    min_e: f64,
    max_e: f64,
    min_n: f64,
    max_n: f64,
}

fn image_bounds(pose: &Pose, width: i32, height: i32) -> Bounds {
    let corners = [(0.0, 0.0), (width as f64, 0.0), (width as f64, height as f64), (0.0, height as f64)];
    let mut b = Bounds { min_e: f64::INFINITY, max_e: f64::NEG_INFINITY, min_n: f64::INFINITY, max_n: f64::NEG_INFINITY };
    for (x, y) in corners {
        let (e, n) = pose.apply(x, y);
        b.min_e = b.min_e.min(e);
        b.max_e = b.max_e.max(e);
        b.min_n = b.min_n.min(n);
        b.max_n = b.max_n.max(n);
    }
    b
}

fn array3_to_mat(m: &[[f64; 3]; 3]) -> Result<Mat> {
    let mut mat = Mat::new_rows_cols_with_default(3, 3, CV_64F, Scalar::all(0.0))?;
    for (r, row) in m.iter().enumerate() {
        for (c, &v) in row.iter().enumerate() {
            *mat.at_2d_mut::<f64>(r as i32, c as i32)? = v;
        }
    }
    Ok(mat)
}

/// The canvas-pixel rectangle a photo's (world-space) bounding box occupies,
/// clamped to the canvas. `world_to_raster` is a pure scale+translate (no
/// rotation), so transforming just the box's two opposite corners is enough
/// to get an axis-aligned raster-space box. Returns `None` if the (clamped)
/// rectangle is empty.
fn raster_rect(bounds: &Bounds, world_to_raster: &[[f64; 3]; 3], canvas_w: i32, canvas_h: i32) -> Option<Rect> {
    let (x0, y0) = apply_matrix(world_to_raster, bounds.min_e, bounds.min_n);
    let (x1, y1) = apply_matrix(world_to_raster, bounds.max_e, bounds.max_n);
    let x = (x0.min(x1).floor() as i32).clamp(0, canvas_w);
    let y = (y0.min(y1).floor() as i32).clamp(0, canvas_h);
    let x_end = (x0.max(x1).ceil() as i32).clamp(0, canvas_w);
    let y_end = (y0.max(y1).ceil() as i32).clamp(0, canvas_h);
    (x_end > x && y_end > y).then(|| Rect::new(x, y, x_end - x, y_end - y))
}

/// One photo's placement in the output canvas, computed up front (cheap, no
/// image I/O) so the expensive warp step below only ever touches the pixels a
/// photo actually covers.
struct Placement<'a, Pth> {
    path: &'a Pth,
    /// The canvas rectangle this photo lands in.
    rect: Rect,
    /// "photo pixel -> `rect`-local pixel" homography.
    pixel_to_roi: [[f64; 3]; 3],
    source_size: (i32, i32),
}

/// Composites the image at `image_paths[i]` (sized `image_sizes[i]`, warped
/// through `poses[i]`) into one raster at `meters_per_pixel` resolution.
/// Images without a pose are skipped. Returns `None` if no image had a pose
/// at all. `image_paths`/`image_sizes`/`poses` must all be the same length,
/// indexed the same way. `on_progress` is called (from multiple worker
/// threads) roughly every 10% of photos warped.
pub fn compose<Pth: AsRef<Path> + Sync>(
    image_paths: &[Pth],
    image_sizes: &[(f64, f64)],
    poses: &[Option<Pose>],
    meters_per_pixel: f64,
    on_progress: impl Fn(u8) + Sync,
) -> Result<Option<Mosaic>> {
    let mut overall: Option<Bounds> = None;
    for (&(w, h), pose) in image_sizes.iter().zip(poses) {
        let Some(pose) = pose else { continue };
        let b = image_bounds(pose, w as i32, h as i32);
        overall = Some(match overall {
            None => b,
            Some(o) => Bounds {
                min_e: o.min_e.min(b.min_e),
                max_e: o.max_e.max(b.max_e),
                min_n: o.min_n.min(b.min_n),
                max_n: o.max_n.max(b.max_n),
            },
        });
    }
    let Some(bounds) = overall else { return Ok(None) };

    let out_w = (((bounds.max_e - bounds.min_e) / meters_per_pixel).ceil().max(1.0)) as i32;
    let out_h = (((bounds.max_n - bounds.min_n) / meters_per_pixel).ceil().max(1.0)) as i32;

    // World (east,north) -> raster pixel (x right, y down; north-up raster).
    let world_to_raster: [[f64; 3]; 3] = [
        [1.0 / meters_per_pixel, 0.0, -bounds.min_e / meters_per_pixel],
        [0.0, -1.0 / meters_per_pixel, bounds.max_n / meters_per_pixel],
        [0.0, 0.0, 1.0],
    ];

    let mut placements = Vec::new();
    for (path, (&(w, h), pose)) in image_paths.iter().zip(image_sizes.iter().zip(poses)) {
        let Some(pose) = pose else { continue };
        let rect = match raster_rect(&image_bounds(pose, w as i32, h as i32), &world_to_raster, out_w, out_h) {
            Some(r) => r,
            None => continue,
        };
        let pixel_to_raster = matmul3(&world_to_raster, &pose.0);
        let translate = [[1.0, 0.0, -rect.x as f64], [0.0, 1.0, -rect.y as f64], [0.0, 0.0, 1.0]];
        placements.push(Placement { path, rect, pixel_to_roi: matmul3(&translate, &pixel_to_raster), source_size: (w as i32, h as i32) });
    }
    if placements.is_empty() {
        return Ok(None);
    }

    // The heaviest step, and the one that scales with photo count - so it
    // runs in parallel, one independent ROI-sized buffer pair per photo.
    let warped: Vec<Result<(Rect, Mat, Mat)>> = par_map_with_progress(
        placements,
        |p| -> Result<(Rect, Mat, Mat)> {
            let img = imgcodecs::imread(p.path.as_ref(), imgcodecs::IMREAD_COLOR)?;
            let pixel_to_roi = array3_to_mat(&p.pixel_to_roi)?;
            let roi_size = Size::new(p.rect.width, p.rect.height);

            let mut warped_img = Mat::default();
            imgproc::warp_perspective(&img, &mut warped_img, &pixel_to_roi, roi_size, imgproc::INTER_LINEAR, BORDER_CONSTANT, Scalar::all(0.0))?;

            let weights = center_distance_weights(p.source_size.0, p.source_size.1)?;
            let mut warped_weights = Mat::default();
            imgproc::warp_perspective(&weights, &mut warped_weights, &pixel_to_roi, roi_size, imgproc::INTER_LINEAR, BORDER_CONSTANT, Scalar::all(0.0))?;

            Ok((p.rect, warped_img, warped_weights))
        },
        on_progress,
    );

    let mut accum = Mat::new_rows_cols_with_default(out_h, out_w, CV_32FC3, Scalar::all(0.0))?;
    let mut weight_sum = Mat::new_rows_cols_with_default(out_h, out_w, CV_32FC1, Scalar::all(0.0))?;

    for result in warped {
        let (rect, warped_img, warped_weights) = result?;
        let mut accum_roi = accum.roi_mut(rect)?;
        let mut weight_roi = weight_sum.roi_mut(rect)?;
        accumulate(&mut accum_roi, &mut weight_roi, &warped_img, &warped_weights)?;
    }

    let image = normalize(&accum, &weight_sum, out_w, out_h)?;
    Ok(Some(Mosaic { image, origin_east: bounds.min_e, origin_north: bounds.max_n, meters_per_pixel }))
}

fn accumulate(accum: &mut impl MatTrait, weight_sum: &mut impl MatTrait, warped: &impl MatTraitConst, weights: &impl MatTraitConst) -> Result<()> {
    let (h, w) = (accum.rows(), accum.cols());
    for y in 0..h {
        for x in 0..w {
            let wt = *weights.at_2d::<f32>(y, x)?;
            if wt <= 0.0 {
                continue;
            }
            let px = *warped.at_2d::<Vec3b>(y, x)?;
            let acc = accum.at_2d_mut::<Vec3f>(y, x)?;
            for c in 0..3 {
                acc[c] += px[c] as f32 * wt;
            }
            *weight_sum.at_2d_mut::<f32>(y, x)? += wt;
        }
    }
    Ok(())
}

fn normalize(accum: &Mat, weight_sum: &Mat, w: i32, h: i32) -> Result<Mat> {
    let mut out = Mat::new_rows_cols_with_default(h, w, CV_8UC3, Scalar::all(0.0))?;
    for y in 0..h {
        for x in 0..w {
            let wt = *weight_sum.at_2d::<f32>(y, x)?;
            if wt <= 0.0 {
                continue;
            }
            let acc = *accum.at_2d::<Vec3f>(y, x)?;
            let dst = out.at_2d_mut::<Vec3b>(y, x)?;
            for c in 0..3 {
                dst[c] = (acc[c] / wt).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn solid_image_file(dir: &Path, name: &str, size: i32, color: (u8, u8, u8)) -> PathBuf {
        let mut img = Mat::new_rows_cols_with_default(size, size, CV_8UC3, Scalar::all(0.0)).unwrap();
        for y in 0..size {
            for x in 0..size {
                *img.at_2d_mut::<Vec3b>(y, x).unwrap() = Vec3b::from([color.0, color.1, color.2]);
            }
        }
        let path = dir.join(name);
        imgcodecs::imwrite_def(&path, &img).unwrap();
        path
    }

    fn identity_pose(mpp: f64) -> Pose {
        // Image pixel -> world meters, 1:1 with `mpp` meters/pixel, no rotation.
        Pose([[mpp, 0.0, 0.0], [0.0, -mpp, 0.0], [0.0, 0.0, 1.0]])
    }

    #[test]
    fn single_image_round_trips_through_the_mosaic() {
        let dir = tempfile::tempdir().unwrap();
        let path = solid_image_file(dir.path(), "a.png", 50, (10, 20, 30));
        let mosaic = compose(&[path], &[(50.0, 50.0)], &[Some(identity_pose(1.0))], 1.0, |_| {}).unwrap().expect("mosaic");

        assert_eq!(mosaic.image.cols(), 50);
        assert_eq!(mosaic.image.rows(), 50);
        let center = *mosaic.image.at_2d::<Vec3b>(25, 25).unwrap();
        assert_eq!(center, Vec3b::from([10, 20, 30]));
    }

    #[test]
    fn no_poses_produces_no_mosaic() {
        let dir = tempfile::tempdir().unwrap();
        let path = solid_image_file(dir.path(), "a.png", 10, (1, 2, 3));
        assert!(compose(&[path], &[(10.0, 10.0)], &[None], 1.0, |_| {}).unwrap().is_none());
    }

    #[test]
    fn two_offset_images_each_land_in_their_own_part_of_the_canvas() {
        // Two non-overlapping 10x10 photos placed 20m apart - each should
        // only paint its own half of the canvas, proving the per-photo ROI
        // crop doesn't misplace or truncate a photo relative to the others.
        let dir = tempfile::tempdir().unwrap();
        let a = solid_image_file(dir.path(), "a.png", 10, (200, 0, 0));
        let b = solid_image_file(dir.path(), "b.png", 10, (0, 200, 0));

        let pose_a = Pose([[1.0, 0.0, 0.0], [0.0, -1.0, 0.0], [0.0, 0.0, 1.0]]);
        let pose_b = Pose([[1.0, 0.0, 20.0], [0.0, -1.0, 0.0], [0.0, 0.0, 1.0]]);

        let mosaic = compose(&[a, b], &[(10.0, 10.0), (10.0, 10.0)], &[Some(pose_a), Some(pose_b)], 1.0, |_| {})
            .unwrap()
            .expect("mosaic");

        assert_eq!(mosaic.image.cols(), 30);
        let left = *mosaic.image.at_2d::<Vec3b>(5, 5).unwrap();
        let right = *mosaic.image.at_2d::<Vec3b>(5, 25).unwrap();
        assert_eq!(left, Vec3b::from([200, 0, 0]));
        assert_eq!(right, Vec3b::from([0, 200, 0]));
    }
}
