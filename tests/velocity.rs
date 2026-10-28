//! Velocity request contract: all-or-nothing fields, validated before reads,
//! boundary refusal and the advertised capability.
use ras_commander_fast_map::{boundary_session, sloping_session};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};
static NEXT: AtomicUsize = AtomicUsize::new(0);
fn scratch() -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "fast-map-velocity-{}-{}",
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

#[test]
fn velocity_fields_are_all_or_nothing_before_reads() {
    let mut normals_only = sloping();
    normals_only["velocity_normals_files"] = json!(["n.f32"]);
    let mut velocity_only = sloping();
    velocity_only["profiles"][0]["face_velocity_files"] = json!(["v.f32"]);
    let mut wrong_count = normals_only.clone();
    wrong_count["velocity_normals_files"] = json!(["n.f32", "n2.f32"]);
    wrong_count["profiles"][0]["face_velocity_files"] = json!(["v.f32"]);
    for v in [normals_only, velocity_only, wrong_count] {
        let folder = scratch();
        let r: sloping_session::Request = serde_json::from_value(v).unwrap();
        let err = sloping_session::run(&r, &folder, &folder.join("out")).unwrap_err();
        assert_eq!(err.kind(), "invalid", "{err}");
        assert!(!folder.join("out").exists());
        fs::remove_dir_all(folder).unwrap();
    }
}

#[test]
fn boundaries_refuse_velocity() {
    let folder = scratch();
    let mut model = sloping();
    model["velocity_normals_files"] = json!(["n.f32"]);
    model["profiles"][0]["face_velocity_files"] = json!(["v.f32"]);
    let v = json!({"schema":"ras-commander-boundary-request/1","interpolation":"sloping","model":model,
        "terrain_metadata":{"shape":[256,256],"transform":[0,1,0,256,0,-1],"tile_shape":[256,256],
        "tile_grid":[1,1],"selected_tiles":[0],"mask":{"path":"mask","dtype":"|u1","shape":[1,65536]},
        "perimeter":{"path":"perimeter","dtype":"<f4","shape":[1,513]}}});
    let r: boundary_session::Request = serde_json::from_value(v).unwrap();
    let err = boundary_session::run(&r, &folder, &folder.join("out"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("velocity"), "{err}");
    fs::remove_dir_all(folder).unwrap();
}

#[test]
fn capability_is_advertised() {
    assert_eq!(
        sloping_session::method_record()["velocity"],
        "rasmapper-sloping-velocity/0"
    );
}
