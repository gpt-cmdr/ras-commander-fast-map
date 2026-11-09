//! Max shear-stress request contract: validated before any read, refused by
//! embedding requests, and the advertised capability.
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
        "fast-map-shear-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&p).unwrap();
    p
}
fn shear_mesh() -> Value {
    json!({"normals_file":"n.f32","tables_info_file":"i.bin","tables_values_file":"v.bin",
        "table_rows":4,"times":3,"water_surface_file":"ws.f32","face_velocity_file":"fv.f32"})
}
fn sloping() -> Value {
    json!({"schema":"ras-commander-sloping-request/1","terrain":"missing.tif","epsg":26915,
        "crs_identity":{"kind":"epsg"},"terrain_transform":[0,1,0,256,0,-1],"window":[0,0,256,256],
        "geometry_file":"missing.wkb","mesh_files":["missing.mesh"],"owner_cells":[[0,0]],
        "profiles":[{"id":"max","values_files":["missing.f32"]}],"threads":1,
        "max_working_bytes":268435456,
        "shear_stress":{"units":"US Customary","meshes":[shear_mesh()]}})
}

fn refused(v: Value) -> (String, String) {
    let folder = scratch();
    let r: sloping_session::Request = serde_json::from_value(v).unwrap();
    let err = sloping_session::run(&r, &folder, &folder.join("out")).unwrap_err();
    assert!(!folder.join("out").exists());
    fs::remove_dir_all(folder).unwrap();
    (err.kind().to_string(), err.to_string())
}

#[test]
fn shear_requests_are_validated_before_reads() {
    let mut units = sloping();
    units["shear_stress"]["units"] = json!("Imperial");
    assert_eq!(refused(units).0, "unsupported");
    let mut meshes = sloping();
    meshes["shear_stress"]["meshes"] = json!([shear_mesh(), shear_mesh()]);
    assert_eq!(refused(meshes).0, "invalid");
    let mut times = sloping();
    times["shear_stress"]["meshes"][0]["times"] = json!(0);
    assert_eq!(refused(times).0, "invalid");
    let mut two = sloping();
    two["profiles"] =
        json!([{"id":"a","values_files":["a.f32"]},{"id":"b","values_files":["b.f32"]}]);
    let (kind, message) = refused(two);
    assert_eq!(kind, "unsupported");
    assert!(message.contains("Max profile only"), "{message}");
    let mut pretty = sloping();
    pretty["options"] = json!({"render_mode":"slopingPretty"});
    assert_eq!(refused(pretty).0, "unsupported");
    let mut native = sloping();
    native["geotiff_outputs"] = json!(true);
    native["stream_outputs"] = json!(true);
    assert_eq!(refused(native).0, "unsupported");
    let mut depth = sloping();
    depth["shear_stress"]["depth"] = json!("max");
    assert!(serde_json::from_value::<sloping_session::Request>(depth).is_err());
    let mut unknown = sloping();
    unknown["shear_stress"]["meshes"][0]["extra"] = json!(1);
    assert!(serde_json::from_value::<sloping_session::Request>(unknown).is_err());
}

#[test]
fn boundaries_refuse_shear() {
    let folder = scratch();
    let v = json!({"schema":"ras-commander-boundary-request/1","interpolation":"sloping","model":sloping(),
        "terrain_metadata":{"shape":[256,256],"transform":[0,1,0,256,0,-1],"tile_shape":[256,256],
        "tile_grid":[1,1],"selected_tiles":[0],"mask":{"path":"mask","dtype":"|u1","shape":[1,65536]},
        "perimeter":{"path":"perimeter","dtype":"<f4","shape":[1,513]}}});
    let r: boundary_session::Request = serde_json::from_value(v).unwrap();
    let err = boundary_session::run(&r, &folder, &folder.join("out"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("shear"), "{err}");
    fs::remove_dir_all(folder).unwrap();
}

#[test]
fn capability_is_advertised() {
    let record = sloping_session::method_record();
    assert_eq!(record["shear_stress"], "rasmapper-face-shear-max/1");
    assert_eq!(
        record["shear_stress_depth"],
        json!(["rendered", "horizontal"])
    );
    let method = ras_commander_fast_map::shear::method_record();
    assert_eq!(method["method_id"], "rasmapper-face-shear-max/1");
    assert_eq!(method["profiles"], json!(["Max"]));
}
