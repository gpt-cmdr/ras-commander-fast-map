use ras_commander_fast_map::{MapError, one_d_session};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
    request: Value,
    terrain: Vec<f32>,
}

fn request() -> Value {
    json!({
        "schema": "fast-map-one-d-diagnostic/2", "scope": "xs-surface-only",
        "points_file": "points.bin", "point_count": 8,
        "triangles_file": "triangles.bin", "triangle_count": 4,
        "tin_info_file": "info.bin", "xs_ids_file": "ids.bin", "segment_count": 2,
        "cross_section_count": 4, "levels_file": "levels.bin", "terrain_file": "terrain.bin",
        "width": 6, "height": 5, "geo_transform": [1000.125, 1.25, 0., 2000.25, 0., -1.25],
        "render_order_file": "ranks.bin", "tile_size": 2, "max_working_bytes": 1_000_000
    })
}

fn write_f32(path: &Path, values: &[f32]) {
    fs::write(
        path,
        values
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect::<Vec<_>>(),
    )
    .unwrap();
}

fn write_i32(path: &Path, values: &[i32]) {
    fs::write(
        path,
        values
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect::<Vec<_>>(),
    )
    .unwrap();
}

fn read_f32(path: &Path) -> Vec<f32> {
    let bytes = fs::read(path).unwrap();
    assert_eq!(bytes.len() % 4, 0);
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|value| f32::from_le_bytes(*value))
        .collect()
}

impl Fixture {
    fn empty() -> Self {
        let root = std::env::temp_dir().join(format!(
            "fast-map-native-transport-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self {
            root,
            request: request(),
            terrain: Vec::new(),
        }
    }

    fn populated() -> Self {
        let mut fixture = Self::empty();
        // Two complete overlapping rectangles. The lower-water surface is
        // stored last but ranked first, so rank changes have observable output.
        let rectangle: [[f64; 4]; 4] = [
            [998.875, 1992.75, 1., -1.],
            [998.875, 2001.5, 1., 1.],
            [1008.875, 1992.75, 0., -1.],
            [1008.875, 2001.5, 0., 1.],
        ];
        fs::write(
            fixture.root.join("points.bin"),
            rectangle
                .iter()
                .chain(rectangle.iter())
                .flatten()
                .flat_map(|value| value.to_le_bytes())
                .collect::<Vec<_>>(),
        )
        .unwrap();
        write_i32(
            &fixture.root.join("triangles.bin"),
            &[0, 2, 1, 1, 2, 3, 0, 2, 1, 1, 2, 3],
        );
        write_i32(&fixture.root.join("info.bin"), &[0, 4, 0, 2, 4, 4, 2, 2]);
        write_i32(&fixture.root.join("ids.bin"), &[0, 1, 2, 3]);
        write_i32(&fixture.root.join("ranks.bin"), &[1, 0]);
        write_f32(&fixture.root.join("levels.bin"), &[20., 18., 12., 8.]);
        fixture.terrain = vec![0.; 30];
        fixture.terrain[1] = f32::NAN;
        fixture.terrain[8] = 30.;
        fixture.terrain[16] = f32::NAN;
        write_f32(&fixture.root.join("terrain.bin"), &fixture.terrain);
        fixture
    }

    fn run(&self, request: Value, name: &str) -> Result<Value, MapError> {
        one_d_session::run(
            &serde_json::from_value(request).unwrap(),
            &self.root,
            &self.root.join(name),
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn native_schema_requires_rank_file_and_tile_size_before_payload_reads() {
    let fixture = Fixture::empty();
    for field in ["render_order_file", "tile_size"] {
        let mut invalid = fixture.request.clone();
        invalid.as_object_mut().unwrap().remove(field);
        assert!(matches!(
            fixture.run(invalid, field),
            Err(MapError::Invalid(_))
        ));
        assert!(!fixture.root.join(field).exists());
    }
    let mut both_missing = fixture.request.clone();
    both_missing
        .as_object_mut()
        .unwrap()
        .remove("render_order_file");
    both_missing.as_object_mut().unwrap().remove("tile_size");
    assert!(matches!(
        fixture.run(both_missing, "both-missing"),
        Err(MapError::Invalid(_))
    ));
    assert!(!fixture.root.join("both-missing").exists());
}

#[test]
fn legacy_schema_refuses_each_native_option_before_payload_reads() {
    let fixture = Fixture::empty();
    let mut legacy = fixture.request.clone();
    legacy["schema"] = json!("fast-map-one-d-diagnostic/1");
    legacy.as_object_mut().unwrap().remove("render_order_file");
    legacy.as_object_mut().unwrap().remove("tile_size");
    for (field, value) in [
        ("render_order_file", json!("ranks.bin")),
        ("tile_size", json!(2)),
        (
            "window",
            json!({"col": 0, "row": 0, "width": 1, "height": 1}),
        ),
    ] {
        let mut invalid = legacy.clone();
        invalid[field] = value;
        assert!(matches!(
            fixture.run(invalid, field),
            Err(MapError::Invalid(_))
        ));
        assert!(!fixture.root.join(field).exists());
    }
}

#[test]
fn native_budget_is_admitted_before_any_payload_read() {
    let fixture = Fixture::empty();
    let mut denied = fixture.request.clone();
    denied["max_working_bytes"] = json!(1);
    assert!(matches!(
        fixture.run(denied, "denied"),
        Err(MapError::Budget(_))
    ));
    assert!(!fixture.root.join("denied").exists());
}

#[test]
fn malformed_rank_payload_and_nonpermutations_never_create_outputs() {
    let fixture = Fixture::populated();
    fs::write(fixture.root.join("ranks.bin"), [0_u8, 0, 0]).unwrap();
    assert!(matches!(
        fixture.run(fixture.request.clone(), "truncated"),
        Err(MapError::Invalid(_))
    ));
    assert!(!fixture.root.join("truncated").exists());
    for (name, ranks) in [("duplicate", [0, 0]), ("outside", [0, 2])] {
        write_i32(&fixture.root.join("ranks.bin"), &ranks);
        assert!(matches!(
            fixture.run(fixture.request.clone(), name),
            Err(MapError::Invalid(_))
        ));
        assert!(!fixture.root.join(name).exists());
    }
}

#[test]
fn native_window_preserves_global_tile_history_and_complete_masks() {
    let fixture = Fixture::populated();
    let full_report = fixture.run(fixture.request.clone(), "full").unwrap();
    assert_eq!(
        full_report["method"]["method_id"],
        "one-d-xs-surface-diagnostic/4"
    );
    let (col, row, width, height) = (1, 1, 4, 3);
    let cropped_terrain: Vec<f32> = (row..row + height)
        .flat_map(|r| {
            fixture.terrain[r * 6 + col..r * 6 + col + width]
                .iter()
                .copied()
        })
        .collect();
    write_f32(&fixture.root.join("crop-terrain.bin"), &cropped_terrain);
    let mut crop = fixture.request.clone();
    crop["terrain_file"] = json!("crop-terrain.bin");
    crop["window"] = json!({"col": col, "row": row, "width": width, "height": height});
    let report = fixture.run(crop, "crop").unwrap();
    assert_eq!(report["width"], width);
    assert_eq!(report["height"], height);
    assert_eq!(
        report["geo_transform"],
        json!([1001.375, 1.25, 0., 1999., 0., -1.25])
    );
    for product in ["depth", "wse"] {
        let full = read_f32(&fixture.root.join(format!("full/{product}.bin")));
        let cropped = read_f32(&fixture.root.join(format!("crop/{product}.bin")));
        assert_eq!(full.len(), 30);
        assert_eq!(cropped.len(), width * height);
        assert!(full.iter().any(|value| value.is_finite()));
        assert!(cropped.iter().any(|value| value.is_finite()));
        assert!(cropped.iter().any(|value| value.is_nan()));
        for r in 0..height {
            for c in 0..width {
                let a = full[(row + r) * 6 + col + c];
                let b = cropped[r * width + c];
                assert_eq!(a.is_nan(), b.is_nan(), "{product} {r},{c} mask");
                if a.is_finite() {
                    assert_eq!(a.to_bits(), b.to_bits(), "{product} {r},{c} bits");
                }
            }
        }
    }
    let wse = read_f32(&fixture.root.join("full/wse.bin"));
    assert!(wse.iter().filter(|z| z.is_finite()).all(|z| *z < 12.));
    assert!(wse[1].is_nan());
    assert!(wse[8].is_nan());
    assert!(wse[16].is_nan());
}

#[test]
fn cli_advertises_both_legacy_and_native_methods_and_maps_schema_two() {
    let methods = Command::new(env!("CARGO_BIN_EXE_ras2fim-batch"))
        .arg("method")
        .output()
        .unwrap();
    assert!(methods.status.success());
    let methods: Value = serde_json::from_slice(&methods.stdout).unwrap();
    assert_eq!(
        methods["one_d_diagnostic"]["method_id"],
        "one-d-xs-surface-diagnostic/3"
    );
    assert_eq!(
        methods["one_d_native_diagnostic"]["method_id"],
        "one-d-xs-surface-diagnostic/4"
    );
    let fixture = Fixture::populated();
    fs::write(
        fixture.root.join("request.json"),
        serde_json::to_vec(&fixture.request).unwrap(),
    )
    .unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_ras2fim-batch"))
        .arg("one-d-diagnostic")
        .arg(fixture.root.join("request.json"))
        .arg(fixture.root.join("cli"))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let report: Value =
        serde_json::from_slice(&fs::read(fixture.root.join("cli/report.json")).unwrap()).unwrap();
    assert_eq!(report["schema"], "fast-map-one-d-diagnostic/2");
    assert_eq!(
        report["method"]["method_id"],
        "one-d-xs-surface-diagnostic/4"
    );
    assert!(report["valid_pixels"].as_u64().unwrap() > 0);
    assert_eq!(
        fs::metadata(fixture.root.join("cli/depth.bin"))
            .unwrap()
            .len(),
        30 * 4
    );
    assert_eq!(
        fs::metadata(fixture.root.join("cli/wse.bin"))
            .unwrap()
            .len(),
        30 * 4
    );
}
