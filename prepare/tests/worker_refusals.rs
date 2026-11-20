//! The worker records refusals in `result.json` as `_worker` does
//! (`status: aborted`, `error: "<Python class>: <message>"`), never a panic.

use serde_json::{Value, json};

fn run(config: Value) -> Value {
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
    json!({"engine_path": "missing-engine", "method": {"sloping": {
        "method_id": "rasmapper-plain-sloping/1", "stream_outputs": true,
        "multi_window_cache": true, "persistent_session": true}}})
}

#[test]
fn engine_without_multi_window_cache_is_refused_like_prepare_sloping_model() {
    let mut engine = engine_record();
    engine["method"]["sloping"]["multi_window_cache"] = json!(false);
    let result = run(json!({"tag": "cache", "cpus": [0], "engine": engine,
        "controls": {}, "jobs": [{"key": "k", "options": {"render_mode": "sloping",
        "reduce_shallow_to_horizontal": false, "use_depth_weighted_faces": false,
        "postprocessing_hdf": null, "invalid_cells": "repair"}, "interpolation": "sloping"}]}));
    assert_eq!(result["status"], "aborted");
    assert_eq!(
        result["error"],
        "FimEngineError: Configurable windows require multi_window_cache capability"
    );
}

#[test]
fn malformed_controls_abort_with_an_error_not_a_panic() {
    let result = run(
        json!({"tag": "controls", "cpus": [0], "engine": engine_record(),
        "controls": {"max_profiles_per_batch": 0}, "jobs": [{"key": "k", "options": {"render_mode": "sloping",
        "reduce_shallow_to_horizontal": false, "use_depth_weighted_faces": false,
        "postprocessing_hdf": null, "invalid_cells": "repair"}, "interpolation": "sloping",
        "sources": {}, "aoi": {}, "timesteps": []}]}),
    );
    assert_eq!(result["status"], "aborted");
    let error = result["error"].as_str().unwrap();
    assert!(!error.contains("panic"), "{error}");
}
