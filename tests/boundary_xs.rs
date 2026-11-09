use ras_commander_fast_map::{
    MapError,
    boundary_xs::{PreparedXs, XsInput, planned_bytes},
};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    root: PathBuf,
    descriptor: Value,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "boundary-xs-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        for (name, bytes) in [
            (
                "points.bin",
                [0., 0., 0.5, 0., 1., 0., 0.5, 0., 0., 1., 0.5, 0.]
                    .iter()
                    .flat_map(|v: &f64| v.to_le_bytes())
                    .collect(),
            ),
            (
                "triangles.bin",
                [0_i32, 1, 2].iter().flat_map(|v| v.to_le_bytes()).collect(),
            ),
            (
                "info.bin",
                [0_i32, 3, 0, 1]
                    .iter()
                    .flat_map(|v| v.to_le_bytes())
                    .collect(),
            ),
            (
                "ids.bin",
                [0_i32, 1].iter().flat_map(|v| v.to_le_bytes()).collect(),
            ),
            ("ranks.bin", 0_u32.to_le_bytes().to_vec()),
            (
                "m.bin",
                [0_f64; 3].iter().flat_map(|v| v.to_le_bytes()).collect(),
            ),
            (
                "clips.bin",
                [f32::NAN; 16]
                    .iter()
                    .flat_map(|v| v.to_le_bytes())
                    .collect(),
            ),
            (
                "first.bin",
                [10_f32; 2].iter().flat_map(|v| v.to_le_bytes()).collect(),
            ),
            (
                "second.bin",
                [20_f32; 2].iter().flat_map(|v| v.to_le_bytes()).collect(),
            ),
        ] {
            fs::write(root.join(name), bytes).unwrap();
        }
        let descriptor = json!({"point_count":3,"triangle_count":1,"segment_count":1,"cross_section_count":2,
            "points_file":"points.bin","triangles_file":"triangles.bin","tin_info_file":"info.bin",
            "xs_ids_file":"ids.bin","render_order_file":"ranks.bin","normalized_m_file":"m.bin",
            "xs_clip_file":"clips.bin","clipping_scope":"levees-and-xs-end-obstructions-only",
            "profiles":[{"id":"zulu","levels_file":"first.bin"},{"id":"alpha","levels_file":"second.bin"}]});
        Self { root, descriptor }
    }
    fn input(&self) -> XsInput {
        serde_json::from_value(self.descriptor.clone()).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        for entry in fs::read_dir(&self.root).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_file() {
                let _ = fs::remove_file(entry.path());
            }
        }
        let _ = fs::remove_dir(&self.root);
    }
}

#[test]
fn payloads_preserve_declared_profile_order_and_support_signed_sampling() {
    let f = Fixture::new();
    let p = PreparedXs::load(&f.input(), &f.root, 1 << 20).unwrap();
    assert_eq!(p.profile_ids().collect::<Vec<_>>(), ["zulu", "alpha"]);
    assert_eq!(p.levels("alpha").unwrap(), [20.; 2]);
    assert!(p.levels("ALPHA").is_err());
    let (mut d, mut w, mut claims) = ([0.], [0.], [0]);
    p.surface()
        .sample_signed_points(
            p.levels("zulu").unwrap(),
            &[[0.25, 0.25]],
            &[15.],
            &mut d,
            &mut w,
            &mut claims,
        )
        .unwrap();
    assert_eq!(d, [-5.]);
    assert_eq!(w, [10.]);
    assert_eq!(claims, [1]);
}

#[test]
fn unknown_and_legacy_raster_fields_are_not_accepted() {
    let f = Fixture::new();
    for key in [
        "terrain_file",
        "tile_size",
        "scope",
        "width",
        "grid_contract",
    ] {
        let mut descriptor = f.descriptor.clone();
        descriptor[key] = json!(1);
        assert!(serde_json::from_value::<XsInput>(descriptor).is_err());
    }
    let mut descriptor = f.descriptor.clone();
    descriptor["profiles"][0]["extra"] = json!(true);
    assert!(serde_json::from_value::<XsInput>(descriptor).is_err());
}

#[test]
fn budget_counts_and_profile_identity_are_admitted_before_payload_open() {
    let f = Fixture::new();
    let mut input = f.input();
    input.points_file = PathBuf::from("absent.bin");
    assert!(matches!(
        PreparedXs::load(&input, &f.root, 1),
        Err(MapError::Budget(_))
    ));
    input.point_count = usize::MAX;
    assert!(matches!(planned_bytes(&input), Err(MapError::Budget(_))));
    for (field, value) in [
        ("point_count", json!(0)),
        ("cross_section_count", json!(1)),
        ("clipping_scope", json!("other")),
        ("profiles", json!([])),
    ] {
        let mut d = f.descriptor.clone();
        d[field] = value;
        let mut input: XsInput = serde_json::from_value(d).unwrap();
        input.points_file = "absent.bin".into();
        assert!(matches!(
            PreparedXs::load(&input, &f.root, 1 << 20),
            Err(MapError::Invalid(_))
        ));
    }
    for name in ["ZULU", "../unsafe"] {
        let mut input = f.input();
        input.profiles[1].id = name.into();
        input.points_file = "absent.bin".into();
        assert!(matches!(
            PreparedXs::load(&input, &f.root, 1 << 20),
            Err(MapError::Invalid(_))
        ));
    }
}

#[test]
fn all_profile_file_lengths_are_checked_before_geometry_numbers() {
    let f = Fixture::new();
    fs::write(
        f.root.join("points.bin"),
        [f64::NAN; 12]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect::<Vec<_>>(),
    )
    .unwrap();
    fs::write(f.root.join("second.bin"), [0_u8]).unwrap();
    let error = match PreparedXs::load(&f.input(), &f.root, 1 << 20) {
        Ok(_) => panic!("accepted invalid files"),
        Err(e) => e,
    };
    assert!(error.to_string().contains("second.bin"));
}

#[test]
fn numeric_payloads_fail_closed_for_ranges_ranks_m_clips_and_levels() {
    for (name, bytes) in [
        (
            "info.bin",
            [-1_i32, 3, 0, 1]
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect(),
        ),
        ("ranks.bin", 1_u32.to_le_bytes().to_vec()),
        (
            "m.bin",
            [f64::INFINITY; 3]
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect(),
        ),
        (
            "clips.bin",
            [f32::INFINITY; 16]
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect(),
        ),
        (
            "first.bin",
            [f32::NAN; 2].iter().flat_map(|v| v.to_le_bytes()).collect(),
        ),
        (
            "second.bin",
            [f32::INFINITY; 2]
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect(),
        ),
    ] {
        let f = Fixture::new();
        fs::write(f.root.join(name), bytes).unwrap();
        assert!(
            PreparedXs::load(&f.input(), &f.root, 1 << 20).is_err(),
            "{name}"
        );
    }
}
