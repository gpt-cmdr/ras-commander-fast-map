//! The worker records refusals in `result.json` as `_worker` does
//! (`status: aborted`, `error: "<Python class>: <message>"`), never a panic.
//! Each case reaches the guard it names: the engine is a real file (this test
//! binary) and the config is complete except for the fault under test.

use serde_json::{Value, json};
use std::sync::Mutex;

/// `worker::run` pins CPUs and sets `OMP_NUM_THREADS`; run one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

fn run(config: Value) -> Value {
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!(
        "fmp-worker-{}-{}",
        std::process::id(),
        config["tag"].as_str().unwrap()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut config = config;
    config["control"] = json!(dir.to_string_lossy());
    config["output"] = json!(dir.join("out").to_string_lossy());
    // A CPU from this process's own affinity (a Slurm allocation may exclude CPU 0).
    config["cpus"] = json!([fast_map_prepare::worker::current_cpus()[0]]);
    let path = dir.join("config.json");
    std::fs::write(&path, serde_json::to_string(&config).unwrap()).unwrap();
    let outcome = fast_map_prepare::worker::run(&path);
    assert!(outcome.is_err());
    let result: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("result.json")).unwrap()).unwrap();
    std::fs::remove_dir_all(&dir).ok();
    result
}

fn engine_record() -> Value {
    let exe = std::env::current_exe().unwrap();
    json!({"engine_path": exe.to_string_lossy(), "method": {"sloping": {
        "method_id": "rasmapper-plain-sloping/1", "stream_outputs": true,
        "multi_window_cache": true, "persistent_session": true}}})
}

fn controls() -> Value {
    json!({"max_working_mib": 1024, "max_prepared_mib": 3072, "max_profiles_per_batch": 4096,
        "max_inflight_profiles": 1, "window_cache_mib": null, "depth_cutoff_ft": 0.5,
        "compression": "NONE", "max_accumulator_mib": 256, "max_scratch_bytes": 85899345920u64,
        "stage_cell_subsets": true})
}

/// A complete job record as `_normalise` writes it. The HDF and terrain do not
/// exist: every guard under test comes before they are opened.
fn job(timesteps: Value) -> Value {
    let wkt = r#"PROJCRS["NAD83 / Texas South Central (ftUS)",BASEGEOGCRS["NAD83",DATUM["North American Datum 1983",ELLIPSOID["GRS 1980",6378137,298.257222101,LENGTHUNIT["metre",1]]],PRIMEM["Greenwich",0,ANGLEUNIT["degree",0.0174532925199433]],ID["EPSG",4269]],CONVERSION["unnamed",METHOD["Lambert Conic Conformal (2SP)",ID["EPSG",9802]]],CS[Cartesian,2],AXIS["easting (X)",east,ORDER[1],LENGTHUNIT["US survey foot",0.304800609601219]],AXIS["northing (Y)",north,ORDER[2],LENGTHUNIT["US survey foot",0.304800609601219]],ID["EPSG",2278]]"#;
    // POLYGON ((0 0, 1 0, 1 1, 0 0)), little-endian WKB, as hex.
    let mut b = vec![1u8];
    for n in [3u32, 1, 4] {
        b.extend(n.to_le_bytes());
    }
    for v in [0.0f64, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 0.0] {
        b.extend(v.to_le_bytes());
    }
    let wkb: String = b.iter().map(|x| format!("{x:02x}")).collect();
    json!({"key": "k", "job_id": "reach-p02", "reach_id": "reach",
        "hdf_path": "missing.p02.hdf", "terrain_path": "missing.tif",
        "options": {"render_mode": "sloping", "reduce_shallow_to_horizontal": false,
            "use_depth_weighted_faces": false, "postprocessing_hdf": null,
            "invalid_cells": "repair"},
        "interpolation": "sloping", "sources": {},
        "aoi": {"crs": wkt, "wkb": wkb, "source": "test"},
        "timesteps": timesteps, "groups": []})
}

#[test]
fn engine_without_multi_window_cache_is_refused_like_prepare_sloping_model() {
    let mut engine = engine_record();
    engine["method"]["sloping"]["multi_window_cache"] = json!(false);
    let result = run(
        json!({"tag": "cache", "engine": engine, "controls": controls(),
        "jobs": [job(json!([0]))]}),
    );
    assert_eq!(result["status"], "aborted");
    assert_eq!(
        result["error"],
        "FimEngineError: Configurable windows require multi_window_cache capability"
    );
}

#[test]
fn zero_profiles_per_batch_is_refused_before_any_staging() {
    let mut c = controls();
    c["max_profiles_per_batch"] = json!(0);
    let result = run(
        json!({"tag": "batch", "engine": engine_record(), "controls": c,
        "jobs": [job(json!([0]))]}),
    );
    assert_eq!(result["status"], "aborted");
    assert_eq!(
        result["error"],
        "ValueError: max_profiles_per_batch must be a positive integer"
    );
}

#[test]
fn a_job_without_timesteps_is_refused() {
    let result = run(
        json!({"tag": "empty", "engine": engine_record(), "controls": controls(),
        "jobs": [job(json!([]))]}),
    );
    assert_eq!(result["status"], "aborted");
    assert_eq!(
        result["error"],
        "ValueError: Group timesteps must be unique nonnegative saved indices"
    );
}

#[test]
fn a_missing_plan_hdf_is_a_file_error_not_a_panic() {
    let result = run(
        json!({"tag": "hdf", "engine": engine_record(), "controls": controls(),
        "jobs": [job(json!([0]))]}),
    );
    assert_eq!(result["status"], "aborted");
    let error = result["error"].as_str().unwrap();
    assert!(!error.contains("panic"), "{error}");
}

#[test]
fn an_unknown_control_is_refused_not_dropped() {
    // A control the native worker does not implement must never be ignored:
    // that is how `corner_fit_bound` was once dropped (native corner fits under
    // a bounded request).
    let mut c = controls();
    c["future_control"] = json!(true);
    let result = run(
        json!({"tag": "unknown", "engine": engine_record(), "controls": c,
        "jobs": [job(json!([0]))]}),
    );
    assert_eq!(result["status"], "aborted");
    assert_eq!(
        result["error"],
        "FimUnsupportedError: the native worker does not implement control \"future_control\""
    );
}

#[test]
fn a_bounded_request_needs_an_engine_with_the_bounded_method() {
    let mut c = controls();
    c["corner_fit_bound"] = json!({"mode": "input_range", "tolerance": 0.0});
    let result = run(
        json!({"tag": "bounded-engine", "engine": engine_record(), "controls": c,
        "jobs": [job(json!([0]))]}),
    );
    assert_eq!(result["status"], "aborted");
    assert_eq!(
        result["error"],
        "FimUnsupportedError: Engine lacks the opt-in sloping method \
         rasmapper-plain-sloping-bounded-corners/1"
    );
}

#[test]
fn a_malformed_bound_is_refused_as_python_refuses_it() {
    let mut c = controls();
    c["corner_fit_bound"] = json!({"mode": "input_range", "tolerance": -1.0});
    let result = run(
        json!({"tag": "bounded-malformed", "engine": engine_record(), "controls": c,
        "jobs": [job(json!([0]))]}),
    );
    assert_eq!(result["status"], "aborted");
    assert_eq!(
        result["error"],
        "ValueError: corner_fit_bound tolerance must be finite and nonnegative (WSE units)"
    );
}

#[test]
fn a_null_bound_is_native() {
    // `"corner_fit_bound": null` is the native method: the refusal reached is the
    // missing plan HDF, after every control and engine gate.
    let mut c = controls();
    c["corner_fit_bound"] = Value::Null;
    let result = run(
        json!({"tag": "bounded-null", "engine": engine_record(), "controls": c,
        "jobs": [job(json!([0]))]}),
    );
    assert_eq!(result["status"], "aborted");
    let error = result["error"].as_str().unwrap();
    assert!(
        !error.contains("corner_fit_bound") && !error.contains("bounded"),
        "{error}"
    );
}
