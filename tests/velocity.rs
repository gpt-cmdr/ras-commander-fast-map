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

fn with_velocity() -> Value {
    let mut v = sloping();
    v["velocity_normals_files"] = json!(["n.f32"]);
    v["profiles"][0]["face_velocity_files"] = json!(["v.f32"]);
    v
}

fn invalid(v: Value) -> String {
    let folder = scratch();
    let r: sloping_session::Request = serde_json::from_value(v).unwrap();
    let err = sloping_session::run(&r, &folder, &folder.join("out")).unwrap_err();
    assert_eq!(err.kind(), "invalid", "{err}");
    assert!(!folder.join("out").exists());
    fs::remove_dir_all(folder).unwrap();
    err.to_string()
}

#[test]
fn derived_controls_are_validated_before_reads() {
    // Derived controls without velocity.
    for (field, value) in [
        ("derived_products", json!(["depth_x_velocity"])),
        ("velocity_depth", json!("horizontal")),
        ("reduce_max", json!(true)),
    ] {
        let mut v = sloping();
        v[field] = value;
        assert!(invalid(v).contains("require velocity"), "{field}");
    }
    // Unknown or repeated products, Froude without gravity.
    for products in [json!(["shear"]), json!(["froude", "froude"])] {
        let mut v = with_velocity();
        v["derived_products"] = products;
        v["gravity"] = json!(32.174);
        assert!(invalid(v).contains("derived_products"));
    }
    let mut v = with_velocity();
    v["derived_products"] = json!(["froude"]);
    assert!(invalid(v).contains("gravity"));
    // The Max reduction needs products and file outputs.
    let mut v = with_velocity();
    v["reduce_max"] = json!(true);
    assert!(invalid(v).contains("reduce_max"));
    let mut v = with_velocity();
    v["reduce_max"] = json!(true);
    v["derived_products"] = json!(["depth_x_velocity"]);
    v["stream_outputs"] = json!(true);
    assert!(invalid(v).contains("reduce_max"));
}

#[test]
fn derived_capabilities_are_advertised() {
    let record = sloping_session::method_record();
    assert_eq!(
        record["derived_products"],
        json!(["depth_x_velocity", "depth_x_velocity_squared", "froude"])
    );
    assert_eq!(record["velocity_depth"], json!(["rendered", "horizontal"]));
    assert_eq!(record["derived_max"], "reduce_max/1");
    assert_eq!(
        record["derived_method_id"],
        "rasmapper-depth-velocity-products/1"
    );
}
