use ras_commander_fast_map::{MapError, one_d_session};
use serde_json::{Value, json};
use std::{fs, path::PathBuf};
struct Fixture {
    root: PathBuf,
    request: Value,
}
impl Fixture {
    fn new() -> Self {
        static NEXT_FIXTURE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "xs-clips-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT_FIXTURE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let points = [
            [0_f64, 0., 1., -2.],
            [0., 4., 1., 2.],
            [4., 0., 0., -2.],
            [4., 4., 0., 2.],
        ];
        fs::write(
            root.join("points.bin"),
            points
                .iter()
                .chain(points.iter())
                .flatten()
                .flat_map(|x| x.to_le_bytes())
                .collect::<Vec<_>>(),
        )
        .unwrap();
        for (name, v) in [
            (
                "triangles.bin",
                vec![0_i32, 2, 1, 1, 2, 3, 0, 2, 1, 1, 2, 3],
            ),
            ("info.bin", vec![0, 4, 0, 2, 4, 4, 2, 2]),
            ("ids.bin", vec![0, 1, 2, 3]),
            ("ranks.bin", vec![0, 1]),
        ] {
            fs::write(
                root.join(name),
                v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<_>>(),
            )
            .unwrap();
        }
        for (name, v) in [
            ("levels.bin", vec![10_f32, 10., 15., 15.]),
            ("terrain.bin", vec![0.; 16]),
            ("crop.bin", vec![0.; 4]),
        ] {
            fs::write(
                root.join(name),
                v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<_>>(),
            )
            .unwrap();
        }
        fs::write(
            root.join("m.bin"),
            [-2_f64, 2., -2., 2., -2., 2., -2., 2.]
                .iter()
                .flat_map(|x| x.to_le_bytes())
                .collect::<Vec<_>>(),
        )
        .unwrap();
        let mut clips = [f32::NAN; 32];
        clips[2] = 0.;
        clips[3] = 20.;
        clips[10] = 0.;
        clips[11] = 20.;
        fs::write(
            root.join("clips.bin"),
            clips
                .iter()
                .flat_map(|x| x.to_le_bytes())
                .collect::<Vec<_>>(),
        )
        .unwrap();
        let request = json!({"schema":"fast-map-one-d-diagnostic/3","scope":"xs-surface-only","points_file":"points.bin","point_count":8,"triangles_file":"triangles.bin","triangle_count":4,"tin_info_file":"info.bin","xs_ids_file":"ids.bin","segment_count":2,"cross_section_count":4,"levels_file":"levels.bin","terrain_file":"terrain.bin","width":4,"height":4,"geo_transform":[0.,1.,0.,4.,0.,-1.],"render_order_file":"ranks.bin","tile_size":2,"normalized_m_file":"m.bin","xs_clip_file":"clips.bin","clipping_scope":"levees-and-xs-end-obstructions-only","max_working_bytes":1_000_000});
        Self { root, request }
    }
    fn run(&self, request: Value, name: &str) -> Result<Value, MapError> {
        one_d_session::run(
            &serde_json::from_value(request).unwrap(),
            &self.root,
            &self.root.join(name),
        )
    }
    fn values(&self, folder: &str, product: &str) -> Vec<f32> {
        fs::read(self.root.join(folder).join(format!("{product}.bin")))
            .unwrap()
            .as_chunks::<4>()
            .0
            .iter()
            .map(|x| f32::from_le_bytes(*x))
            .collect()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
#[test]
fn stored_grid_xs_changes_delivery_contract_without_changing_numerics() {
    let f = Fixture::new();
    let mut legacy = f.request.clone();
    legacy["tile_size"] = json!(256);
    f.run(legacy.clone(), "old-grid").unwrap();
    let mut selected = legacy.clone();
    selected["schema"] = json!("fast-map-one-d-diagnostic/4");
    selected["grid_contract"] = json!({"id":"rasmapper-stored-tile-grid/1","source_width":4,"source_height":4,"tile_size":256,
        "source_geo_transform":[0.,1.,0.,4.,0.,-1.],"output_geo_transform":[0.,1.,0.,4.,0.,-1.]});
    let report = f.run(selected.clone(), "stored-grid").unwrap();
    assert_eq!(
        report["method"]["method_id"],
        "one-d-xs-surface-diagnostic/5"
    );
    assert_eq!(report["grid_contract"], selected["grid_contract"]);
    for product in ["depth", "wse"] {
        assert_eq!(
            fs::read(f.root.join("old-grid").join(format!("{product}.bin"))).unwrap(),
            fs::read(f.root.join("stored-grid").join(format!("{product}.bin"))).unwrap()
        );
    }
    selected["schema"] = legacy["schema"].clone();
    assert!(matches!(
        f.run(selected, "bad-legacy"),
        Err(MapError::Unsupported(_))
    ));
    assert!(!f.root.join("bad-legacy").exists());
}

#[test]
fn clipped_owner_retains_nodata_and_crop_uses_global_m_history() {
    let f = Fixture::new();
    let report = f.run(f.request.clone(), "full").unwrap();
    assert_eq!(
        report["method"]["method_id"],
        "one-d-xs-surface-diagnostic/5"
    );
    assert_eq!(report["valid_pixels"], 8);
    let full = f.values("full", "wse");
    assert!(full[..8].iter().all(|x| x.is_nan()));
    assert_eq!(&full[8..], &[10.; 8]);
    let mut crop = f.request.clone();
    crop["window"] = json!({"col":1,"row":1,"width":2,"height":2});
    crop["terrain_file"] = json!("crop.bin");
    f.run(crop, "crop").unwrap();
    for product in ["depth", "wse"] {
        let full = f.values("full", product);
        let crop = f.values("crop", product);
        for (a, b) in crop.iter().zip([full[5], full[6], full[9], full[10]]) {
            assert_eq!(a.is_nan(), b.is_nan());
            if !a.is_nan() {
                assert_eq!(a.to_bits(), b.to_bits());
            }
        }
    }
    let mut old = f.request.clone();
    old["schema"] = json!("fast-map-one-d-diagnostic/2");
    for key in ["normalized_m_file", "xs_clip_file", "clipping_scope"] {
        old.as_object_mut().unwrap().remove(key);
    }
    let report = f.run(old, "legacy-four").unwrap();
    assert_eq!(
        report["method"]["method_id"],
        "one-d-xs-surface-diagnostic/4"
    );
    assert_eq!(f.values("legacy-four", "wse"), vec![10.; 16]);
}
#[test]
fn clipped_options_and_payloads_are_explicit_and_admitted() {
    let f = Fixture::new();
    for key in ["normalized_m_file", "xs_clip_file", "clipping_scope"] {
        let mut r = f.request.clone();
        r.as_object_mut().unwrap().remove(key);
        assert!(matches!(f.run(r, key), Err(MapError::Invalid(_))));
        assert!(!f.root.join(key).exists());
    }
    let mut old = f.request.clone();
    old["schema"] = json!("fast-map-one-d-diagnostic/2");
    assert!(matches!(
        f.run(old, "unexpected"),
        Err(MapError::Invalid(_))
    ));
    let mut budget = f.request.clone();
    budget["max_working_bytes"] = json!(1);
    budget["points_file"] = json!("absent");
    assert!(matches!(f.run(budget, "budget"), Err(MapError::Budget(_))));
    assert!(!f.root.join("budget").exists());
    fs::write(f.root.join("m.bin"), [0_u8; 8]).unwrap();
    assert!(f.run(f.request.clone(), "short-m").is_err());
    assert!(!f.root.join("short-m").exists());
}
