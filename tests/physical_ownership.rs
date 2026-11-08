//! Physical ownership is the actual polygon burn, including dry/nodata pixels.
use ras_commander_fast_map::{
    engine_method_record,
    sloping_session::{self, PreparedSession, Request},
};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn table(dir: &Path, name: &str, rows: usize, bytes: Vec<u8>) -> Value {
    fs::write(dir.join(format!("{name}.bin")), bytes).unwrap();
    json!({"path":format!("{name}.bin"),"rows":rows})
}
fn ints(dir: &Path, name: &str, rows: usize, v: &[i32]) -> Value {
    table(
        dir,
        name,
        rows,
        v.iter().flat_map(|v| v.to_le_bytes()).collect(),
    )
}
fn doubles(dir: &Path, name: &str, rows: usize, v: &[f64]) -> Value {
    table(
        dir,
        name,
        rows,
        v.iter().flat_map(|v| v.to_le_bytes()).collect(),
    )
}
fn floats(dir: &Path, name: &str, rows: usize, v: &[f32]) -> Value {
    table(
        dir,
        name,
        rows,
        v.iter().flat_map(|v| v.to_le_bytes()).collect(),
    )
}
fn polygon(dir: &Path, name: &str, x0: f64, x1: f64) {
    let mut wkb = vec![1];
    for i in [3u32, 1, 5] {
        wkb.extend(i.to_le_bytes());
    }
    for point in [[x0, 0.0], [x0, 4.0], [x1, 4.0], [x1, 0.0], [x0, 0.0]] {
        for v in point {
            wkb.extend(v.to_le_bytes());
        }
    }
    let mut bytes = (wkb.len() as u32).to_le_bytes().to_vec();
    bytes.extend(wkb);
    fs::write(dir.join(name), bytes).unwrap();
}
fn fixture() -> (Fixture, Value) {
    let dir = std::env::temp_dir().join(format!(
        "fast-map-physical-owners-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&dir).unwrap();
    use ras_commander_fast_map::effective_terrain::{self, Effective, Grid};
    effective_terrain::write_geotiff(
        &dir.join("terrain.tif"),
        Effective {
            grid: Grid {
                origin_x: 0.0,
                origin_y: 4.0,
                cell_size: 1.0,
                width: 4,
                height: 4,
            },
            values: (0..16)
                .map(|i| if i == 0 { f32::NAN } else { 0.0 })
                .collect(),
            report: Value::Null,
        },
        3857,
    )
    .unwrap();
    let arrays = json!({
        "centers":doubles(&dir,"centers",5,&[2.,2.,-1.,2.,2.,5.,5.,2.,2.,-1.]),
        "cell_info":ints(&dir,"cell_info",5,&[0,4,4,1,5,1,6,1,7,1]),
        "cell_faces":ints(&dir,"cell_faces",8,&[0,1,1,1,2,1,3,1,0,-1,1,-1,2,-1,3,-1]),
        "cell_minimum":floats(&dir,"cell_minimum",5,&[0.;5]),
        "face_vertices":ints(&dir,"face_vertices",4,&[0,1,1,2,2,3,3,0]),
        "face_cells":ints(&dir,"face_cells",4,&[0,1,0,2,0,3,0,4]),
        "face_info":ints(&dir,"face_info",4,&[0;8]),
        "face_points":doubles(&dir,"face_points",0,&[]),
        "face_minimum":floats(&dir,"face_minimum",4,&[0.;4]),
        "vertices":doubles(&dir,"vertices",4,&[0.,0.,0.,4.,4.,4.,4.,0.]),
        "vertex_info":ints(&dir,"vertex_info",4,&[0,2,2,2,4,2,6,2]),
        "vertex_faces":ints(&dir,"vertex_faces",8,&[3,1,0,1,0,1,1,1,1,1,2,1,2,1,3,1])
    });
    fs::write(
        dir.join("mesh.json"),
        json!({"schema":"ras-commander-sloping-mesh/1","arrays":arrays}).to_string(),
    )
    .unwrap();
    polygon(&dir, "cell.wkb", 0., 3.);
    polygon(&dir, "left.wkb", 0., 2.);
    polygon(&dir, "right.wkb", 2., 4.);
    for (name, value) in [("a", 0.5), ("b", 1.25), ("dry", 0.0), ("high", 10001.0)] {
        floats(&dir, name, 5, &[value, 0., 0., 0., 0.]);
    }
    let model = json!({"schema":sloping_session::SCHEMA,"terrain":"terrain.tif","epsg":3857,
        "crs_identity":{"kind":"epsg"},"terrain_transform":[0.,1.,0.,4.,0.,-1.],"window":[0,0,4,4],
        "geometry_file":"cell.wkb","mesh_files":["mesh.json"],"owner_cells":[[0,0]],
        "profiles":[{"id":"a","values_files":["a.bin"]},{"id":"b","values_files":["b.bin"]},
            {"id":"dry","values_files":["dry.bin"]},{"id":"high","values_files":["high.bin"]}],
        "threads":2,"max_working_bytes":256u64<<20,"window_cache_bytes":0});
    (Fixture(dir), model)
}

#[test]
fn stored_grid_source_identity_rejects_signed_zero_substitution() {
    let (f, mut wire) = fixture();
    wire["schema"] = json!("ras-commander-sloping-request/2");
    wire["terrain_transform"][2] = json!(-0.0_f64);
    wire["grid_contract"] = json!({"id":"rasmapper-stored-tile-grid/1","source_width":4,"source_height":4,"tile_size":256,
        "source_geo_transform":[0.,1.,-0.0_f64,4.,0.,-1.],"output_geo_transform":[0.,1.,0.,4.,0.,-1.]});
    let request: Request = serde_json::from_value(wire).unwrap();
    let output = f.0.join("signed-zero-source");
    let error = sloping_session::run(&request, &f.0, &output).unwrap_err();
    assert!(
        error.to_string().contains("stored grid source transform"),
        "{error}"
    );
    assert!(!output.exists());
}

#[test]
fn stored_grid_owner_export_cache_and_report_are_separate_from_legacy_grid() {
    let (f, mut wire) = fixture();
    wire["schema"] = json!("ras-commander-sloping-request/2");
    wire["grid_contract"] = json!({"id":"rasmapper-stored-tile-grid/1","source_width":4,"source_height":4,"tile_size":256,
        "source_geo_transform":[0.,1.,0.,4.,0.,-1.],"output_geo_transform":[0.,1.,0.,4.,0.,-1.]});
    wire["export_physical_ownership"] = json!(true);
    wire["window_cache_bytes"] = json!(1u64 << 20);
    let req: Request = serde_json::from_value(wire.clone()).unwrap();
    let mut session = PreparedSession::default();
    for folder in ["grid-first", "grid-cached"] {
        let report =
            sloping_session::run_with_session(&req, &f.0, &f.0.join(folder), Some(&mut session))
                .unwrap();
        assert_eq!(report["schema"], "ras-commander-sloping-report/2");
        assert_eq!(report["method"]["method_id"], "rasmapper-plain-sloping/2");
        assert_eq!(report["grid_contract"], wire["grid_contract"]);
        assert_eq!(report["source_geo_transform"], wire["terrain_transform"]);
        assert_eq!(report["session"]["window_reused"], folder == "grid-cached");
        let owners = fs::read(f.0.join(folder).join("owners.bin")).unwrap();
        assert!(
            owners
                .as_chunks::<4>()
                .0
                .iter()
                .all(|b| u32::from_le_bytes(*b) == 1)
        );
    }
    let mut legacy = wire.clone();
    legacy["schema"] = json!(sloping_session::SCHEMA);
    let req = serde_json::from_value(legacy).unwrap();
    assert!(sloping_session::run(&req, &f.0, &f.0.join("bad-grid")).is_err());
    assert!(!f.0.join("bad-grid").exists());
    let mut bad = wire;
    bad["grid_contract"]["output_geo_transform"][1] = json!(2.);
    assert!(
        sloping_session::run(
            &serde_json::from_value(bad).unwrap(),
            &f.0,
            &f.0.join("forged-grid")
        )
        .is_err()
    );
    assert!(!f.0.join("forged-grid").exists());
}

#[test]
fn physical_export_matches_burn_for_dry_nodata_and_cached_windows_without_changing_products() {
    let (f, mut model) = fixture();
    model["window_cache_bytes"] = json!(1u64 << 20);
    let baseline: Request = serde_json::from_value(model.clone()).unwrap();
    assert!(!baseline.export_physical_ownership);
    let control = sloping_session::run(&baseline, &f.0, &f.0.join("control")).unwrap();
    assert_eq!(control["status"], "complete");
    assert!(control.get("physical_ownership").is_none());
    assert!(!f.0.join("control/owners.bin").exists());
    model["export_physical_ownership"] = json!(true);
    let request: Request = serde_json::from_value(model).unwrap();
    let mut session = PreparedSession::default();
    // A preceding delivery mask in the same retained session must not poison
    // the unmasked physical export; the AOI bytes are part of the window key.
    let mut masked = baseline.clone();
    masked.aoi_file = Some("left.wkb".into());
    let preceding =
        sloping_session::run_with_session(&masked, &f.0, &f.0.join("masked"), Some(&mut session))
            .unwrap();
    assert_eq!(preceding["status"], "complete");
    assert!(
        preceding["session"]["retained_window_count"]
            .as_u64()
            .unwrap()
            > 0
    );
    for (name, reused) in [("first", false), ("cached", true)] {
        let out = f.0.join(name);
        let report =
            sloping_session::run_with_session(&request, &f.0, &out, Some(&mut session)).unwrap();
        assert_eq!(report["status"], "complete", "{report}");
        assert_eq!(report["session"]["window_reused"], reused);
        let ownership = &report["physical_ownership"];
        assert_eq!(ownership["kind"], "physical_2d");
        assert_eq!(ownership["dtype"], "<u4");
        assert_eq!(ownership["count"], 16);
        assert_eq!(ownership["bytes"], 64);
        assert_eq!(ownership["owned_pixels"], 12);
        assert_eq!(ownership["owner_cell_count"], 1);
        assert_eq!(ownership["window"], json!([0, 0, 4, 4]));
        assert_eq!(ownership["geo_transform"], json!([0., 1., 0., 4., 0., -1.]));
        assert_eq!(ownership["epsg"], 3857);
        assert_eq!(ownership["crs_identity"], json!({"kind":"epsg"}));
        let bytes = fs::read(out.join(ownership["path"].as_str().unwrap())).unwrap();
        let labels: Vec<u32> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| u32::from_le_bytes(*b))
            .collect();
        assert_eq!(labels, vec![1, 1, 1, 0, 1, 1, 1, 0, 1, 1, 1, 0, 1, 1, 1, 0]);
        for profile in ["a", "b", "dry", "high"] {
            for product in ["depth.bin", "wse.bin"] {
                assert_eq!(
                    fs::read(out.join(profile).join(product)).unwrap(),
                    fs::read(f.0.join("control").join(profile).join(product)).unwrap()
                );
            }
        }
        let dry = fs::read(out.join("dry/depth.bin")).unwrap();
        assert!(
            dry.as_chunks::<4>()
                .0
                .iter()
                .all(|b| f32::from_le_bytes(*b).is_nan())
        );
        let wet = fs::read(out.join("a/depth.bin")).unwrap();
        assert!(f32::from_le_bytes(wet[..4].try_into().unwrap()).is_nan());
        assert_eq!(labels[0], 1, "terrain-nodata remains physically owned");
    }
    // A component failure must remove even a successfully written owner file.
    fs::write(f.0.join("a.bin"), []).unwrap();
    let out = f.0.join("failed");
    let report =
        sloping_session::run_with_session(&request, &f.0, &out, Some(&mut session)).unwrap();
    assert_eq!(report["status"], "aborted");
    assert!(!out.join("owners.bin").exists());
    assert!(!out.join("owners.bin.partial").exists());
    assert!(report.get("physical_ownership").is_none());
}

#[test]
fn export_refuses_filtered_or_storage_inputs_before_missing_payload_reads() {
    let (f, base) = fixture();
    let mut missing = base;
    missing["export_physical_ownership"] = json!(true);
    missing["terrain"] = json!("absent.tif");
    missing["geometry_file"] = json!("absent.wkb");
    for case in 0..5 {
        let mut value = missing.clone();
        match case {
            0 => value["aoi_file"] = json!("absent-aoi.wkb"),
            1 => {
                value["terrain_scope"] = json!({"kind":"top_priority_source","source_count":2,
                "mask":{"path":"absent.mask","dtype":"|u1","shape":[1,65536]}})
            }
            2 => value["storage_areas_file"] = json!("absent-storage.wkb"),
            3 => value["storage_area_count"] = json!(1),
            _ => value["profiles"][0]["storage_area_values_file"] = json!("absent-values.bin"),
        }
        let request: Request = serde_json::from_value(value).unwrap();
        let out = f.0.join(format!("refused-{case}"));
        let error = sloping_session::run(&request, &f.0, &out).unwrap_err();
        assert_eq!(error.kind(), "unsupported");
        assert!(error.to_string().contains("export_physical_ownership"));
        assert!(!out.exists());
    }
}

#[test]
fn capability_is_additive_in_legacy_and_structured_discovery() {
    let registry = engine_method_record();
    assert_eq!(registry["sloping"]["export_physical_ownership"], true);
    assert_eq!(
        registry["sloping"]["method_id"],
        "rasmapper-plain-sloping/1"
    );
    let record = registry["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["key"] == "sloping")
        .unwrap();
    assert_eq!(record["flags"]["export_physical_ownership"], true);
}
