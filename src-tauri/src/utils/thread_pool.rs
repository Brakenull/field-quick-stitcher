//! Thin rayon wrapper that reports coarse (~10%) progress while parallel-mapping
//! over a batch, so the UI can show a progress bar without per-file event spam.

use std::sync::atomic::{AtomicUsize, Ordering};

use rayon::prelude::*;

/// Runs `f` over `items` in parallel, calling `on_progress(percent)` roughly every
/// 10% of completion (and always once at 100%). `on_progress` may be called
/// concurrently from multiple worker threads.
pub fn par_map_with_progress<T, R, F, P>(items: Vec<T>, f: F, on_progress: P) -> Vec<R>
where
    T: Send,
    R: Send,
    F: Fn(T) -> R + Sync,
    P: Fn(u8) + Sync,
{
    let total = items.len().max(1);
    let done = AtomicUsize::new(0);
    let last_reported = AtomicUsize::new(0);

    items
        .into_par_iter()
        .map(|item| {
            let result = f(item);
            let completed = done.fetch_add(1, Ordering::Relaxed) + 1;
            let percent = (completed * 100) / total;
            let last = last_reported.load(Ordering::Relaxed);
            if percent >= last + 10 || completed == total {
                if last_reported
                    .compare_exchange(last, percent, Ordering::Relaxed, Ordering::Relaxed)
                    .is_ok()
                {
                    on_progress(percent as u8);
                }
            }
            result
        })
        .collect()
}
