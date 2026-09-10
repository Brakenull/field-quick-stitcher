//! Quick Stitch pipeline (Phase 2): downsample -> GPS-guided neighbor matching ->
//! ORB features -> RANSAC homography -> pose-graph alignment -> blend -> GeoTIFF.
//! See `.claude/phase/quick-stitch.md` for the design this follows.

pub mod blending;
pub mod downsample;
pub mod features;
pub mod geotiff_export;
pub mod homography;
pub mod matching;
pub mod mosaic;
pub mod neighbor_index;
pub mod onnx_matcher;
pub mod pipeline;
pub mod pose_graph;
