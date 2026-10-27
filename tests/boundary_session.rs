//! Boundary admission/lifecycle and optional real-model determinism proof.
use ras_commander_fast_map::boundary_session::{self, Request};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};
static NEXT: AtomicUsize = AtomicUsize::new(0);
fn scratch() -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "fast-map-boundary-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&p).unwrap();
    p
}
fn request() -> Value {
    json!({"schema":"ras-commander-boundary-request/1","interpolation":"sloping","model":{"schema":"ras-commander-sloping-request/1","terrain":"missing.tif","epsg":26915,"crs_identity":{"kind":"epsg"},"terrain_transform":[0,1,0,256,0,-1],"window":[0,0,256,256],"geometry_file":"missing.wkb","mesh_files":["missing.mesh"],"owner_cells":[[0,0]],"profiles":[{"id":"max","values_files":["missing.f32"]}],"threads":1,"max_working_bytes":268435456},"terrain_metadata":{"shape":[256,256],"transform":[0,1,0,256,0,-1],"tile_shape":[256,256],"tile_grid":[1,1],"selected_tiles":[0],"mask":{"path":"mask","dtype":"|u1","shape":[1,65536]},"perimeter":{"path":"perimeter","dtype":"<f4","shape":[1,513]}}})
}
#[test]
fn unsupported_options_abort_with_explicit_report_before_reads() {
    let folder = scratch();
    let mut v = request();
    v["model"]["options"] = json!({"render_mode":"slopingPretty"});
    let r: Request = serde_json::from_value(v).unwrap();
    assert!(boundary_session::run(&r, &folder, &folder.join("out")).is_err());
    let report: Value =
        serde_json::from_slice(&fs::read(folder.join("out/report.json")).unwrap()).unwrap();
    assert_eq!(report["status"], "aborted");
    assert!(report["error"].as_str().unwrap().contains("options"));
    fs::remove_dir_all(folder).unwrap();
}
#[test]
fn rejects_duplicate_tiles_and_path_profile_ids_before_reads() {
    for (field, value) in [("tiles", json!([0, 0])), ("id", json!("../escape"))] {
        let folder = scratch();
        let mut v = request();
        if field == "tiles" {
            v["terrain_metadata"]["selected_tiles"] = value;
        } else {
            v["model"]["profiles"][0]["id"] = value;
        }
        let r: Request = serde_json::from_value(v).unwrap();
        let err = boundary_session::run(&r, &folder, &folder.join("out"))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains(if field == "tiles" {
                "selected tiles"
            } else {
                "profile"
            }),
            "{err}"
        );
        fs::remove_dir_all(folder).unwrap();
    }
}
#[test]
fn rejects_cross_platform_profile_collisions_and_reserved_devices_before_input_reads() {
    for ids in [
        vec!["A", "a"],
        vec!["MAX", "max"],
        vec!["con.ext"],
        vec!["aux"],
        vec!["PRN"],
        vec!["nul.txt"],
        vec!["COM1"],
        vec!["com9.bin"],
        vec!["LPT1"],
        vec!["lpt9.ext"],
        vec!["."],
        vec![".."],
    ] {
        let folder = scratch();
        let mut value = request();
        value["model"]["profiles"] = ids
            .iter()
            .map(|id| json!({"id":id,"values_files":["missing.f32"]}))
            .collect::<Vec<_>>()
            .into();
        let request: Request = serde_json::from_value(value).unwrap();
        let error = boundary_session::run(&request, &folder, &folder.join("out"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("boundary profile"), "{ids:?}: {error}");
        let report: Value =
            serde_json::from_slice(&fs::read(folder.join("out/report.json")).unwrap()).unwrap();
        assert_eq!(report["status"], "aborted");
        assert!(
            fs::read_dir(folder.join("out"))
                .unwrap()
                .all(|entry| entry.unwrap().file_name() == "report.json")
        );
        fs::remove_dir_all(folder).unwrap();
    }
}
#[test]
fn accepts_normal_dotted_and_max_profile_ids_without_changing_tile_validation() {
    let folder = scratch();
    let mut value = request();
    value["model"]["profiles"] = json!([{"id":"max","values_files":["missing.f32"]},{"id":"reach.01_A-2","values_files":["missing.f32"]}]);
    value["terrain_metadata"]["selected_tiles"] = json!([0, 0]);
    let request: Request = serde_json::from_value(value).unwrap();
    let error = boundary_session::run(&request, &folder, &folder.join("out"))
        .unwrap_err()
        .to_string();
    assert!(error.contains("selected tiles"), "{error}");
    fs::remove_dir_all(folder).unwrap();
}
#[test]
fn existing_output_is_preserved() {
    let folder = scratch();
    fs::write(folder.join("keep.geojson"), "original").unwrap();
    let r: Request = serde_json::from_value(request()).unwrap();
    assert!(boundary_session::run(&r, &folder, &folder).is_err());
    assert_eq!(
        fs::read_to_string(folder.join("keep.geojson")).unwrap(),
        "original"
    );
    assert!(!folder.join("report.json").exists());
    fs::remove_dir_all(folder).unwrap();
}
#[test]
#[ignore = "requires RAS_FAST_MAP_BOUNDARY_REQUEST pointing to a real staged model request"]
fn real_model_deterministic_across_worker_and_profile_concurrency() {
    let path = PathBuf::from(
        std::env::var_os("RAS_FAST_MAP_BOUNDARY_REQUEST").expect("real model request required"),
    );
    let v: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let folder = scratch();
    let base = path.parent().unwrap();
    let mut baseline = None;
    for (workers, inflight) in [(1, 1), (4, 1), (4, 4)] {
        let mut req = v.clone();
        req["model"]["threads"] = workers.into();
        req["model"]["max_inflight_profiles"] = inflight.into();
        let r: Request = serde_json::from_value(req).unwrap();
        let out = folder.join(format!("run-{workers}-{inflight}"));
        let report = boundary_session::run(&r, base, &out).unwrap();
        assert_eq!(report["status"], "complete");
        let products: Vec<_> = report["profiles"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| fs::read(out.join(row["file"].as_str().unwrap())).unwrap())
            .collect();
        if let Some(ref expected) = baseline {
            assert_eq!(&products, expected);
        } else {
            baseline = Some(products);
        }
    }
    fs::remove_dir_all(folder).unwrap();
}
