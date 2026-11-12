//! Thread budget of the executable, measured from outside on Linux and Windows.
//!
//! Each workload runs `ras2fim-batch` (or, for concurrent terrio calls, a child
//! test process) on generated inputs large enough for terrio to split its work,
//! while this test samples the child's threads every few hundred microseconds:
//!
//! - the threads named `terrio-par-*` (terrio's process-wide pool): at most one;
//! - CPU time per thread (Linux `/proc/<pid>/task/*/schedstat`, nanoseconds;
//!   Windows `QueryThreadCycleTime`, cycles calibrated against the wall clock),
//!   each thread's share capped at the wall time between samples, from which
//!   the CPU the child used over every `WINDOW` of wall time gives its
//!   busy threads: never more than the request's `threads` (plus `TOLERANCE`
//!   for sampling skew);
//! - for the record, OS threads and threads running or ready (Linux `R`; Windows
//!   Running/Ready/Standby/DeferredReady). Instantaneous states also count a
//!   rayon worker's idle spin and threads waiting for a CPU, so they are
//!   reported, not asserted.
//!
//! Busy threads are asserted in optimized builds (CI runs this file with
//! `--release` on Linux and Windows); unoptimized, rayon's idle spin between
//! jobs is long enough to show, so they are only reported.
//!
//! The first `STARTUP` of the child is not sampled: on Windows the loader's worker
//! threads run beside `main` while the process starts.
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

/// The executable under test (`FAST_MAP_THREAD_BUDGET_BIN` measures another build).
fn bin() -> PathBuf {
    std::env::var_os("FAST_MAP_THREAD_BUDGET_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_ras2fim-batch")))
}

/// The tests of this file run one at a time, so that a sibling's child does not
/// compete for the CPUs a sampled child's threads should be running on.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// Process start, not sampled.
const STARTUP: Duration = Duration::from_millis(30);
/// Wall time over which CPU use is averaged into busy threads.
const WINDOW: Duration = Duration::from_millis(10);
/// Busy threads tolerated above the budget: per-thread reads are not one instant.
const TOLERANCE: f64 = 0.2;

// ---------------------------------------------------------------- sampling

/// One thread at one sample.
struct ThreadSample {
    tid: u64,
    /// Running or ready.
    running: bool,
    /// CPU time used so far, nanoseconds.
    cpu_ns: f64,
}

#[cfg(target_os = "linux")]
struct Sampler;

#[cfg(target_os = "linux")]
impl Sampler {
    fn new() -> Self {
        Self
    }

    fn sample(&mut self, pid: u32) -> Option<Vec<ThreadSample>> {
        let mut threads = Vec::new();
        for entry in fs::read_dir(format!("/proc/{pid}/task")).ok()?.flatten() {
            let Some(tid) = entry.file_name().to_str().and_then(|s| s.parse().ok()) else {
                continue;
            };
            let stat = fs::read_to_string(entry.path().join("stat")).unwrap_or_default();
            // "tid (comm) S ...": the state follows the last ')'.
            let running = stat
                .rsplit_once(')')
                .is_some_and(|(_, rest)| rest.trim_start().starts_with('R'));
            let Some(cpu_ns) = fs::read_to_string(entry.path().join("schedstat"))
                .ok()
                .and_then(|s| s.split_whitespace().next()?.parse().ok())
            else {
                continue;
            };
            threads.push(ThreadSample {
                tid,
                running,
                cpu_ns,
            });
        }
        Some(threads)
    }

    fn name(&mut self, pid: u32, tid: u64) -> String {
        fs::read_to_string(format!("/proc/{pid}/task/{tid}/comm"))
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    }
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;
    #[link(name = "ntdll", kind = "raw-dylib")]
    unsafe extern "system" {
        pub fn NtQuerySystemInformation(
            class: u32,
            buffer: *mut c_void,
            length: u32,
            returned: *mut u32,
        ) -> i32;
    }
    #[link(name = "kernel32", kind = "raw-dylib")]
    unsafe extern "system" {
        pub fn OpenThread(access: u32, inherit: i32, tid: u32) -> *mut c_void;
        pub fn CloseHandle(handle: *mut c_void) -> i32;
        pub fn GetThreadDescription(handle: *mut c_void, out: *mut *mut u16) -> i32;
        pub fn LocalFree(memory: *mut c_void) -> *mut c_void;
        pub fn QueryThreadCycleTime(handle: *mut c_void, cycles: *mut u64) -> i32;
        pub fn GetCurrentThread() -> *mut c_void;
    }
}

#[cfg(windows)]
struct Sampler {
    handles: HashMap<u64, *mut std::ffi::c_void>,
    ns_per_cycle: f64,
    buffer: Vec<u64>,
}

#[cfg(windows)]
impl Sampler {
    fn new() -> Self {
        // Cycles this thread counts per nanosecond of wall time while it spins.
        // A spin the scheduler interrupts counts fewer cycles, so the fastest of
        // several short spins is the clock's rate.
        let cycles = || {
            let mut c = 0;
            // SAFETY: the pseudo-handle of the calling thread.
            unsafe { win::QueryThreadCycleTime(win::GetCurrentThread(), &mut c) };
            c
        };
        let mut ns_per_cycle = f64::INFINITY;
        for _ in 0..20 {
            let (c0, t0) = (cycles(), Instant::now());
            let mut x = 1.0f64;
            while t0.elapsed() < Duration::from_millis(2) {
                x = std::hint::black_box(x * 1.000001);
            }
            let rate = t0.elapsed().as_nanos() as f64 / (cycles() - c0).max(1) as f64;
            ns_per_cycle = ns_per_cycle.min(rate);
        }
        Self {
            handles: HashMap::new(),
            ns_per_cycle,
            buffer: vec![0; 1 << 18],
        }
    }
}

#[cfg(windows)]
impl Drop for Sampler {
    fn drop(&mut self) {
        for h in self.handles.values() {
            if !h.is_null() {
                // SAFETY: handles opened by `handle`.
                unsafe { win::CloseHandle(*h) };
            }
        }
    }
}

#[cfg(windows)]
impl Sampler {
    fn handle(&mut self, tid: u64) -> *mut std::ffi::c_void {
        *self.handles.entry(tid).or_insert_with(|| {
            // SAFETY: plain Win32 call; THREAD_QUERY_LIMITED_INFORMATION.
            unsafe { win::OpenThread(0x0800, 0, tid as u32) }
        })
    }

    fn sample(&mut self, pid: u32) -> Option<Vec<ThreadSample>> {
        // SystemProcessInformation (5). x64 layouts: SYSTEM_PROCESS_INFORMATION has
        // NextEntryOffset@0, NumberOfThreads@4, UniqueProcessId@80 and its threads
        // from @256; each SYSTEM_THREAD_INFORMATION is 80 bytes, with
        // ClientId.UniqueThread@48 and ThreadState@68.
        assert_eq!(std::mem::size_of::<usize>(), 8, "x64 layout");
        loop {
            let mut returned = 0u32;
            let bytes = (self.buffer.len() * 8) as u32;
            // SAFETY: the buffer is writable for `bytes` bytes.
            let status = unsafe {
                win::NtQuerySystemInformation(
                    5,
                    self.buffer.as_mut_ptr().cast(),
                    bytes,
                    &mut returned,
                )
            };
            if status == 0 {
                break;
            }
            if status as u32 == 0xC000_0004 {
                self.buffer = vec![0; (returned as usize).max(bytes as usize * 2) / 8 + 4096];
                continue;
            }
            return None;
        }
        let mut found = Vec::new();
        {
            // SAFETY: the u64 buffer viewed as bytes, within its length.
            let bytes: &[u8] = unsafe {
                std::slice::from_raw_parts(self.buffer.as_ptr().cast(), self.buffer.len() * 8)
            };
            let u32_at = |o: usize| u32::from_le_bytes(bytes[o..o + 4].try_into().unwrap());
            let u64_at = |o: usize| u64::from_le_bytes(bytes[o..o + 8].try_into().unwrap());
            let mut offset = 0;
            loop {
                let next = u32_at(offset) as usize;
                if u64_at(offset + 80) == pid as u64 {
                    for i in 0..u32_at(offset + 4) as usize {
                        let t = offset + 256 + i * 80;
                        // 1 Ready, 2 Running, 3 Standby, 7 DeferredReady.
                        found.push((u64_at(t + 48), matches!(u32_at(t + 68), 1 | 2 | 3 | 7)));
                    }
                    break;
                }
                if next == 0 {
                    return None;
                }
                offset += next;
            }
        }
        let mut threads = Vec::new();
        for (tid, running) in found {
            let handle = self.handle(tid);
            let mut cycles = 0u64;
            // SAFETY: a thread handle opened with query rights (null is skipped).
            if handle.is_null() || unsafe { win::QueryThreadCycleTime(handle, &mut cycles) } == 0 {
                continue;
            }
            threads.push(ThreadSample {
                tid,
                running,
                cpu_ns: cycles as f64 * self.ns_per_cycle,
            });
        }
        Some(threads)
    }

    fn name(&mut self, _pid: u32, tid: u64) -> String {
        let handle = self.handle(tid);
        if handle.is_null() {
            return String::new();
        }
        // SAFETY: plain Win32 calls on an open handle; the text is freed here.
        unsafe {
            let mut text: *mut u16 = std::ptr::null_mut();
            let mut name = String::new();
            if win::GetThreadDescription(handle, &mut text) >= 0 && !text.is_null() {
                let mut len = 0;
                while *text.add(len) != 0 {
                    len += 1;
                }
                name = String::from_utf16_lossy(std::slice::from_raw_parts(text, len));
                win::LocalFree(text.cast());
            }
            name
        }
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
struct Sampler;

#[cfg(not(any(target_os = "linux", windows)))]
impl Sampler {
    fn new() -> Self {
        Self
    }
    fn sample(&mut self, _pid: u32) -> Option<Vec<ThreadSample>> {
        None
    }
    fn name(&mut self, _pid: u32, _tid: u64) -> String {
        String::new()
    }
}

#[derive(Debug, Default)]
struct Trace {
    samples: usize,
    max_os_threads: usize,
    /// Most threads running or ready in one sample (reported, not asserted).
    max_running: usize,
    /// Most CPU used over any `WINDOW`, in threads.
    max_busy: f64,
    /// CPU used over the whole sampled run, in threads.
    mean_busy: f64,
    terrio_threads: usize,
    terrio_running_max: usize,
    histogram: BTreeMap<usize, usize>,
    seconds: f64,
}

fn trace(child: &mut Child) -> Trace {
    let pid = child.id();
    let started = Instant::now();
    let mut sampler = Sampler::new();
    let mut t = Trace::default();
    let mut names: HashMap<u64, String> = HashMap::new();
    let mut last: HashMap<u64, f64> = HashMap::new();
    // (seconds, cumulative CPU nanoseconds of threads seen in consecutive samples).
    let mut history: Vec<(f64, f64)> = Vec::new();
    let mut cumulative = 0.0;
    let mut snapshots: Vec<HashMap<u64, f64>> = Vec::new();
    while child.try_wait().unwrap().is_none() {
        // The first milliseconds belong to process start (on Windows, the loader's
        // worker threads run beside `main`), before any request work.
        if started.elapsed() < STARTUP {
            std::thread::sleep(Duration::from_micros(300));
            continue;
        }
        if let Some(threads) = sampler.sample(pid) {
            let now = started.elapsed().as_secs_f64();
            // One thread uses at most the wall time between two samples: this
            // bounds a clock conversion that overstates (Windows cycle counts
            // on a host whose cycle rate varies).
            let interval_ns = history.last().map_or(0.0, |h| (now - h.0) * 1e9);
            let mut running = 0;
            let mut terrio_running = 0;
            let mut next = HashMap::new();
            for th in &threads {
                // A new thread names itself shortly after it starts: look again
                // while its name is still empty.
                let name = names.entry(th.tid).or_default();
                if name.is_empty() {
                    *name = sampler.name(pid, th.tid);
                }
                running += th.running as usize;
                terrio_running += (th.running && name.starts_with("terrio-par")) as usize;
                if let Some(before) = last.get(&th.tid) {
                    cumulative += (th.cpu_ns - before).clamp(0.0, interval_ns);
                }
                next.insert(th.tid, th.cpu_ns);
            }
            if std::env::var_os("FAST_MAP_THREAD_BUDGET_DEBUG").is_some() {
                snapshots.push(next.clone());
            }
            last = next;
            history.push((now, cumulative));
            t.samples += 1;
            t.max_os_threads = t.max_os_threads.max(threads.len());
            t.max_running = t.max_running.max(running);
            t.terrio_running_max = t.terrio_running_max.max(terrio_running);
            *t.histogram.entry(running).or_default() += 1;
        }
        std::thread::sleep(Duration::from_micros(300));
    }
    // Busy threads over every span of at least `WINDOW` ending at a sample.
    let window = WINDOW.as_secs_f64();
    let mut j = 0;
    for i in 0..history.len() {
        while j + 1 < i && history[i].0 - history[j + 1].0 >= window {
            j += 1;
        }
        let span = history[i].0 - history[j].0;
        if span >= window {
            let busy = (history[i].1 - history[j].1) / (span * 1e9);
            if busy > t.max_busy && !snapshots.is_empty() {
                let mut per: Vec<(String, f64)> = snapshots[i]
                    .iter()
                    .filter_map(|(tid, c)| {
                        let b = snapshots[j].get(tid)?;
                        Some((
                            format!("{tid}:{}", names.get(tid).cloned().unwrap_or_default()),
                            (c - b) / (span * 1e9),
                        ))
                    })
                    .filter(|x| x.1 > 0.01)
                    .collect();
                per.sort_by(|a, b| b.1.total_cmp(&a.1));
                eprintln!("  window at {:.3}s busy {busy:.2}: {per:?}", history[i].0);
            }
            t.max_busy = t.max_busy.max(busy);
        }
    }
    if let (Some(first), Some(end)) = (history.first(), history.last())
        && end.0 > first.0
    {
        t.mean_busy = (end.1 - first.1) / ((end.0 - first.0) * 1e9);
    }
    t.terrio_threads = names
        .values()
        .filter(|n| n.starts_with("terrio-par"))
        .count();
    t.seconds = started.elapsed().as_secs_f64();
    t
}

/// The budget checks of one sampled run.
fn assert_budget(label: &str, t: &Trace, threads: usize) {
    assert!(t.samples >= 20, "{label}: too short to sample: {t:?}");
    assert!(
        t.terrio_threads <= 1,
        "{label}: terrio's pool must have one thread: {t:?}"
    );
    if cfg!(debug_assertions) {
        // Unoptimized rayon spins far longer between jobs (its idle loop is not
        // inlined), which shows as CPU above the budget: CI runs these tests
        // optimized, as shipped (see `.github/workflows/build.yml`).
        eprintln!("{label}: unoptimized build, busy threads reported only");
        return;
    }
    assert!(
        t.max_busy <= threads as f64 + TOLERANCE,
        "{label}: busy threads exceeded the request's {threads}: {t:?}"
    );
}

/// Runs `command` on `request` and checks the thread budget while it runs.
fn check(dir: &Path, command: &str, request: &Value, threads: usize) -> Trace {
    let label = format!("{command}-t{threads}");
    fs::write(dir.join(format!("{label}.json")), request.to_string()).unwrap();
    let mut child = Command::new(bin())
        .arg(command)
        .arg(dir.join(format!("{label}.json")))
        .arg(dir.join(&label))
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let t = trace(&mut child);
    let output = child.wait_with_output().unwrap();
    eprintln!("{label}: {t:?}");
    assert!(
        output.status.success(),
        "{label}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    if t.samples > 0 {
        assert_budget(&label, &t, threads);
    }
    t
}

// ---------------------------------------------------------------- inputs

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn scratch(name: &str) -> Scratch {
    let dir = std::env::temp_dir().join(format!(
        "fast-map-thread-budget-{name}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    Scratch(dir)
}

fn le<T: Copy, const N: usize>(values: &[T], to: fn(T) -> [u8; N]) -> Vec<u8> {
    values.iter().flat_map(|v| to(*v)).collect()
}

fn table(dir: &Path, name: &str, rows: usize, bytes: Vec<u8>) -> Value {
    fs::write(dir.join(format!("{name}.bin")), bytes).unwrap();
    json!({"path": format!("{name}.bin"), "rows": rows})
}

fn rectangle(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<u8> {
    let mut wkb = vec![1];
    for i in [3u32, 1, 5] {
        wkb.extend(i.to_le_bytes());
    }
    for p in [[x0, y0], [x0, y1], [x1, y1], [x1, y0], [x0, y0]] {
        for v in p {
            wkb.extend(v.to_le_bytes());
        }
    }
    let mut framed = (wkb.len() as u32).to_le_bytes().to_vec();
    framed.extend(wkb);
    framed
}

/// One 2D cell over a `w` x `w` terrain window, four profiles; LZW GeoTIFFs.
fn sloping(dir: &Path, w: usize, threads: usize) -> Value {
    use ras_commander_fast_map::effective_terrain::{self, Effective, Grid};
    let cell = 4.0 / w as f64;
    let grid = Grid {
        origin_x: 0.0,
        origin_y: 4.0,
        cell_size: cell,
        width: w,
        height: w,
    };
    let values = (0..w * w).map(|i| (i % 977) as f32 * 1e-4).collect();
    let terrain = Effective {
        grid,
        values,
        report: Value::Null,
    };
    effective_terrain::write_geotiff(&dir.join("terrain.tif"), terrain, 3857).unwrap();
    let i = |v: &[i32]| le(v, i32::to_le_bytes);
    let d = |v: &[f64]| le(v, f64::to_le_bytes);
    let f = |v: &[f32]| le(v, f32::to_le_bytes);
    let arrays = json!({
        "centers": table(dir, "centers", 5, d(&[2., 2., -1., 2., 2., 5., 5., 2., 2., -1.])),
        "cell_info": table(dir, "cell_info", 5, i(&[0, 4, 4, 1, 5, 1, 6, 1, 7, 1])),
        "cell_faces": table(dir, "cell_faces", 8, i(&[0, 1, 1, 1, 2, 1, 3, 1, 0, -1, 1, -1, 2, -1, 3, -1])),
        "cell_minimum": table(dir, "cell_minimum", 5, f(&[0.; 5])),
        "face_vertices": table(dir, "face_vertices", 4, i(&[0, 1, 1, 2, 2, 3, 3, 0])),
        "face_cells": table(dir, "face_cells", 4, i(&[0, 1, 0, 2, 0, 3, 0, 4])),
        "face_info": table(dir, "face_info", 4, i(&[0; 8])),
        "face_points": table(dir, "face_points", 0, d(&[])),
        "face_minimum": table(dir, "face_minimum", 4, f(&[0.; 4])),
        "vertices": table(dir, "vertices", 4, d(&[0., 0., 0., 4., 4., 4., 4., 0.])),
        "vertex_info": table(dir, "vertex_info", 4, i(&[0, 2, 2, 2, 4, 2, 6, 2])),
        "vertex_faces": table(dir, "vertex_faces", 8, i(&[3, 1, 0, 1, 0, 1, 1, 1, 1, 1, 2, 1, 2, 1, 3, 1])),
    });
    fs::write(
        dir.join("mesh.json"),
        json!({"schema": "ras-commander-sloping-mesh/1", "arrays": arrays}).to_string(),
    )
    .unwrap();
    fs::write(dir.join("cell.wkb"), rectangle(0., 0., 4., 4.)).unwrap();
    let profiles: Vec<Value> = [0.5f32, 1.25, 0.75, 2.0]
        .iter()
        .enumerate()
        .map(|(k, v)| {
            table(dir, &format!("p{k}"), 5, f(&[*v, 0., 0., 0., 0.]));
            json!({"id": format!("p{k}"), "values_files": [format!("p{k}.bin")]})
        })
        .collect();
    json!({"schema": ras_commander_fast_map::sloping_session::SCHEMA, "terrain": "terrain.tif",
        "epsg": 3857, "crs_identity": {"kind": "epsg"}, "terrain_transform": [0., cell, 0., 4., 0., -cell],
        "window": [0, 0, w, w], "geometry_file": "cell.wkb", "mesh_files": ["mesh.json"],
        "owner_cells": [[0, 0]], "profiles": profiles, "threads": threads,
        "max_working_bytes": 4u64 << 30, "window_cache_bytes": 0,
        "stream_outputs": true, "geotiff_outputs": true, "compression": "LZW"})
}

/// Grouped maxima of the sloping model, streamed into LZW GeoTIFFs by request workers.
fn depth_groups(model: Value, w: usize) -> Value {
    json!({"schema": ras_commander_fast_map::depth_groups::SCHEMA, "model": model,
        "source_indices": [0, 1, 2, 3], "depth_units": "feet", "max_accumulator_bytes": 1u64 << 31,
        "compression": "LZW", "jobs": [
            {"job_id": "all", "reach_id": "r", "window": [0, 0, w, w], "aoi_file": "cell.wkb", "groups": [
                {"group_id": "max", "timesteps": [0, 1, 2, 3]}, {"group_id": "one", "timesteps": [1]},
                {"group_id": "two", "timesteps": [2, 3]}]}]})
}

/// Storage areas burned on a `w` x `w` grid (two full-grid ownership burns).
fn storage(dir: &Path, w: usize, threads: usize) -> Value {
    fs::write(dir.join("terrain.bin"), vec![0u8; w * w * 4]).unwrap();
    let k = 16;
    fs::write(
        dir.join("levels.bin"),
        le(&vec![5.0f32; k * k], f32::to_le_bytes),
    )
    .unwrap();
    let s = w as f64 / k as f64;
    let mut areas = Vec::new();
    for a in 0..k {
        for b in 0..k {
            let (x, y) = (a as f64 * s, b as f64 * s);
            areas.extend(rectangle(x + 0.3, y + 0.3, x + s - 0.3, y + s - 0.3));
        }
    }
    fs::write(dir.join("areas.wkb"), areas).unwrap();
    json!({"schema": "fast-map-storage/1", "storage_areas_file": "areas.wkb",
        "storage_area_count": k * k, "levels_file": "levels.bin", "terrain_file": "terrain.bin",
        "width": w, "height": w, "geo_transform": [0., 1., 0., w as f64, 0., -1.],
        "max_working_bytes": 4u64 << 30, "threads": threads.min(4)})
}

/// One tiled source; the effective terrain is written as a 256x256-tiled DEFLATE GeoTIFF.
fn effective(dir: &Path, w: usize, threads: usize) -> Value {
    use ras_commander_fast_map::effective_terrain::{self, Effective, Grid, quantize};
    let cell = 1.0 / 32.0;
    let grid = Grid {
        origin_x: 0.0,
        origin_y: w as f64 * cell,
        cell_size: cell,
        width: w,
        height: w,
    };
    let values = (0..w * w)
        .map(|i| quantize(100.0 + (i % w) as f64 * 0.01 + (i / w) as f64 * 0.02))
        .collect();
    let source = Effective {
        grid,
        values,
        report: Value::Null,
    };
    effective_terrain::write_geotiff(&dir.join("source.tif"), source, 26915).unwrap();
    let tiles = w.div_ceil(256) * w.div_ceil(256);
    fs::write(dir.join("source.u1"), vec![1u8; tiles * 65536]).unwrap();
    json!({"schema": "ras-commander-effective-terrain-request/1",
        "sources": [{"path": "source.tif", "mask": {"path": "source.u1", "dtype": "|u1", "shape": [tiles, 65536]}}],
        "epsg": 26915, "crs_identity": {"kind": "epsg"}, "modifications": 0, "threads": threads,
        "max_working_bytes": 4u64 << 30})
}

// ---------------------------------------------------------------- tests

/// Window size: large enough that terrio splits its work, small enough for debug CI.
fn size() -> usize {
    std::env::var("FAST_MAP_THREAD_BUDGET_SIZE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(if cfg!(debug_assertions) { 1024 } else { 2048 })
}

#[test]
fn sloping_and_grouped_geotiffs_stay_within_the_request_threads() {
    let _serial = serial();
    let w = size();
    for threads in [1, 3] {
        let dir = scratch(&format!("sloping-{threads}"));
        let model = sloping(&dir.0, w, threads);
        check(&dir.0, "sloping", &model, threads);
        let mut grouped_model = model;
        grouped_model["stream_outputs"] = false.into();
        grouped_model["geotiff_outputs"] = false.into();
        grouped_model.as_object_mut().unwrap().remove("compression");
        check(
            &dir.0,
            "depth-groups",
            &depth_groups(grouped_model, w),
            threads,
        );
    }
}

#[test]
fn storage_burns_stay_within_the_request_threads() {
    let _serial = serial();
    let w = size() * 2;
    for threads in [1, 3] {
        let dir = scratch(&format!("storage-{threads}"));
        let request = storage(&dir.0, w, threads);
        check(&dir.0, "storage", &request, threads);
    }
}

#[test]
fn effective_terrain_write_stays_within_the_request_threads() {
    let _serial = serial();
    if cfg!(all(windows, debug_assertions)) {
        // Unoptimized, `effective-terrain` overflows the 1 MiB stack of a Windows
        // main thread (also before terrio 0.8); CI runs this test optimized there.
        eprintln!("skipped: unoptimized Windows build");
        return;
    }
    let w = size() * 2;
    for threads in [1, 3] {
        let dir = scratch(&format!("effective-{threads}"));
        let request = effective(&dir.0, w, threads);
        check(&dir.0, "effective-terrain", &request, threads);
    }
}

/// An inherited `RAYON_NUM_THREADS` does not size terrio's pool: the executable
/// replaces it, so the budget holds whatever the caller's environment held.
#[test]
fn inherited_rayon_num_threads_does_not_widen_terrio_pool() {
    let _serial = serial();
    let dir = scratch("inherited");
    let request = storage(&dir.0, size() * 2, 3);
    fs::write(dir.0.join("inherited.json"), request.to_string()).unwrap();
    let mut child = Command::new(bin())
        .arg("storage")
        .arg(dir.0.join("inherited.json"))
        .arg(dir.0.join("inherited"))
        .env("RAYON_NUM_THREADS", "7")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let t = trace(&mut child);
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    eprintln!("inherited RAYON_NUM_THREADS=7: {t:?}");
    if t.samples > 0 {
        assert!(t.terrio_threads <= 1, "{t:?}");
    }
    // The library report records the pool the engine found.
    let report: Value =
        serde_json::from_slice(&fs::read(dir.0.join("inherited/report.json")).unwrap()).unwrap();
    assert_eq!(report["terrio_pool"]["terrio_pool_threads"], 1, "{report}");
    assert_eq!(report["terrio_pool"]["bounded"], true, "{report}");
    assert_eq!(report["terrio_pool"]["block_cache_bytes"], 0, "{report}");
    let output = Command::new(bin()).arg("method").output().unwrap();
    let record: Value = serde_json::from_slice(&output.stdout).unwrap();
    let contract = &record["engine"]["thread_budget"];
    assert_eq!(contract["terrio_pool_threads"], 1, "{contract}");
    assert_eq!(contract["terrio_block_cache_bytes"], 0, "{contract}");
}

// ------------------------------------------------- concurrent terrio calls

const CHILD: &str = "FAST_MAP_THREAD_BUDGET_CHILD";

/// The pattern a request pool has in multi-profile work: every worker alternates
/// its own computation with a terrio call (a polygon burn over a large grid).
/// Run in a child process (see below) so that its environment sizes terrio's pool.
#[test]
#[ignore = "child process of terrio_calls_from_busy_request_workers_stay_within_budget"]
fn concurrent_burn_workload() {
    if std::env::var_os(CHILD).is_none() {
        return;
    }
    use ras_commander_fast_map::ownership;
    use terrio::ops::rasterize::Grid;
    let threads: usize = std::env::var(CHILD).unwrap().parse().unwrap();
    let dir = scratch("burn-child");
    // Many small cells: most of a burn is terrio drawing them, in stripes.
    let (w, k) = (2048usize, 128usize);
    let side = w as f64 / k as f64;
    let mut cells = Vec::new();
    for a in 0..k {
        for b in 0..k {
            let (x, y) = (a as f64 * side, b as f64 * side);
            cells.extend(rectangle(x + 0.3, y + 0.3, x + side - 0.3, y + side - 0.3));
        }
    }
    fs::write(dir.0.join("cells.wkb"), cells).unwrap();
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .unwrap();
    let grid = Grid {
        geo_transform: [0., 1., 0., w as f64, 0., -1.],
        cols: w,
        rows: w,
        crs: Default::default(),
    };
    let expected = ownership::burn(
        ownership::load(&dir.0.join("cells.wkb"), k * k, false).unwrap(),
        grid.clone(),
    )
    .unwrap();
    pool.install(|| {
        use rayon::prelude::*;
        (0..16 * threads).into_par_iter().for_each(|task| {
            // The worker's own work: about 10 ms of arithmetic.
            let tick = Instant::now();
            let mut x = task as f64;
            while tick.elapsed() < Duration::from_millis(20) {
                for _ in 0..1000 {
                    x = std::hint::black_box(x * 1.000001 + 0.5);
                }
            }
            let layer = ownership::load(&dir.0.join("cells.wkb"), k * k, false).unwrap();
            let labels = ownership::burn(layer, grid.clone()).unwrap();
            assert!(labels == expected, "burn differs on task {task}");
        });
    });
}

fn burn_child(threads: usize, terrio_threads: usize) -> Trace {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "concurrent_burn_workload",
            "--exact",
            "--ignored",
            "--test-threads",
            "1",
        ])
        .env(CHILD, threads.to_string())
        .env("RAYON_NUM_THREADS", terrio_threads.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let t = trace(&mut child);
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    eprintln!("concurrent burns, threads={threads}, terrio pool={terrio_threads}: {t:?}");
    t
}

/// Request workers that call terrio while the others compute stay within the
/// request's threads when terrio's pool has one thread (the executable's setting).
/// Two control runs give terrio's pool more threads than the request has: one
/// worker with a pool of two (asserted on hosts with 2 or more CPUs, so on every
/// CI runner) and three workers with a pool of three (asserted with 6 or more).
#[test]
fn terrio_calls_from_busy_request_workers_stay_within_budget() {
    let _serial = serial();
    for threads in [1, 3] {
        let t = burn_child(threads, 1);
        if t.samples == 0 {
            return;
        }
        assert_budget(&format!("concurrent burns t{threads}"), &t, threads);
    }
    // Controls: terrio's pool deliberately larger than the request (the previous
    // behaviour, here even with helper threads). The sampler must see the excess,
    // which shows the checks above can fail. Excess shows as CPU only when the
    // host gives this process more CPUs than the budget.
    let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
    eprintln!("CPUs available to this process: {cpus}");
    // One worker, terrio's pool of two: fits a 2-CPU runner.
    let small = burn_child(1, 2);
    if small.samples == 0 {
        return;
    }
    assert!(small.terrio_threads == 2, "{small:?}");
    if cpus >= 2 && !cfg!(debug_assertions) {
        assert!(
            small.max_busy > 1.0 + TOLERANCE,
            "the sampler must see terrio's second thread busy: {small:?}"
        );
    }
    // Three workers, terrio's pool of three: up to six busy threads.
    let large = burn_child(3, 3);
    assert!(large.terrio_threads == 3, "{large:?}");
    if cpus >= 6 && !cfg!(debug_assertions) {
        assert!(
            large.max_busy > 3.0 + TOLERANCE,
            "the sampler must see terrio's extra threads busy: {large:?}"
        );
    } else {
        eprintln!("three-thread control not asserted: {cpus} CPUs available");
    }
}
