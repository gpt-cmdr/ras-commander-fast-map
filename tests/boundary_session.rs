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
fn legacy_boundary_refuses_mosaic_extension_before_input_reads() {
    let folder = scratch();
    let mut v = request();
    v["additional_terrains"] = json!([{
        "terrain":"missing-extra.tif", "crs_identity":{"kind":"epsg"},
        "terrain_metadata":v["terrain_metadata"].clone()
    }]);
    let r: Request = serde_json::from_value(v).unwrap();
    let error = boundary_session::run(&r, &folder, &folder.join("out"))
        .unwrap_err()
        .to_string();
    assert!(error.contains("schema/interpolation/options"), "{error}");
    fs::remove_dir_all(folder).unwrap();
}

#[test]
fn native_boundary_refuses_partial_source_history_before_input_reads() {
    let folder = scratch();
    let mut v = request();
    v["schema"] = "ras-commander-boundary-request/2".into();
    v["contour_method"] = "rasmapper-serial-zero-depth-contours/1".into();
    v["terrain_metadata"]["shape"] = json!([512, 256]);
    v["terrain_metadata"]["tile_grid"] = json!([2, 1]);
    let r: Request = serde_json::from_value(v).unwrap();
    let error = boundary_session::run(&r, &folder, &folder.join("out"))
        .unwrap_err()
        .to_string();
    assert!(error.contains("every original source tile"), "{error}");
    fs::remove_dir_all(folder).unwrap();
}

fn storage_source_fixture(folder: &std::path::Path, mask_value: u8, outside: bool) -> Value {
    use ras_commander_fast_map::effective_terrain::{self, Effective, Grid};
    effective_terrain::write_geotiff(
        &folder.join("terrain.tif"),
        Effective {
            grid: Grid {
                origin_x: 0.,
                origin_y: 4.,
                cell_size: 1.,
                width: 4,
                height: 4,
            },
            values: vec![0.; 16],
            report: Value::Null,
        },
        2227,
    )
    .unwrap();
    fs::write(folder.join("mask"), vec![mask_value; 65536]).unwrap();
    fs::write(
        folder.join("perimeter"),
        (-9999f32).to_le_bytes().repeat(513),
    )
    .unwrap();
    fs::write(folder.join("levels"), 1f32.to_le_bytes()).unwrap();
    let offset = if outside { 1000.0f64 } else { 0.0 };
    let mut polygon = vec![1u8];
    for n in [3u32, 1, 5] {
        polygon.extend(n.to_le_bytes());
    }
    for [x, y] in [[0.0f64, 0.0f64], [0., 4.], [4., 4.], [4., 0.], [0., 0.]] {
        polygon.extend((x + offset).to_le_bytes());
        polygon.extend((y + offset).to_le_bytes());
    }
    let mut geometry = (polygon.len() as u32).to_le_bytes().to_vec();
    geometry.extend(polygon);
    fs::write(folder.join("storage.wkb"), geometry).unwrap();
    json!({"schema":"ras-commander-boundary-request/2", "interpolation":"sloping",
        "contour_method":"rasmapper-serial-zero-depth-contours/1", "diagnostic_units":true,
        "model":{"schema":"ras-commander-sloping-request/1", "terrain":"terrain.tif", "epsg":2227,
            "crs_identity":{"kind":"epsg"}, "terrain_transform":[0,1,0,4,0,-1], "window":[0,0,4,4],
            "geometry_file":"unused", "mesh_files":[], "owner_cells":[], "storage_areas_file":"storage.wkb",
            "storage_area_count":1, "profiles":[{"id":"a", "values_files":[], "storage_area_values_file":"levels"},
                {"id":"b", "values_files":[], "storage_area_values_file":"levels"}],
            "threads":2, "max_inflight_profiles":2, "max_working_bytes":134217728},
        "terrain_metadata":{"shape":[4,4], "transform":[0,1,0,4,0,-1], "tile_shape":[256,256],
            "tile_grid":[1,1], "selected_tiles":[0], "mask":{"path":"mask", "dtype":"|u1", "shape":[1,65536]},
            "perimeter":{"path":"perimeter", "dtype":"<f4", "shape":[1,513]}}})
}

#[test]
fn later_profile_failure_removes_completed_diagnostic_units() {
    let folder = scratch();
    let mut value = storage_source_fixture(&folder, 1, false);
    value["model"]["threads"] = 1.into();
    value["model"]["max_inflight_profiles"] = 1.into();
    value["model"]["profiles"][1]["storage_area_values_file"] = "missing-levels".into();
    let request: Request = serde_json::from_value(value).unwrap();
    let out = folder.join("out");
    assert!(boundary_session::run(&request, &folder, &out).is_err());
    let report: Value =
        serde_json::from_slice(&fs::read(out.join("report.json")).unwrap()).unwrap();
    assert_eq!(report["status"], "aborted");
    assert!(
        fs::read_dir(&out)
            .unwrap()
            .all(|entry| entry.unwrap().file_name() == "report.json")
    );
    assert!(folder.join("levels").is_file());
    fs::remove_dir_all(folder).unwrap();
}

#[test]
fn native_row_metadata_preserves_parallel_storage_products_and_source_order() {
    let folder = scratch();
    let v = storage_source_fixture(&folder, 1, false);
    let mut expected = None;
    for threads in [1, 2] {
        let mut v = v.clone();
        v["model"]["threads"] = threads.into();
        let req: Request = serde_json::from_value(v).unwrap();
        let out = folder.join(format!("t{threads}"));
        let report = boundary_session::run(&req, &folder, &out).unwrap();
        assert_eq!(report["terrain_metadata_residency"], "active-rows-only");
        assert_eq!(
            report["terrain_sources"][0]["terrain_metadata"]["mask"]["rows_read"],
            2
        );
        assert_eq!(
            report["terrain_sources"][0]["terrain_metadata"]["perimeter"]["rows_read"],
            2
        );
        assert_eq!(report["profiles"][0]["area"], 9.0);
        let products = ["a.geojson", "b.geojson"].map(|path| fs::read(out.join(path)).unwrap());
        if let Some(expected) = &expected {
            assert_eq!(&products, expected);
        } else {
            expected = Some(products);
        }
    }
    fs::remove_dir_all(folder).unwrap();
}

#[test]
fn native_zero_masks_and_no_hydraulic_candidates_keep_empty_ordered_units() {
    for (mask, outside, row_reads) in [(0, false, 2), (1, true, 0)] {
        let folder = scratch();
        let req: Request =
            serde_json::from_value(storage_source_fixture(&folder, mask, outside)).unwrap();
        let out = folder.join("out");
        let report = boundary_session::run(&req, &folder, &out).unwrap();
        assert_eq!(
            report["terrain_sources"][0]["terrain_metadata"]["mask"]["rows_read"],
            row_reads
        );
        assert_eq!(report["profiles"][0]["status"], "empty");
        let dump: Value =
            serde_json::from_slice(&fs::read(out.join("a.units.json")).unwrap()).unwrap();
        assert_eq!(dump["units"], json!([{"sequence":0, "rings":[]} ]));
        fs::remove_dir_all(folder).unwrap();
    }
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
