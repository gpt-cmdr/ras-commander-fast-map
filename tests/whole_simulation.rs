//! Whole-simulation request contract: validation before reads and capability.
use ras_commander_fast_map::whole_simulation::{self, Request};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};
static NEXT: AtomicUsize = AtomicUsize::new(0);
fn scratch() -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "fast-map-whole-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&p).unwrap();
    p
}
fn request() -> Value {
    json!({"schema":"ras-commander-whole-simulation-request/1",
        "horizontal":{"schema":"ras2fim-horizontal-request/1","terrain":"missing.tif","epsg":26915,
            "crs_identity":{"kind":"epsg"},"terrain_transform":[0,1,0,256,0,-1],"window":[0,0,256,256],
            "geometry_file":"missing.wkb","cell_count":1,
            "profiles":[{"id":"t0","values_file":"v0.f32"},{"id":"t1","values_file":"v1.f32"}],
            "min_depth":0.0,"threads":1,"max_working_bytes":268435456},
        "times_hours":[0.0,0.25],"threshold":0.5,"cell_minimum_file":"min.f32"})
}
fn rejected(v: Value) -> String {
    let folder = scratch();
    let r: Request = serde_json::from_value(v).unwrap();
    let err = whole_simulation::run(&r, &folder, &folder.join("out")).unwrap_err();
    assert!(!folder.join("out").exists(), "{err}");
    fs::remove_dir_all(folder).unwrap();
    err.to_string()
}

#[test]
fn invalid_requests_are_rejected_before_reads() {
    let cases: Vec<(&str, Value)> = vec![
        ("times_hours", json!([0.0])),
        ("times_hours", json!([0.25, 0.0])),
        ("times_hours", json!([0.0, 0.0])),
        ("threshold", json!(-0.5)),
        ("products", json!(["arrival_time", "arrival_time"])),
        ("products", json!(["recession"])),
        ("products", json!([])),
        ("schema", json!("ras-commander-whole-simulation-request/2")),
    ];
    for (field, value) in cases {
        let mut v = request();
        v[field] = value;
        assert!(rejected(v).starts_with("invalid input"), "{field}");
    }
    for (field, value) in [("stream_outputs", json!(true)), ("min_depth", json!(0.1))] {
        let mut v = request();
        v["horizontal"][field] = value;
        assert!(rejected(v).contains("must not stream"), "{field}");
    }
    let mut v = request();
    v["horizontal"]["max_working_bytes"] = 1000.into();
    assert!(rejected(v).starts_with("budget exceeded"));
}

#[test]
fn duration_is_qualified_and_needs_no_opt_in() {
    // Duration uses the native recession rule; it is no longer refused, so
    // this request reaches its (missing) inputs.
    let mut v = request();
    v["products"] = json!(["arrival_time", "duration"]);
    let err = rejected(v);
    assert!(!err.starts_with("unsupported"), "{err}");
    assert!(!err.starts_with("invalid input"), "{err}");
}

#[test]
fn duration_with_storage_areas_is_refused_before_reads() {
    // Level-pool pixels have no cell position for the recession search.
    let mut v = request();
    v["products"] = json!(["arrival_time", "duration"]);
    v["horizontal"]["storage_areas_file"] = "areas.wkb".into();
    v["horizontal"]["storage_area_count"] = 1.into();
    let err = rejected(v);
    assert!(err.contains("duration with storage-area surfaces"), "{err}");
}

#[test]
fn capability_is_advertised() {
    let record = whole_simulation::method_record();
    assert_eq!(record["method_id"], "native-whole-simulation-horizontal/1");
    assert_eq!(
        record["unqualified_refused"],
        json!(["irregular_intervals"])
    );
    assert_eq!(
        record["duration_rule"],
        "rasmapper-backward-window-recession/1"
    );
}
