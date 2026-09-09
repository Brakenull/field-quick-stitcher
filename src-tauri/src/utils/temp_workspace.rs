//! A per-run scratch directory for downsampled images. Quick Stitch streams
//! images through this cache instead of holding every downsampled photo in
//! RAM at once - a few hundred photos at ~1600px would otherwise add up to
//! multiple GB resident, which is exactly the out-of-memory risk downsampling
//! is meant to avoid in the first place.

use std::io;
use std::path::PathBuf;

pub struct TempWorkspace {
    dir: tempfile::TempDir,
}

impl TempWorkspace {
    pub fn new() -> io::Result<Self> {
        let dir = tempfile::Builder::new().prefix("field-stitch-").tempdir()?;
        Ok(Self { dir })
    }

    /// The cache file path for the `index`-th photo in a run. Stable for a
    /// given index so callers can address images by position without keeping
    /// the path around separately.
    pub fn path_for(&self, index: usize) -> PathBuf {
        self.dir.path().join(format!("{index:06}.jpg"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn different_indices_get_different_paths_under_the_same_workspace() {
        let workspace = TempWorkspace::new().expect("create workspace");
        let a = workspace.path_for(0);
        let b = workspace.path_for(1);
        assert_ne!(a, b);
        assert!(a.starts_with(workspace.dir.path()));
    }
}
