//! Terrain-scope request contract: validated before reads, refused for
//! cell-face sloping and boundaries, advertised capability.
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
        "fast-map-scope-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&p).unwrap();
    p
}
fn scope(kind: &str) -> Value {
    json!({"kind":kind,"source_count":2,"mask":{"path":"mask.u1","dtype":"|u1","shape":[4,65536]}})
}
fn sloping() -> Value {
    json!({"schema":"ras-commander-sloping-request/1","terrain":"missing.tif","epsg":26915,
        "crs_identity":{"kind":"epsg"},"terrain_transform":[0,1,0,256,0,-1],"window":[0,0,256,256],
        "geometry_file":"missing.wkb","mesh_files":["missing.mesh"],"owner_cells":[[0,0]],
        "profiles":[{"id":"max","values_files":["missing.f32"]}],"threads":1,
        "max_working_bytes":268435456,"terrain_scope":scope("top_priority_source")})
}

#[test]
fn invalid_scope_is_rejected_before_reads() {
    let folder = scratch();
    let mut v = sloping();
    v["terrain_scope"] = scope("mosaic");
    let r: sloping_session::Request = serde_json::from_value(v).unwrap();
    let err = sloping_session::run(&r, &folder, &folder.join("out")).unwrap_err();
    assert_eq!(err.kind(), "invalid");
    assert!(!folder.join("out").exists());
    let h = json!({"schema":"ras2fim-horizontal-request/1","terrain":"missing.tif","epsg":26915,
        "crs_identity":{"kind":"epsg"},"terrain_transform":[0,1,0,256,0,-1],"window":[0,0,256,256],
        "geometry_file":"missing.wkb","cell_count":1,"profiles":[{"id":"max","values_file":"missing.f32"}],
        "min_depth":0.0,"threads":1,"max_working_bytes":268435456,"terrain_scope":scope("mosaic")});
    let r: horizontal::Request = serde_json::from_value(h).unwrap();
    assert_eq!(
        horizontal::run(&r, &folder, &folder.join("out"))
            .unwrap_err()
            .kind(),
        "invalid"
    );
    fs::remove_dir_all(folder).unwrap();
}

#[test]
fn cell_face_sloping_is_refused() {
    let folder = scratch();
    let mut v = sloping();
    v["options"] = json!({"render_mode":"slopingPretty"});
    let r: sloping_session::Request = serde_json::from_value(v).unwrap();
    let err = sloping_session::run(&r, &folder, &folder.join("out")).unwrap_err();
    assert_eq!(err.kind(), "unsupported");
    fs::remove_dir_all(folder).unwrap();
}

#[test]
fn boundaries_refuse_terrain_scope() {
    let folder = scratch();
    let v = json!({"schema":"ras-commander-boundary-request/1","interpolation":"sloping","model":sloping(),
        "terrain_metadata":{"shape":[256,256],"transform":[0,1,0,256,0,-1],"tile_shape":[256,256],
        "tile_grid":[1,1],"selected_tiles":[0],"mask":{"path":"mask","dtype":"|u1","shape":[1,65536]},
        "perimeter":{"path":"perimeter","dtype":"<f4","shape":[1,513]}}});
    let r: boundary_session::Request = serde_json::from_value(v).unwrap();
    let err = boundary_session::run(&r, &folder, &folder.join("out"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("top-priority"), "{err}");
    fs::remove_dir_all(folder).unwrap();
}

#[test]
fn capability_is_advertised() {
    for record in [
        sloping_session::method_record(),
        horizontal::method_record(),
    ] {
        assert_eq!(record["terrain_scope"], "top-priority-source-footprint/1");
    }
}
