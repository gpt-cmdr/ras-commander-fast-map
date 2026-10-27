//! Storage-area request contract: all-or-nothing fields, refusal before reads,
//! boundary refusal and the advertised capability.
use ras_commander_fast_map::{boundary_session, horizontal, sloping_session};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};
static NEXT: AtomicUsize = AtomicUsize::new(0);
fn scratch() -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "fast-map-storage-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&p).unwrap();
    p
}
fn sloping() -> Value {
    json!({"schema":"ras-commander-sloping-request/1","terrain":"missing.tif","epsg":26915,
        "crs_identity":{"kind":"epsg"},"terrain_transform":[0,1,0,256,0,-1],"window":[0,0,256,256],
        "geometry_file":"missing.wkb","mesh_files":["missing.mesh"],"owner_cells":[[0,0]],
        "profiles":[{"id":"max","values_files":["missing.f32"]}],"threads":1,
        "max_working_bytes":268435456})
}
fn horizontal() -> Value {
    json!({"schema":"ras2fim-horizontal-request/1","terrain":"missing.tif","epsg":26915,
        "crs_identity":{"kind":"epsg"},"terrain_transform":[0,1,0,256,0,-1],"window":[0,0,256,256],
        "geometry_file":"missing.wkb","cell_count":1,"profiles":[{"id":"max","values_file":"missing.f32"}],
        "min_depth":0.0,"threads":1,"max_working_bytes":268435456})
}
fn partial_variants(base: Value, profile_key: &str) -> Vec<Value> {
    let mut only_file = base.clone();
    only_file["storage_areas_file"] = "sa.wkb".into();
    let mut file_and_count = only_file.clone();
    file_and_count["storage_area_count"] = 1.into();
    let mut only_values = base.clone();
    only_values["profiles"][0][profile_key] = "sa.f32".into();
    let mut count_without_file = base;
    count_without_file["storage_area_count"] = 2.into();
    count_without_file["profiles"][0][profile_key] = "sa.f32".into();
    vec![only_file, file_and_count, only_values, count_without_file]
}

#[test]
fn sloping_storage_area_fields_are_all_or_nothing_before_reads() {
    for v in partial_variants(sloping(), "storage_area_values_file") {
        let folder = scratch();
        let r: sloping_session::Request = serde_json::from_value(v).unwrap();
        let err = sloping_session::run(&r, &folder, &folder.join("out")).unwrap_err();
        assert_eq!(err.kind(), "invalid", "{err}");
        assert!(err.to_string().contains("storage_area"), "{err}");
        assert!(!folder.join("out").exists());
        fs::remove_dir_all(folder).unwrap();
    }
}

#[test]
fn horizontal_storage_area_fields_are_all_or_nothing_before_reads() {
    for v in partial_variants(horizontal(), "storage_area_values_file") {
        let folder = scratch();
        let r: horizontal::Request = serde_json::from_value(v).unwrap();
        let err = horizontal::run(&r, &folder, &folder.join("out")).unwrap_err();
        assert_eq!(err.kind(), "invalid", "{err}");
        assert!(!folder.join("out").exists());
        fs::remove_dir_all(folder).unwrap();
    }
}

#[test]
fn boundaries_refuse_storage_areas() {
    let folder = scratch();
    let mut model = sloping();
    model["storage_areas_file"] = "sa.wkb".into();
    model["storage_area_count"] = 1.into();
    model["profiles"][0]["storage_area_values_file"] = "sa.f32".into();
    let v = json!({"schema":"ras-commander-boundary-request/1","interpolation":"sloping","model":model,
        "terrain_metadata":{"shape":[256,256],"transform":[0,1,0,256,0,-1],"tile_shape":[256,256],
        "tile_grid":[1,1],"selected_tiles":[0],"mask":{"path":"mask","dtype":"|u1","shape":[1,65536]},
        "perimeter":{"path":"perimeter","dtype":"<f4","shape":[1,513]}}});
    let r: boundary_session::Request = serde_json::from_value(v).unwrap();
    let err = boundary_session::run(&r, &folder, &folder.join("out"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("storage-area"), "{err}");
    fs::remove_dir_all(folder).unwrap();
}

#[test]
fn capability_is_advertised() {
    assert_eq!(
        sloping_session::method_record()["storage_area_surfaces"],
        "storage-area-level-pool/1"
    );
    assert_eq!(
        horizontal::method_record()["storage_area_surfaces"],
        "storage-area-level-pool/1"
    );
}
