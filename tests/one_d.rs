use ras_commander_fast_map::{
    MapError,
    one_d::{PreparedSurface, Segment},
};

fn points() -> [[f64; 4]; 4] {
    [
        [0., 0., 1., -2.],
        [0., 2., 1., 2.],
        [2., 0., 0., -2.],
        [2., 2., 0., 2.],
    ]
}
fn segment() -> Segment {
    Segment {
        point_start: 0,
        point_count: 4,
        triangle_start: 0,
        triangle_count: 2,
        upstream: 0,
        downstream: 1,
    }
}
fn surface() -> PreparedSurface {
    PreparedSurface::new(&points(), &[[0, 2, 1], [1, 2, 3]], &[segment()], 2, 8192).unwrap()
}

#[test]
fn reuses_geometry_for_profiles_and_preserves_common_mask() {
    let s = surface();
    let first = s.profile(&[12., 8.]).unwrap();
    assert_eq!(first.sample([1., 1.], 3.), Some((7., 10.)));
    assert_eq!(first.sample([0., 1.], 3.), Some((9., 12.)));
    assert_eq!(first.sample([2., 1.], 3.), Some((5., 8.)));
    assert_eq!(first.sample([1., 1.], 10.), None);
    assert_eq!(first.sample([1., 1.], f32::NAN), None);
    assert_eq!(first.sample([3., 1.], 0.), None);
    assert_eq!(
        s.profile(&[16., 12.]).unwrap().sample([1., 1.], 3.),
        Some((11., 14.))
    );
    assert_eq!(
        s.profile(&[f32::NAN, 8.]).unwrap().sample([2., 1.], 0.),
        None
    );
    assert!(s.profile(&[8.]).is_err());
    assert!(s.profile(&[f32::INFINITY, 8.]).is_err());
}

#[test]
fn shared_edge_uses_source_order_for_both_windings() {
    let s = surface();
    assert_eq!(s.locate([1., 1.]).unwrap().triangle, 0);
    let reversed =
        PreparedSurface::new(&points(), &[[1, 2, 0], [3, 2, 1]], &[segment()], 2, 8192).unwrap();
    for p in [[1., 1.], [0.25, 0.5], [1.75, 1.5]] {
        assert_eq!(s.locate(p), reversed.locate(p));
    }
    assert!(s.locate([f64::NAN, 0.]).is_none());
}

#[test]
fn indices_are_local_to_each_segment() {
    let mut pts = points().to_vec();
    pts.extend(points().map(|mut p| {
        p[0] += 3.;
        p
    }));
    let mut second = segment();
    second.point_start = 4;
    second.triangle_start = 2;
    second.upstream = 1;
    second.downstream = 2;
    let s = PreparedSurface::new(
        &pts,
        &[[0, 2, 1], [1, 2, 3], [0, 2, 1], [1, 2, 3]],
        &[segment(), second],
        3,
        8192,
    )
    .unwrap();
    let p = s.locate([4., 1.]).unwrap();
    assert_eq!(
        (p.segment, p.triangle, p.upstream, p.downstream),
        (1, 2, 1, 2)
    );
    assert_eq!(
        s.profile(&[20., 12., 8.]).unwrap().sample([4., 1.], 0.),
        Some((10., 10.))
    );
}

#[test]
fn later_segment_owns_overlap_even_when_dry_or_missing() {
    let mut pts = points().to_vec();
    pts.extend(points());
    let second = Segment {
        point_start: 4,
        triangle_start: 2,
        upstream: 1,
        downstream: 2,
        ..segment()
    };
    let s = PreparedSurface::new(
        &pts,
        &[[0, 2, 1], [1, 2, 3], [0, 2, 1], [1, 2, 3]],
        &[segment(), second],
        3,
        8192,
    )
    .unwrap();
    let hit = s.locate([1., 1.]).unwrap();
    assert_eq!((hit.segment, hit.triangle), (1, 2));
    assert_eq!(
        s.profile(&[12., 8., 2.]).unwrap().sample([1., 1.], 0.),
        Some((5., 5.))
    );
    assert_eq!(
        s.profile(&[12., 8., 2.]).unwrap().sample([1., 1.], 6.),
        None
    );
    assert_eq!(
        s.profile(&[12., 8., f32::NAN])
            .unwrap()
            .sample([1., 1.], 0.),
        None
    );
}

#[test]
fn corrupt_surface_is_refused_instead_of_silently_losing_coverage() {
    for tris in [
        [[0, 2, 4], [1, 2, 3]],
        [[-1, 2, 1], [1, 2, 3]],
        [[0, 0, 1], [1, 2, 3]],
    ] {
        assert!(matches!(
            PreparedSurface::new(&points(), &tris, &[segment()], 2, 8192),
            Err(MapError::Invalid(_))
        ));
    }
    for s in [
        Segment {
            point_start: 1,
            ..segment()
        },
        Segment {
            upstream: 2,
            ..segment()
        },
        Segment {
            triangle_count: 1,
            ..segment()
        },
        Segment {
            point_count: usize::MAX,
            ..segment()
        },
    ] {
        assert!(PreparedSurface::new(&points(), &[[0, 2, 1], [1, 2, 3]], &[s], 2, 8192).is_err());
    }
    let mut p = points();
    p[0][2] = 1.01;
    assert!(PreparedSurface::new(&p, &[[0, 2, 1], [1, 2, 3]], &[segment()], 2, 8192).is_err());
}

#[test]
fn budget_and_size_overflow_are_explicit() {
    assert!(matches!(
        PreparedSurface::planned_bytes(usize::MAX, 1, 1),
        Err(MapError::Budget(_))
    ));
    assert!(matches!(
        PreparedSurface::new(&points(), &[[0, 2, 1], [1, 2, 3]], &[segment()], 2, 0),
        Err(MapError::Budget(_))
    ));
}

#[test]
fn plot_tolerance_and_subtraction_are_binary32() {
    let s = surface();
    let wse = 1.001_f32;
    let levels = [wse, wse];
    let profile = s.profile(&levels).unwrap();
    let (depth, z) = profile.sample([1., 1.], 1.).unwrap();
    assert_eq!(depth.to_bits(), (wse - 1_f32).to_bits());
    assert_eq!(z.to_bits(), wse.to_bits());
    let lower = f32::from_bits(wse.to_bits() - 1);
    assert!(
        s.profile(&[lower, lower])
            .unwrap()
            .sample([1., 1.], 1.)
            .is_none()
    );
}

#[test]
fn weighted_sum_is_not_reassociated_to_difference_form() {
    let s = surface();
    let (_, wse) = s
        .profile(&[12.3, 8.7])
        .unwrap()
        .sample([1.96, 1.], 0.)
        .unwrap();
    assert_eq!(wse.to_bits(), 0x410c5a1d);
    let difference_form = 8.7_f32 + 0.02_f32 * (12.3_f32 - 8.7_f32);
    assert_eq!(difference_form.to_bits(), 0x410c5a1c);
}

#[test]
#[ignore = "requires opt-in external prepared request and native reference buffers"]
fn external_native_crop_masks_and_bits() {
    use std::{fs, path::PathBuf};
    let base = PathBuf::from(
        std::env::var_os("FAST_MAP_ONE_D_FIXTURE").expect("set FAST_MAP_ONE_D_FIXTURE"),
    );
    let request = serde_json::from_slice(&fs::read(base.join("request.json")).unwrap()).unwrap();
    let output = std::env::temp_dir().join(format!(
        "fast-map-one-d-external-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    ras_commander_fast_map::one_d_session::run(&request, &base, &output).unwrap();
    for product in ["depth", "wse"] {
        let candidate = fs::read(output.join(format!("{product}.bin"))).unwrap();
        let reference = fs::read(base.join(format!("expected-{product}.bin"))).unwrap();
        assert_eq!(candidate.len(), reference.len());
        for (index, (a, b)) in candidate
            .as_chunks::<4>()
            .0
            .iter()
            .zip(reference.as_chunks::<4>().0)
            .enumerate()
        {
            let (a, b) = (f32::from_le_bytes(*a), f32::from_le_bytes(*b));
            assert_eq!(a.is_nan(), b.is_nan(), "{product} pixel {index}: mask");
            if !a.is_nan() {
                assert_eq!(a.to_bits(), b.to_bits(), "{product} pixel {index}: bits");
            }
        }
    }
    fs::remove_dir_all(output).unwrap();
}

#[test]
fn cli_maps_binary_payload_and_refuses_budget_before_reads() {
    use std::{fs, process::Command};
    let root = std::env::temp_dir().join(format!(
        "fast-map-one-d-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&root).unwrap();
    let request = serde_json::json!({
        "schema": "fast-map-one-d-diagnostic/1", "scope": "xs-surface-only",
        "points_file": "points.bin", "point_count": 4,
        "triangles_file": "triangles.bin", "triangle_count": 2,
        "tin_info_file": "info.bin", "xs_ids_file": "ids.bin", "segment_count": 1,
        "cross_section_count": 2, "levels_file": "levels.bin", "terrain_file": "terrain.bin",
        "width": 3, "height": 2, "geo_transform": [0., 1., 0., 2., 0., -1.],
        "max_working_bytes": 1_000_000
    });
    let mut denied = request.clone();
    denied["max_working_bytes"] = serde_json::json!(1);
    assert!(matches!(
        ras_commander_fast_map::one_d_session::run(
            &serde_json::from_value(denied).unwrap(),
            &root,
            &root.join("denied")
        ),
        Err(MapError::Budget(_))
    ));
    assert!(!root.join("denied").exists());
    fs::write(
        root.join("points.bin"),
        points()
            .iter()
            .flatten()
            .flat_map(|v| v.to_le_bytes())
            .collect::<Vec<_>>(),
    )
    .unwrap();
    for (name, values) in [
        ("triangles.bin", vec![0_i32, 2, 1, 1, 2, 3]),
        ("info.bin", vec![0, 4, 0, 2]),
        ("ids.bin", vec![0, 1]),
    ] {
        fs::write(
            root.join(name),
            values
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect::<Vec<_>>(),
        )
        .unwrap();
    }
    for (name, values) in [
        ("levels.bin", vec![12_f32, 8.]),
        ("terrain.bin", vec![3., 3., 3., f32::NAN, 10., 0.]),
    ] {
        fs::write(
            root.join(name),
            values
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect::<Vec<_>>(),
        )
        .unwrap();
    }
    fs::write(
        root.join("request.json"),
        serde_json::to_vec(&request).unwrap(),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_ras2fim-batch"))
        .arg("one-d-diagnostic")
        .arg(root.join("request.json"))
        .arg(root.join("out"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("out/report.json")).unwrap()).unwrap();
    assert_eq!(report["valid_pixels"], 2);
    assert_eq!(report["method"]["native_parity_qualified"], false);
    for (file, expected) in [("depth.bin", [8_f32, 6.]), ("wse.bin", [11_f32, 9.])] {
        let raw = fs::read(root.join("out").join(file)).unwrap();
        let vals: Vec<_> = raw
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect();
        assert_eq!(&vals[..2], &expected);
        assert!(vals[2..].iter().all(|v| v.is_nan()));
    }
    fs::remove_dir_all(root).unwrap();
}
