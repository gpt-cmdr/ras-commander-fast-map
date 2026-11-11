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

#[test]
fn multi_profile_requests_match_single_requests_bit_for_bit() {
    let fixture = Fixture::populated();
    let profiles: [[f32; 4]; 3] = [
        [20., 18., 12., 8.],
        [9., f32::NAN, 7., 6.],
        [3., 2., 25., 24.],
    ];
    let mut names = vec![];
    for (i, levels) in profiles.iter().enumerate() {
        let name = format!("levels-{i}.bin");
        write_f32(&fixture.root.join(&name), levels);
        let mut single = fixture.request.clone();
        single["levels_file"] = json!(name);
        fixture.run(single, &format!("single-{i}")).unwrap();
        names.push(name);
    }
    for threads in [1, 3] {
        let mut multi = fixture.request.clone();
        multi.as_object_mut().unwrap().remove("levels_file");
        multi["levels_files"] = json!(names);
        multi["threads"] = json!(threads);
        let out = format!("multi-{threads}");
        let report = fixture.run(multi, &out).unwrap();
        assert_eq!(report["status"], "complete");
        assert_eq!(report["profile_count"], 3);
        assert!(report.get("depth_file").is_none());
        for i in 0..3 {
            let single: Value = serde_json::from_slice(
                &fs::read(fixture.root.join(format!("single-{i}/report.json"))).unwrap(),
            )
            .unwrap();
            let sub: Value = serde_json::from_slice(
                &fs::read(
                    fixture
                        .root
                        .join(format!("{out}/profile-{i:04}/report.json")),
                )
                .unwrap(),
            )
            .unwrap();
            assert_eq!(sub["valid_pixels"], single["valid_pixels"]);
            assert_eq!(
                report["profiles"][i]["valid_pixels"],
                single["valid_pixels"]
            );
            assert_eq!(sub["method"], single["method"]);
            for product in ["depth", "wse"] {
                let a = fs::read(fixture.root.join(format!("single-{i}/{product}.bin"))).unwrap();
                let b = fs::read(
                    fixture
                        .root
                        .join(format!("{out}/profile-{i:04}/{product}.bin")),
                )
                .unwrap();
                assert_eq!(a, b, "profile {i} {product} threads {threads}");
            }
        }
    }
}

#[test]
fn multi_profile_selection_is_refused_before_outputs() {
    let fixture = Fixture::populated();
    write_f32(&fixture.root.join("levels-b.bin"), &[1., 2., 3., 4.]);
    write_f32(&fixture.root.join("short.bin"), &[1., 2., 3.]);
    let mut both = fixture.request.clone();
    both["levels_files"] = json!(["levels.bin"]);
    let mut neither = fixture.request.clone();
    neither.as_object_mut().unwrap().remove("levels_file");
    let mut empty = neither.clone();
    empty["levels_files"] = json!([]);
    let mut too_many = neither.clone();
    too_many["levels_files"] = json!(vec!["levels.bin"; one_d_session::MAX_PROFILES + 1]);
    let mut legacy = neither.clone();
    legacy["schema"] = json!("fast-map-one-d-diagnostic/1");
    for key in ["render_order_file", "tile_size"] {
        legacy.as_object_mut().unwrap().remove(key);
    }
    legacy["levels_files"] = json!(["levels.bin"]);
    let mut short = neither.clone();
    short["levels_files"] = json!(["levels.bin", "short.bin"]);
    let mut missing = neither.clone();
    missing["levels_files"] = json!(["levels-b.bin", "absent.bin"]);
    for (i, request) in [both, neither, empty, too_many, legacy, short, missing]
        .into_iter()
        .enumerate()
    {
        let name = format!("refused-{i}");
        assert!(fixture.run(request, &name).is_err(), "case {i}");
        assert!(
            !fixture.root.join(&name).exists(),
            "case {i} created output"
        );
    }
}

fn read_tif(path: &Path) -> (Vec<f32>, Option<f64>, usize, usize) {
    let mut raster = terrio::LazyRaster::open_tiff(path.to_str().unwrap())
        .unwrap()
        .unwrap();
    let (w, h) = (raster.width(), raster.height());
    let ds = raster
        .read_window(
            terrio::Window {
                x: 0,
                y: 0,
                width: w,
                height: h,
            },
            &terrio::WindowOptions {
                bands: vec![1],
                overview: None,
            },
        )
        .unwrap();
    let terrio::raster::RasterData::F32(values) = ds.raster.data else {
        panic!("not Float32")
    };
    (values, raster.nodata(), w, h)
}

#[test]
fn final_format_writes_raw_bits_with_mask_and_nodata_for_single_and_multi() {
    let fixture = Fixture::populated();
    fixture.run(fixture.request.clone(), "raw").unwrap();
    // Mask out the first row and one interior pixel.
    let mut mask = vec![1_u8; 30];
    mask[..6].fill(0);
    mask[13] = 0;
    fs::write(fixture.root.join("mask.u8"), &mask).unwrap();
    let mut request = fixture.request.clone();
    request["final_format"] = json!({"nodata": -9999.0, "block_size": 16, "overview_factors": [2]});
    request["epsg"] = json!(26915);
    request["aoi_mask_file"] = json!("mask.u8");
    request["flooded_depth_range"] = json!([0.0, 10000.0]);
    let report = fixture.run(request.clone(), "final").unwrap();
    assert_eq!(report["depth_file"], "depth.tif");
    assert_eq!(report["compression"], "DEFLATE");
    assert!(!fixture.root.join("final/depth.bin").exists());
    let raw_depth = read_f32(&fixture.root.join("raw/depth.bin"));
    let mut finite = 0;
    let mut flooded = 0;
    for product in ["depth", "wse"] {
        let raw = read_f32(&fixture.root.join(format!("raw/{product}.bin")));
        let (values, nodata, w, h) = read_tif(&fixture.root.join(format!("final/{product}.tif")));
        assert_eq!((w, h, nodata), (6, 5, Some(-9999.0)));
        for (i, (r, v)) in raw.iter().zip(&values).enumerate() {
            if mask[i] == 0 || r.is_nan() {
                assert_eq!(*v, -9999.0, "{product} {i}");
            } else {
                assert_eq!(r.to_bits(), v.to_bits(), "{product} {i}");
            }
        }
    }
    for (i, d) in raw_depth.iter().enumerate() {
        if mask[i] == 1 && d.is_finite() {
            finite += 1;
            flooded += usize::from(*d > 0.0 && *d <= 10000.0);
        }
    }
    assert!(finite > 0);
    assert_eq!(report["depth_finite_pixels"], finite);
    assert_eq!(report["flooded_pixels"], flooded);
    assert_eq!(report["depth_final"]["valid_pixels"], finite);
    // Multi-profile delivery writes the same files per profile.
    let mut multi = request;
    multi.as_object_mut().unwrap().remove("levels_file");
    multi["levels_files"] = json!(["levels.bin", "levels.bin"]);
    multi["threads"] = json!(2);
    fixture.run(multi, "multi").unwrap();
    for i in 0..2 {
        for product in ["depth", "wse"] {
            assert_eq!(
                fs::read(fixture.root.join(format!("final/{product}.tif"))).unwrap(),
                fs::read(
                    fixture
                        .root
                        .join(format!("multi/profile-{i:04}/{product}.tif"))
                )
                .unwrap(),
                "file bytes are independent of workers and batching"
            );
        }
    }
}

#[test]
fn final_format_options_are_refused_before_outputs() {
    let fixture = Fixture::populated();
    fs::write(fixture.root.join("bad-mask.u8"), [2_u8; 30]).unwrap();
    fs::write(fixture.root.join("short-mask.u8"), [1_u8; 29]).unwrap();
    let base = {
        let mut r = fixture.request.clone();
        r["final_format"] = json!({"nodata": -9999.0});
        r["epsg"] = json!(26915);
        r["flooded_depth_range"] = json!([0.0, 10000.0]);
        r
    };
    let mut cases = vec![];
    for key in ["epsg", "flooded_depth_range"] {
        let mut r = base.clone();
        r.as_object_mut().unwrap().remove(key);
        cases.push(r);
    }
    let mut r = fixture.request.clone();
    r["epsg"] = json!(26915);
    cases.push(r);
    let mut r = base.clone();
    r["epsg"] = json!(1);
    cases.push(r);
    let mut r = base.clone();
    r["flooded_depth_range"] = json!([1.0, 0.0]);
    cases.push(r);
    let mut r = base.clone();
    r["final_format"] = json!({"nodata": 0.1});
    cases.push(r);
    for mask in ["bad-mask.u8", "short-mask.u8", "absent.u8"] {
        let mut r = base.clone();
        r["aoi_mask_file"] = json!(mask);
        cases.push(r);
    }
    for (i, request) in cases.into_iter().enumerate() {
        let name = format!("refused-{i}");
        assert!(fixture.run(request, &name).is_err(), "case {i}");
        assert!(
            !fixture.root.join(&name).exists(),
            "case {i} created output"
        );
    }
}

fn storage_polygon() -> Vec<u8> {
    // One rectangle over columns 0-3 and rows 0-1 of the populated fixture grid.
    let mut wkb = vec![1];
    wkb.extend(3_u32.to_le_bytes());
    wkb.extend(1_u32.to_le_bytes());
    wkb.extend(5_u32.to_le_bytes());
    for [x, y] in [
        [1000.125_f64, 1997.75_f64],
        [1005.125, 1997.75],
        [1005.125, 2000.25],
        [1000.125, 2000.25],
        [1000.125, 1997.75],
    ] {
        wkb.extend(x.to_le_bytes());
        wkb.extend(y.to_le_bytes());
    }
    let mut out = (wkb.len() as u32).to_le_bytes().to_vec();
    out.extend(wkb);
    out
}

#[test]
fn inline_storage_matches_storage_then_composition_bits() {
    let fixture = Fixture::populated();
    fs::write(fixture.root.join("areas.wkb"), storage_polygon()).unwrap();
    write_f32(&fixture.root.join("pool-wet.bin"), &[5.0]);
    write_f32(&fixture.root.join("pool-dry.bin"), &[-1.0]);
    let gt = json!([1000.125, 1.25, 0., 2000.25, 0., -1.25]);
    let pools = ["pool-wet.bin", "pool-dry.bin"];
    // Three-call reference per profile.
    for (i, pool) in pools.iter().enumerate() {
        fixture
            .run(fixture.request.clone(), &format!("xs-{i}"))
            .unwrap();
        let storage: ras_commander_fast_map::storage_session::Request = serde_json::from_value(json!({
            "schema": "fast-map-storage/1", "storage_areas_file": "areas.wkb", "storage_area_count": 1,
            "levels_file": pool, "terrain_file": "terrain.bin", "width": 6, "height": 5,
            "geo_transform": gt, "max_working_bytes": 16_777_216, "threads": 2})).unwrap();
        ras_commander_fast_map::storage_session::run(
            &storage,
            &fixture.root,
            &fixture.root.join(format!("pool-{i}")),
        )
        .unwrap();
        fs::write(fixture.root.join("zero.u32"), [0_u8; 120]).unwrap();
        write_f32(&fixture.root.join("dry.f32"), &[f32::NAN; 30]);
        let component = |d: String, w: String| {
            json!({"profile_id": "p", "crs": "c", "width": 6, "height": 5,
            "geo_transform": gt, "depth_file": d, "wse_file": w})
        };
        let compose: ras_commander_fast_map::coupled_session::Request = serde_json::from_value(json!({
            "schema": "fast-map-coupled-diagnostic/2",
            "xs": component(format!("xs-{i}/depth.bin"), format!("xs-{i}/wse.bin")),
            "two_d": component("dry.f32".into(), "dry.f32".into()),
            "two_d_owners_file": "zero.u32", "two_d_cell_count": 0,
            "storage": {"component": component(format!("pool-{i}/depth.bin"), format!("pool-{i}/wse.bin")),
                "owners_file": format!("pool-{i}/owners.bin"), "area_count": 1},
            "max_working_bytes": 16_777_216})).unwrap();
        ras_commander_fast_map::coupled_session::run(
            &compose,
            &fixture.root,
            &fixture.root.join(format!("composed-{i}")),
        )
        .unwrap();
    }
    for threads in [1, 3] {
        let mut inline = fixture.request.clone();
        inline.as_object_mut().unwrap().remove("levels_file");
        inline["levels_files"] = json!(["levels.bin", "levels.bin"]);
        inline["threads"] = json!(threads);
        inline["storage"] = json!({"storage_areas_file": "areas.wkb", "storage_area_count": 1,
            "levels_files": pools, "geo_transform": gt});
        inline["max_working_bytes"] = json!(16_777_216);
        let out = format!("inline-{threads}");
        fixture.run(inline, &out).unwrap();
        for i in 0..2 {
            for product in ["depth", "wse"] {
                assert_eq!(
                    fs::read(fixture.root.join(format!("composed-{i}/{product}.bin"))).unwrap(),
                    fs::read(
                        fixture
                            .root
                            .join(format!("{out}/profile-{i:04}/{product}.bin"))
                    )
                    .unwrap(),
                    "profile {i} {product} threads {threads}"
                );
            }
            let report: Value = serde_json::from_slice(
                &fs::read(
                    fixture
                        .root
                        .join(format!("{out}/profile-{i:04}/report.json")),
                )
                .unwrap(),
            )
            .unwrap();
            let reference: Value = serde_json::from_slice(
                &fs::read(fixture.root.join(format!("composed-{i}/report.json"))).unwrap(),
            )
            .unwrap();
            assert_eq!(report["composition"]["counts"], reference["counts"]);
            assert_eq!(report["composition"]["method"], reference["method"]);
        }
    }
    // The wet pool must actually change the composition relative to the dry one.
    assert_ne!(
        fs::read(fixture.root.join("composed-0/wse.bin")).unwrap(),
        fs::read(fixture.root.join("composed-1/wse.bin")).unwrap()
    );
    // Refusals before outputs: wrong transform, wrong levels count, stored grid excluded.
    let mut bad = fixture.request.clone();
    bad["max_working_bytes"] = json!(16_777_216);
    bad["storage"] = json!({"storage_areas_file": "areas.wkb", "storage_area_count": 1,
        "levels_files": ["pool-wet.bin"], "geo_transform": [1000.0, 1.25, 0., 2000.25, 0., -1.25]});
    assert!(fixture.run(bad.clone(), "bad-gt").is_err());
    bad["storage"]["geo_transform"] = gt.clone();
    bad["storage"]["levels_files"] = json!(["pool-wet.bin", "pool-dry.bin"]);
    assert!(fixture.run(bad, "bad-count").is_err());
    assert!(!fixture.root.join("bad-gt").exists() && !fixture.root.join("bad-count").exists());
}

#[test]
fn terrain_tiff_matches_staged_terrain_bits_and_refuses_mismatched_grids() {
    use ras_commander_fast_map::{final_tiff, native_output::FinalFormat};
    let fixture = Fixture::populated();
    let gt = [1000.125, 1.25, 0., 2000.25, 0., -1.25];
    let format: FinalFormat =
        serde_json::from_value(json!({"nodata": -9999.0, "block_size": 16})).unwrap();
    let layout = final_tiff::Layout::new(&format, 6, 5, gt).unwrap();
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(2)
        .build()
        .unwrap();
    // NaN pixels become the file's nodata and must read back as NaN.
    final_tiff::write(
        &fixture.root.join("terrain.tif"),
        &fixture.terrain,
        &layout,
        26915,
        &pool,
    )
    .unwrap();
    for (window, threads) in [(None, 1), (Some([1, 1, 4, 3]), 3)] {
        let mut staged = fixture.request.clone();
        let mut direct = fixture.request.clone();
        direct.as_object_mut().unwrap().remove("terrain_file");
        direct["terrain_tiff"] = json!("terrain.tif");
        direct["threads"] = json!(threads);
        direct["max_working_bytes"] = json!(64_000_000);
        if let Some([col, row, width, height]) = window {
            let crop: Vec<f32> = (row..row + height)
                .flat_map(|r| fixture.terrain[r * 6 + col..r * 6 + col + width].to_vec())
                .collect();
            write_f32(&fixture.root.join("crop.bin"), &crop);
            staged["terrain_file"] = json!("crop.bin");
            for r in [&mut staged, &mut direct] {
                r["window"] = json!({"col": col, "row": row, "width": width, "height": height});
            }
        }
        let name = format!("{window:?}").replace(['[', ']', ' ', ','], "");
        let a = fixture.run(staged, &format!("staged{name}")).unwrap();
        let b = fixture.run(direct, &format!("direct{name}")).unwrap();
        assert_eq!(a["valid_pixels"], b["valid_pixels"]);
        for product in ["depth", "wse"] {
            assert_eq!(
                fs::read(fixture.root.join(format!("staged{name}/{product}.bin"))).unwrap(),
                fs::read(fixture.root.join(format!("direct{name}/{product}.bin"))).unwrap(),
                "{product} {window:?}"
            );
        }
    }
    // Header mismatches and ambiguous terrain selection refuse before outputs.
    let other =
        final_tiff::Layout::new(&format, 6, 5, [1000.0, 1.25, 0., 2000.25, 0., -1.25]).unwrap();
    final_tiff::write(
        &fixture.root.join("shifted.tif"),
        &fixture.terrain,
        &other,
        26915,
        &pool,
    )
    .unwrap();
    let mut shifted = fixture.request.clone();
    shifted.as_object_mut().unwrap().remove("terrain_file");
    shifted["terrain_tiff"] = json!("shifted.tif");
    let mut both = fixture.request.clone();
    both["terrain_tiff"] = json!("terrain.tif");
    let mut neither = fixture.request.clone();
    neither.as_object_mut().unwrap().remove("terrain_file");
    for (i, request) in [shifted, both, neither].into_iter().enumerate() {
        let name = format!("terrain-refused-{i}");
        assert!(fixture.run(request, &name).is_err(), "case {i}");
        assert!(!fixture.root.join(&name).exists());
    }
}
