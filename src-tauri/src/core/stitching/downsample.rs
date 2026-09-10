//! Parallel image downsampling: shrinks full-resolution drone JPEGs down to a
//! manageable size before feature extraction, per the Quick Stitch spec's
//! 80-90% pixel-count reduction step (long edge ~1200-1920px). Writes each
//! result to the run's `TempWorkspace` rather than returning it in memory, so
//! the pipeline never holds more than one full downsampled image at a time.

use std::fmt;
use std::path::{Path, PathBuf};

use opencv::core::{Mat, Size};
use opencv::prelude::*;
use opencv::{imgcodecs, imgproc};

use crate::utils::temp_workspace::TempWorkspace;
use crate::utils::thread_pool::par_map_with_progress;

/// Target long-edge size in pixels after downsampling.
pub const TARGET_LONG_EDGE_PX: i32 = 1600;

#[derive(Debug)]
pub struct DownsampleError(pub String);

impl fmt::Display for DownsampleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A downsampled image cached on disk, plus its size.
pub struct CachedImage {
    pub path: PathBuf,
    pub width: i32,
    pub height: i32,
}

/// Loads and downsamples one image, preserving aspect ratio so the longer edge
/// becomes [`TARGET_LONG_EDGE_PX`] (a no-op resize if already smaller), then
/// writes the result to `output_path`.
pub fn downsample_one(input_path: &Path, output_path: &Path) -> Result<CachedImage, DownsampleError> {
    let img = imgcodecs::imread(input_path, imgcodecs::IMREAD_COLOR)
        .map_err(|e| DownsampleError(format!("failed to read {}: {e}", input_path.display())))?;
    if img.empty() {
        return Err(DownsampleError(format!("failed to decode {}", input_path.display())));
    }

    let (w, h) = (img.cols(), img.rows());
    let long_edge = w.max(h);
    let result = if long_edge <= TARGET_LONG_EDGE_PX {
        img
    } else {
        let scale = TARGET_LONG_EDGE_PX as f64 / long_edge as f64;
        let new_size = Size::new((w as f64 * scale).round() as i32, (h as f64 * scale).round() as i32);
        let mut resized = Mat::default();
        imgproc::resize(&img, &mut resized, new_size, 0.0, 0.0, imgproc::INTER_AREA)
            .map_err(|e| DownsampleError(format!("failed to resize {}: {e}", input_path.display())))?;
        resized
    };

    let (width, height) = (result.cols(), result.rows());
    imgcodecs::imwrite_def(output_path, &result)
        .map_err(|e| DownsampleError(format!("failed to write {}: {e}", output_path.display())))?;

    Ok(CachedImage { path: output_path.to_path_buf(), width, height })
}

/// Downsamples every path in parallel into `workspace`, reporting coarse
/// progress via `on_progress`. Per-photo failures are kept as `Err` in the
/// output rather than aborting the batch, so the pipeline can still stitch
/// whatever loaded successfully. Output order matches `input_paths`.
pub fn downsample_all<P>(input_paths: Vec<PathBuf>, workspace: &TempWorkspace, on_progress: P) -> Vec<Result<CachedImage, DownsampleError>>
where
    P: Fn(u8) + Sync,
{
    let indexed: Vec<(usize, PathBuf)> = input_paths.into_iter().enumerate().collect();
    par_map_with_progress(indexed, |(i, input)| downsample_one(&input, &workspace.path_for(i)), on_progress)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_test_jpeg(path: &Path, width: u32, height: u32) {
        use image::{ImageBuffer, Rgb};
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
            ImageBuffer::from_fn(width, height, |x, y| Rgb([(x % 256) as u8, (y % 256) as u8, 128]));
        img.save(path).expect("write test jpeg");
    }

    #[test]
    fn downsamples_large_image_to_target_long_edge() {
        let dir = tempfile::tempdir().expect("tempdir");
        let input = dir.path().join("big.jpg");
        write_test_jpeg(&input, 4000, 3000);
        let output = dir.path().join("out.jpg");

        let result = downsample_one(&input, &output).expect("downsample");
        assert_eq!(result.width, TARGET_LONG_EDGE_PX);
        assert_eq!(result.height, (3000.0 * TARGET_LONG_EDGE_PX as f64 / 4000.0).round() as i32, "aspect ratio should be preserved");
        assert!(output.exists(), "downsampled image should be written to disk");

        let reloaded = imgcodecs::imread(&output, imgcodecs::IMREAD_COLOR).unwrap();
        assert_eq!(reloaded.cols(), TARGET_LONG_EDGE_PX);
    }

    #[test]
    fn leaves_small_image_untouched() {
        let dir = tempfile::tempdir().expect("tempdir");
        let input = dir.path().join("small.jpg");
        write_test_jpeg(&input, 320, 240);
        let output = dir.path().join("out.jpg");

        let result = downsample_one(&input, &output).expect("downsample");
        assert_eq!(result.width, 320);
        assert_eq!(result.height, 240);
    }

    #[test]
    fn missing_file_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().expect("tempdir");
        let output = dir.path().join("out.jpg");
        let result = downsample_one(Path::new("does-not-exist.jpg"), &output);
        assert!(result.is_err());
    }

    #[test]
    fn downsample_all_preserves_input_order_and_writes_into_the_workspace() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut inputs = Vec::new();
        for (i, size) in [(4000u32, 3000u32), (320, 240)].into_iter().enumerate() {
            let path = dir.path().join(format!("in_{i}.jpg"));
            write_test_jpeg(&path, size.0, size.1);
            inputs.push(path);
        }

        let workspace = TempWorkspace::new().expect("workspace");
        let results = downsample_all(inputs, &workspace, |_| {});
        assert_eq!(results.len(), 2);
        let first = results[0].as_ref().expect("first ok");
        let second = results[1].as_ref().expect("second ok");
        assert_eq!(first.width, TARGET_LONG_EDGE_PX, "large image should have been downsampled");
        assert_eq!(second.width, 320, "small image should be untouched");
        assert!(first.path.exists());
        assert!(second.path.exists());
    }
}
