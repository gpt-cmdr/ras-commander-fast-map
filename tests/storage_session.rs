use ras_commander_fast_map::{
    coupled::{self, Products, Storage},
    storage_session,
};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};
static NEXT: AtomicUsize = AtomicUsize::new(0);
fn scratch() -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "storage-session-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&p).unwrap();
    p
}
fn polygon() -> Vec<u8> {
    let mut wkb = vec![1];
    wkb.extend(3_u32.to_le_bytes());
    wkb.extend(1_u32.to_le_bytes());
    wkb.extend(5_u32.to_le_bytes());
    for [x, y] in [[0_f64, 0_f64], [3., 0.], [3., 1.], [0., 1.], [0., 0.]] {
        wkb.extend(x.to_le_bytes());
        wkb.extend(y.to_le_bytes());
    }
    let mut out = (wkb.len() as u32).to_le_bytes().to_vec();
    out.extend(wkb);
    out
}
fn request() -> Value {
    json!({"schema":"fast-map-storage/1","storage_areas_file":"areas.wkb","storage_area_count":1,
        "levels_file":"levels.bin","terrain_file":"terrain.bin","width":4,"height":1,
        "geo_transform":[0.,1.,0.,1.,0.,-1.],"max_working_bytes":16777216,"threads":2})
}
fn input(p: &Path) {
    fs::write(p.join("areas.wkb"), polygon()).unwrap();
    fs::write(p.join("levels.bin"), 10_f32.to_le_bytes()).unwrap();
    fs::write(
        p.join("terrain.bin"),
        [9_f32, 11., f32::NAN, 0.]
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect::<Vec<_>>(),
    )
    .unwrap();
}
fn floats(path: PathBuf) -> Vec<f32> {
    fs::read(path)
        .unwrap()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|v| f32::from_le_bytes(*v))
        .collect()
}
#[test]
fn storage_only_cli_keeps_dry_coverage_and_native_bits_for_composition() {
    let p = scratch();
    input(&p);
    fs::write(
        p.join("request.json"),
        serde_json::to_vec(&request()).unwrap(),
    )
    .unwrap();
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_ras2fim-batch"))
        .arg("storage")
        .arg(p.join("request.json"))
        .arg(p.join("out"))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let report: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["storage_owned_pixels"], 3);
    assert_eq!(report["valid_pixels"], 1);
    let d = floats(p.join("out/depth.bin"));
    let z = floats(p.join("out/wse.bin"));
    assert_eq!(d[0].to_bits(), 1_f32.to_bits());
    assert_eq!(z[0].to_bits(), 10_f32.to_bits());
    assert!(d[1..].iter().all(|v| v.is_nan()));
    let labels: Vec<u32> = fs::read(p.join("out/owners.bin"))
        .unwrap()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|v| u32::from_le_bytes(*v))
        .collect();
    assert_eq!(labels, [1, 1, 1, 0]);
    let dry = vec![f32::NAN; 4];
    let mut cd = dry.clone();
    let mut cw = dry.clone();
    let counts = coupled::compose_with_storage_into(
        &[0; 4],
        0,
        Products {
            depth: &[5.; 4],
            wse: &[20.; 4],
        },
        Products {
            depth: &dry,
            wse: &dry,
        },
        Storage {
            owners: &labels,
            area_count: 1,
            products: Products { depth: &d, wse: &z },
        },
        &mut cd,
        &mut cw,
    )
    .unwrap();
    assert_eq!(counts.dry_storage_suppressed_wet_xs, 2);
    assert!(cd[1].is_nan() && cd[2].is_nan());
    assert_eq!(cd[3], 5.);
    fs::remove_dir_all(p).unwrap();
}
#[test]
fn storage_admits_before_payload_reads_and_refuses_overlap() {
    let p = scratch();
    fs::write(p.join("areas.wkb"), polygon()).unwrap();
    let mut v = request();
    v["max_working_bytes"] = 1.into();
    let err =
        storage_session::run(&serde_json::from_value(v).unwrap(), &p, &p.join("out")).unwrap_err();
    assert_eq!(err.kind(), "budget");
    assert!(!p.join("out").exists());
    input(&p);
    let mut polygons = polygon();
    polygons.extend(polygon());
    fs::write(p.join("areas.wkb"), polygons).unwrap();
    fs::write(
        p.join("levels.bin"),
        [10_f32, 10.]
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let mut v = request();
    v["storage_area_count"] = 2.into();
    let err =
        storage_session::run(&serde_json::from_value(v).unwrap(), &p, &p.join("out")).unwrap_err();
    assert_eq!(err.kind(), "unsupported");
    assert!(!p.join("out").exists());
    fs::remove_dir_all(p).unwrap();
}
#[test]
fn storage_rejects_nonfinite_levels_and_invalid_grid_threads() {
    let p = scratch();
    input(&p);
    for (key, value) in [
        ("width", json!(usize::MAX)),
        ("threads", json!(0)),
        ("threads", json!(5)),
        ("schema", json!("unknown")),
    ] {
        let mut v = request();
        v[key] = value;
        assert!(
            storage_session::run(&serde_json::from_value(v).unwrap(), &p, &p.join("out")).is_err()
        );
    }
    fs::write(p.join("levels.bin"), f32::NAN.to_le_bytes()).unwrap();
    assert!(
        storage_session::run(
            &serde_json::from_value(request()).unwrap(),
            &p,
            &p.join("out")
        )
        .is_err()
    );
    assert!(!p.join("out").exists());
    fs::remove_dir_all(p).unwrap();
}
