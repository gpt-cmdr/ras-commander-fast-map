//! The engine's use of terrio's process-wide thread pool.
//!
//! terrio (>= 0.5) runs all of its internal parallel work (strip/tile encoding of
//! multi-chunk writes, warping, rasterizing in stripes, multi-chunk decoding) on
//! one pool per process, `terrio-par-*`. It starts that pool on first use, sized
//! by `RAYON_NUM_THREADS` (else one thread per core), and offers no native API to
//! size, replace or bypass it; it never runs on the caller's rayon pool. A request
//! pool calling into it therefore cannot account for terrio's threads: a rayon
//! worker waiting on another pool keeps running its own pool's jobs, so a request
//! of `N` threads could keep about `2N` threads busy.
//!
//! The engine bounds this structurally:
//!
//! - terrio's pool has **one** thread ([`TERRIO_POOL_THREADS`]). The executable
//!   sets `RAYON_NUM_THREADS=1` before any thread starts (the engine's own pools
//!   always set their size, so the variable sizes only terrio's pool and rayon's
//!   unused global pool). With one thread, rasterizing is a single stripe and
//!   single-block reads a single chunk, both run on the calling thread; terrio's
//!   multi-piece work (warps, multi-chunk writes) runs on that one thread.
//! - [`call`] runs every engine call into terrio that can reach its pool on a
//!   short-lived helper thread while the caller blocks in `join` (for a request
//!   worker, an OS wait, not a rayon wait that would take other jobs). So whenever terrio's thread is busy, the worker that asked for
//!   it is not, and a request never has more busy threads than its `threads`.
//! - terrio's process-wide decoded-block cache is turned off ([`prepare`]): the
//!   engine keeps decoded terrain blocks itself, under its own byte budgets, and
//!   an inherited `TERRIO_CACHEMAX`/`GDAL_CACHEMAX` cannot add unplanned memory.
//!
//! Library callers that do not run the executable's `main` must set
//! `RAYON_NUM_THREADS=1` in their process before terrio is first used (for
//! example before starting threads); [`status`] reports the size terrio's pool
//! was given when the engine first used it. Without it, terrio's pool has one
//! thread per core and a call can add that many busy threads.
use serde_json::{Value, json};
use std::sync::OnceLock;

/// The size of terrio's pool the engine's thread budget relies on.
pub const TERRIO_POOL_THREADS: usize = 1;

/// The environment variable that sizes terrio's pool when it starts.
pub const POOL_ENV: &str = "RAYON_NUM_THREADS";

/// terrio's pool size as configured when the engine first prepared terrio.
static STARTED: OnceLock<usize> = OnceLock::new();

/// The pool size `RAYON_NUM_THREADS` gives (rayon's rule: a positive integer,
/// else one thread per core).
fn configured_threads() -> usize {
    std::env::var(POOL_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1)
        })
}

/// Turns terrio's decoded-block cache off and starts terrio's pool on the
/// calling thread without reading any payload (an empty in-memory rasterize):
/// started from several request workers at once, terrio would start and discard
/// extra pools. Cheap after the first call; the cache size is set every time, as
/// a library caller could have changed it.
pub fn prepare() {
    terrio::block_cache::set_max_bytes(0);
    STARTED.get_or_init(|| {
        let threads = configured_threads();
        start_pool();
        threads
    });
}

fn start_pool() {
    use terrio::ops::rasterize::{Grid, RasterizeOptions, rasterize};
    let options = RasterizeOptions {
        burn_values: vec![1.0],
        grid: Some(Grid {
            geo_transform: [0.0, 1.0, 0.0, 1.0, 0.0, -1.0],
            cols: 1,
            rows: 1,
            crs: Default::default(),
        }),
        data_type: Some(wbraster::DataType::U8),
        ..Default::default()
    };
    // Any outcome is fine: only the pool start (`par::threads`) matters.
    let _ = rasterize(&[wbvector::Layer::new("start")], &options);
}

/// Stack of the helper thread: terrio's encoders and warper need more than a
/// Windows main thread's 1 MiB in unoptimized builds.
const HELPER_STACK: usize = 16 << 20;

/// Runs `f`, a call into terrio that may use terrio's pool, on a helper thread
/// while the calling thread blocks in `join`. On a request-pool worker this is
/// the point: an OS wait takes no other jobs, where a rayon wait on terrio's
/// pool would keep running the request's. Results and panics are those of `f`.
pub(crate) fn call<R: Send>(f: impl FnOnce() -> R + Send) -> R {
    prepare();
    let mut f = Some(f);
    let slot = &mut f;
    let outcome = std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("fast-map-terrio".into())
            .stack_size(HELPER_STACK)
            .spawn_scoped(scope, move || (slot.take().expect("call runs once"))())
            .map(|handle| handle.join())
    });
    match outcome {
        Ok(Ok(value)) => value,
        Ok(Err(panic)) => std::panic::resume_unwind(panic),
        // No thread could be started: run here (the closure did not run).
        Err(_) => (f.take().expect("call did not run"))(),
    }
}

/// The thread-budget contract (static, for discovery records).
pub fn contract() -> Value {
    json!({
        "terrio_pool_threads": TERRIO_POOL_THREADS,
        "sized_by": POOL_ENV,
        "executable_sets": format!("{POOL_ENV}={TERRIO_POOL_THREADS} before any thread starts, replacing an inherited value"),
        "library_callers": format!("set {POOL_ENV}={TERRIO_POOL_THREADS} before terrio is first used"),
        "terrio_calls": "calls that can reach terrio's pool run off request workers (helper thread; the worker blocks without taking other jobs)",
        "terrio_block_cache_bytes": 0,
        "busy_threads": "at most the request's threads",
    })
}

/// terrio's pool size as the engine found it when it first prepared terrio
/// (preparing terrio now if it has not yet), and the block cache size.
pub fn status() -> Value {
    prepare();
    let threads = STARTED.get().copied();
    json!({
        "terrio_pool_threads": threads,
        "bounded": threads.map(|t| t <= TERRIO_POOL_THREADS),
        "block_cache_bytes": terrio::block_cache::max_bytes(),
    })
}
