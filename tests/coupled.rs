use ras_commander_fast_map::{
    MapError,
    coupled::{Products, compose_into},
};

#[test]
fn stored_grid_coupled_contract_binds_components_and_refuses_before_reads() {
    let grid = serde_json::json!({"id":"rasmapper-stored-tile-grid/1","source_width":513,"source_height":257,"tile_size":256,
        "source_geo_transform":[100.,2.,0.,1000.,0.,-2.],"output_geo_transform":[100.,2.,0.,1000.,0.,-2.]});
    let component = serde_json::json!({"profile_id":"p","crs":"EPSG:3857","width":2,"height":1,
        "geo_transform":[610.,2.,0.,490.,0.,-2.],"depth_file":"missing.bin","wse_file":"missing.bin","grid_contract":grid});
    let wire = serde_json::json!({"schema":"fast-map-coupled-diagnostic/3","grid_contract":grid,"window":[255,255,2,1],
        "xs":component,"two_d":component,"two_d_owners_file":"missing.bin","two_d_cell_count":1,"max_working_bytes":1_000_000});
    let base = std::path::Path::new("nonexistent-grid-input");
    let run = |wire| {
        ras_commander_fast_map::coupled_session::run(
            &serde_json::from_value(wire).unwrap(),
            base,
            &base.join("out"),
        )
    };
    // A valid descriptor progresses to payload I/O; each forgery refuses earlier.
    assert!(matches!(run(wire.clone()), Err(MapError::Io(_))));
    for (target, field, value) in [
        ("two_d", "grid_contract", serde_json::Value::Null),
        (
            "two_d",
            "geo_transform",
            serde_json::json!([612., 2., 0., 490., 0., -2.]),
        ),
        ("xs", "width", serde_json::json!(3)),
    ] {
        let mut bad = wire.clone();
        bad[target][field] = value;
        assert!(matches!(run(bad), Err(MapError::Invalid(_))));
    }
    let mut bad = wire.clone();
    bad["schema"] = serde_json::json!("fast-map-coupled-diagnostic/1");
    assert!(matches!(run(bad), Err(MapError::Unsupported(_))));
    let mut budget = wire;
    budget["max_working_bytes"] = serde_json::json!(1);
    assert!(matches!(run(budget), Err(MapError::Budget(_))));
}

#[test]
fn storage_coverage_suppresses_wet_xs_and_preserves_selected_bits() {
    use ras_commander_fast_map::coupled::{Storage, compose_with_storage_into};
    let nan = f32::from_bits(0x7fc01234);
    let xs = Products {
        depth: &[5., 7., 8., 9.],
        wse: &[100., 110., 120., 130.],
    };
    let mesh = Products {
        depth: &[3., nan, nan, nan],
        wse: &[95., nan, nan, nan],
    };
    let storage = Storage {
        owners: &[0, 1, 2, 0],
        area_count: 2,
        products: Products {
            depth: &[nan, nan, -0., nan],
            wse: &[nan, nan, f32::from_bits(0x42c80001), nan],
        },
    };
    let mut depth = [123.; 4];
    let mut wse = [456.; 4];
    let counts =
        compose_with_storage_into(&[1, 0, 0, 0], 1, xs, mesh, storage, &mut depth, &mut wse)
            .unwrap();
    assert_eq!(
        depth.map(f32::to_bits),
        [
            3_f32.to_bits(),
            nan.to_bits(),
            (-0_f32).to_bits(),
            9_f32.to_bits()
        ]
    );
    assert_eq!(
        wse.map(f32::to_bits),
        [
            95_f32.to_bits(),
            nan.to_bits(),
            0x42c80001,
            130_f32.to_bits()
        ]
    );
    assert_eq!(counts.base.valid_pixels, 3);
    assert_eq!(counts.base.two_d_owned_pixels, 1);
    assert_eq!(counts.storage_owned_pixels, 2);
    assert_eq!(counts.dry_storage_suppressed_wet_xs, 1);
}

#[test]
fn storage_validation_is_atomic_even_for_dry_overlap_and_late_bad_inputs() {
    use ras_commander_fast_map::coupled::{Storage, compose_with_storage_into};
    let nan = f32::NAN;
    let xs = Products {
        depth: &[5., 7.],
        wse: &[100., 110.],
    };
    let mesh = Products {
        depth: &[nan, nan],
        wse: &[nan, nan],
    };
    for (owners, storage_owners, area_count, sd, sw) in [
        ([0, 0], [1, 2], 1, [nan, nan], [nan, nan]),
        ([0, 1], [0, 1], 1, [nan, nan], [nan, nan]),
        ([0, 0], [0, 1], 0, [nan, nan], [nan, nan]),
        ([0, 0], [0, 1], 1, [nan, nan], [nan, 9.]),
        ([0, 0], [0, 0], 1, [nan, 3.], [nan, 9.]),
        ([0, 0], [0, 1], 1, [nan, -1.], [nan, 9.]),
        ([0, 0], [0, 1], 1, [nan, 3.], [nan, f32::INFINITY]),
        ([0, 2], [1, 0], 1, [1., nan], [9., nan]),
    ] {
        let storage = Storage {
            owners: &storage_owners,
            area_count,
            products: Products {
                depth: &sd,
                wse: &sw,
            },
        };
        let mut depth = [123.; 2];
        let mut wse = [456.; 2];
        assert!(
            compose_with_storage_into(&owners, 1, xs, mesh, storage, &mut depth, &mut wse).is_err()
        );
        assert_eq!(depth, [123.; 2]);
        assert_eq!(wse, [456.; 2]);
    }
}

#[test]
fn storage_transport_requires_explicit_schema_alignment_and_extra_budget_before_reads() {
    let component = serde_json::json!({"profile_id":"steady:Big","crs":"EPSG:3857", "width":3,"height":1,
        "geo_transform":[0.,1.,0.,1.,0.,-1.],"depth_file":"missing.bin","wse_file":"missing.bin"});
    let mut wire = serde_json::json!({"schema":"fast-map-coupled-diagnostic/2","xs":component,"two_d":component,
        "two_d_owners_file":"missing.bin","two_d_cell_count":1,"max_working_bytes":1_000_000,
        "storage":{"component":component,"owners_file":"missing.bin","area_count":1}});
    let base = std::path::Path::new("nonexistent-storage-input");
    let run = |v| {
        ras_commander_fast_map::coupled_session::run(
            &serde_json::from_value(v).unwrap(),
            base,
            &base.join("output"),
        )
    };
    for (field, value) in [
        ("profile_id", serde_json::json!("steady:Bigger")),
        ("crs", serde_json::json!("EPSG:2277")),
        ("height", serde_json::json!(2)),
        (
            "geo_transform",
            serde_json::json!([1., 1., 0., 1., 0., -1.]),
        ),
    ] {
        let mut bad = wire.clone();
        bad["storage"]["component"][field] = value;
        assert!(matches!(run(bad), Err(MapError::Invalid(_))));
    }
    let mut bad = wire.clone();
    bad["schema"] = serde_json::json!("fast-map-coupled-diagnostic/1");
    assert!(matches!(run(bad), Err(MapError::Unsupported(_))));
    let mut bad = wire.clone();
    bad.as_object_mut().unwrap().remove("storage");
    assert!(matches!(run(bad), Err(MapError::Unsupported(_))));
    // The old two-component allocation is insufficient for storage, even
    // though every payload path is missing. Admission must fail before I/O.
    wire["max_working_bytes"] = serde_json::json!(3 * 28 + 65536 + 8192 + 65536 + 32 * (10 + 9));
    assert!(matches!(run(wire), Err(MapError::Budget(_))));
}

#[test]
fn physical_coverage_wins_before_visibility_and_copies_bits() {
    let xs = Products {
        depth: &[5., 7., -0.],
        wse: &[100., 110., 90.],
    };
    let mesh = Products {
        depth: &[3., f32::NAN, f32::NAN],
        wse: &[95., f32::NAN, f32::NAN],
    };
    let mut d = [0.; 3];
    let mut w = [0.; 3];
    let counts = compose_into(&[1, 1, 0], 1, xs, mesh, &mut d, &mut w).unwrap();
    assert_eq!(d[0], 3.);
    assert_eq!(w[0], 95.);
    assert!(d[1].is_nan() && w[1].is_nan());
    assert_eq!(d[2].to_bits(), (-0_f32).to_bits());
    assert_eq!(w[2], 90.);
    assert_eq!(counts.valid_pixels, 2);
    assert_eq!(counts.two_d_owned_pixels, 2);
    assert_eq!(counts.dry_two_d_suppressed_wet_xs, 1);
}

#[test]
fn storage_transport_writes_versioned_report_and_exact_products() {
    use std::fs;
    let root = std::env::temp_dir().join(format!(
        "coupled-storage-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&root).unwrap();
    let nan = f32::from_bits(0x7fc01234);
    for (name, values) in [
        ("xs-depth", [5., 7., 9.]),
        ("xs-wse", [100., 110., 120.]),
        ("mesh-depth", [3., nan, nan]),
        ("mesh-wse", [95., nan, nan]),
        ("storage-depth", [nan, nan, -0.]),
        ("storage-wse", [nan, nan, 90.]),
    ] {
        fs::write(
            root.join(name),
            values
                .into_iter()
                .flat_map(f32::to_le_bytes)
                .collect::<Vec<_>>(),
        )
        .unwrap();
    }
    for (name, labels) in [
        ("mesh-owners", [1_u32, 0, 0]),
        ("storage-owners", [0, 1, 2]),
    ] {
        fs::write(
            root.join(name),
            labels
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect::<Vec<_>>(),
        )
        .unwrap();
    }
    let component = |name: &str| {
        serde_json::json!({"profile_id":"steady:Big", "crs":"prepared-crs",
        "width":3,"height":1,"geo_transform":[0.,1.,0.,1.,0.,-1.],
        "depth_file":format!("{name}-depth"),"wse_file":format!("{name}-wse")})
    };
    let request = serde_json::from_value(serde_json::json!({"schema":"fast-map-coupled-diagnostic/2",
        "xs":component("xs"),"two_d":component("mesh"),"two_d_owners_file":"mesh-owners","two_d_cell_count":1,
        "storage":{"component":component("storage"),"owners_file":"storage-owners","area_count":2},"max_working_bytes":1_000_000})).unwrap();
    let output = root.join("output");
    let report = ras_commander_fast_map::coupled_session::run(&request, &root, &output).unwrap();
    assert_eq!(report["schema"], "fast-map-coupled-diagnostic/2");
    assert_eq!(
        report["method"]["method_id"],
        ras_commander_fast_map::coupled::STORAGE_METHOD_ID
    );
    assert_eq!(report["method"]["native_parity_qualified"], false);
    assert_eq!(report["counts"]["storage_owned_pixels"], 2);
    assert_eq!(report["counts"]["dry_storage_suppressed_wet_xs"], 1);
    assert_eq!(report["counts"]["valid_pixels"], 2);
    for (name, expected) in [("depth.bin", [3., nan, -0.]), ("wse.bin", [95., nan, 90.])] {
        assert_eq!(
            fs::read(output.join(name)).unwrap(),
            expected
                .into_iter()
                .flat_map(f32::to_le_bytes)
                .collect::<Vec<_>>()
        );
    }
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&fs::read(output.join("report.json")).unwrap())
            .unwrap(),
        report
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn invalid_inputs_do_not_partially_mutate_outputs() {
    let xs = Products {
        depth: &[5., 7.],
        wse: &[100., 110.],
    };
    for (owners, md, mw) in [
        ([1, 2], [3., 4.], [95., 96.]),
        ([1, 1], [3., f32::NAN], [95., 96.]),
        ([1, 0], [3., 4.], [95., 96.]),
        ([1, 1], [3., f32::INFINITY], [95., 96.]),
        ([1, 1], [3., -1.], [95., 96.]),
    ] {
        let mut d = [123.; 2];
        let mut w = [456.; 2];
        assert!(
            compose_into(
                &owners,
                1,
                xs,
                Products {
                    depth: &md,
                    wse: &mw
                },
                &mut d,
                &mut w
            )
            .is_err()
        );
        assert_eq!(d, [123.; 2]);
        assert_eq!(w, [456.; 2]);
    }
}

#[test]
fn transport_rejects_budget_and_misaligned_profiles_before_reads() {
    let component = serde_json::json!({"profile_id":"t1","crs":"EPSG:3857", "width":3,"height":1,
        "geo_transform":[0.,1.,0.,1.,0.,-1.],"depth_file":"missing.bin","wse_file":"missing.bin"});
    let mut wire = serde_json::json!({"schema":"fast-map-coupled-diagnostic/1","xs":component,"two_d":component,
        "two_d_owners_file":"missing.bin","two_d_cell_count":1,"max_working_bytes":1});
    let base = std::path::Path::new("nonexistent-coupled-input");
    let run = |v| {
        ras_commander_fast_map::coupled_session::run(
            &serde_json::from_value(v).unwrap(),
            base,
            &base.join("output"),
        )
    };
    assert!(matches!(run(wire.clone()), Err(MapError::Budget(_))));
    wire["max_working_bytes"] = serde_json::json!(3 * 28 + 65536);
    assert!(matches!(run(wire.clone()), Err(MapError::Budget(_))));
    wire["max_working_bytes"] = serde_json::json!(1_000_000);
    for (field, size) in [("crs", 16385), ("profile_id", 257)] {
        let mut oversized = wire.clone();
        oversized["xs"][field] = serde_json::json!("a".repeat(size));
        oversized["two_d"][field] = oversized["xs"][field].clone();
        assert!(matches!(run(oversized), Err(MapError::Invalid(_))));
    }
    for (key, value) in [
        ("profile_id", serde_json::json!("t2")),
        ("crs", serde_json::json!("EPSG:2277")),
        ("width", serde_json::json!(4)),
        (
            "geo_transform",
            serde_json::json!([1., 1., 0., 1., 0., -1.]),
        ),
    ] {
        let mut wrong = wire.clone();
        wrong["two_d"][key] = value;
        assert!(matches!(run(wrong), Err(MapError::Invalid(_))));
    }
}

#[test]
#[ignore = "requires external coupled component request and independent native reference buffers"]
fn external_coupled_masks_and_bits() {
    use std::{fs, path::PathBuf, process::Command};
    let base = PathBuf::from(
        std::env::var_os("FAST_MAP_COUPLED_FIXTURE").expect("set FAST_MAP_COUPLED_FIXTURE"),
    );
    let output = std::env::temp_dir().join(format!(
        "fast-map-coupled-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let result = Command::new(env!("CARGO_BIN_EXE_ras2fim-batch"))
        .arg("coupled-diagnostic")
        .arg(base.join("request.json"))
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    for product in ["depth", "wse"] {
        let a = fs::read(output.join(format!("{product}.bin"))).unwrap();
        let b = fs::read(base.join(format!("expected-{product}.bin"))).unwrap();
        assert_eq!(a.len(), b.len());
        for (i, (a, b)) in a
            .as_chunks::<4>()
            .0
            .iter()
            .zip(b.as_chunks::<4>().0)
            .enumerate()
        {
            let (a, b) = (f32::from_le_bytes(*a), f32::from_le_bytes(*b));
            assert_eq!(a.is_nan(), b.is_nan(), "{product} mask at {i}");
            if !a.is_nan() {
                assert_eq!(a.to_bits(), b.to_bits(), "{product} bits at {i}");
            }
        }
    }
    fs::remove_dir_all(output).unwrap();
}

#[test]
fn diagnostic_cli_bounds_json_before_deserialization() {
    use std::{fs, process::Command};
    let root = std::env::temp_dir().join(format!(
        "coupled-json-cap-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&root).unwrap();
    let path = root.join("oversized.json");
    fs::write(&path, vec![b' '; 65537]).unwrap();
    for command in ["one-d-diagnostic", "coupled-diagnostic"] {
        let result = Command::new(env!("CARGO_BIN_EXE_ras2fim-batch"))
            .arg(command)
            .arg(&path)
            .arg(root.join("out"))
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains("65536 bytes"));
        assert!(!root.join("out").exists());
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn storage_only_composition_requires_empty_dry_mesh_and_preserves_legacy_refusal() {
    use ras_commander_fast_map::coupled::{Storage, compose_with_storage_into};
    let nan = f32::NAN;
    let xs = Products {
        depth: &[2., 3., 4.],
        wse: &[12., 13., 14.],
    };
    let mesh = Products {
        depth: &[nan, nan, nan],
        wse: &[nan, nan, nan],
    };
    let storage = Storage {
        owners: &[1, 1, 0],
        area_count: 1,
        products: Products {
            depth: &[1., nan, nan],
            wse: &[11., nan, nan],
        },
    };
    let mut d = [123.; 3];
    let mut w = [456.; 3];
    assert!(compose_into(&[0, 0, 0], 0, xs, mesh, &mut d, &mut w).is_err());
    assert_eq!(d, [123.; 3]);
    let counts =
        compose_with_storage_into(&[0, 0, 0], 0, xs, mesh, storage, &mut d, &mut w).unwrap();
    assert_eq!(counts.base.two_d_owned_pixels, 0);
    assert_eq!(counts.base.valid_pixels, 2);
    assert_eq!(counts.dry_storage_suppressed_wet_xs, 1);
    assert_eq!(d[0].to_bits(), 1_f32.to_bits());
    assert!(d[1].is_nan());
    assert_eq!(d[2], 4.);
    d.fill(123.);
    w.fill(456.);
    for bad in [
        Products {
            depth: &[1., nan, nan],
            wse: &[11., nan, nan],
        },
        Products {
            depth: &[nan, nan, 1.],
            wse: &[nan, nan, 11.],
        },
    ] {
        assert!(
            compose_with_storage_into(&[0, 0, 0], 0, xs, bad, storage, &mut d, &mut w).is_err()
        );
        assert_eq!(d, [123.; 3]);
        assert_eq!(w, [456.; 3]);
    }
    assert!(compose_with_storage_into(&[0, 0, 1], 0, xs, mesh, storage, &mut d, &mut w).is_err());
    assert_eq!(d, [123.; 3]);
    assert_eq!(w, [456.; 3]);
}

#[test]
fn storage_composition_final_format_writes_composed_bits() {
    use std::fs;
    let root = std::env::temp_dir().join(format!(
        "coupled-final-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&root).unwrap();
    let nan = f32::NAN;
    for (name, values) in [
        ("xs-depth", [5., 7., 9.]),
        ("xs-wse", [100., 110., 120.]),
        ("mesh-depth", [3., nan, nan]),
        ("mesh-wse", [95., nan, nan]),
        ("storage-depth", [nan, nan, 0.5]),
        ("storage-wse", [nan, nan, 90.]),
    ] {
        fs::write(
            root.join(name),
            values
                .into_iter()
                .flat_map(f32::to_le_bytes)
                .collect::<Vec<_>>(),
        )
        .unwrap();
    }
    for (name, labels) in [
        ("mesh-owners", [1_u32, 0, 0]),
        ("storage-owners", [0, 1, 2]),
    ] {
        fs::write(
            root.join(name),
            labels
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect::<Vec<_>>(),
        )
        .unwrap();
    }
    fs::write(root.join("mask"), [1_u8, 1, 0]).unwrap();
    let component = |name: &str| {
        serde_json::json!({"profile_id":"steady:Big", "crs":"prepared-crs",
        "width":3,"height":1,"geo_transform":[500000.,1.,0.,4000000.,0.,-1.],
        "depth_file":format!("{name}-depth"),"wse_file":format!("{name}-wse")})
    };
    let wire = |extra: serde_json::Value| {
        let mut r = serde_json::json!({"schema":"fast-map-coupled-diagnostic/2",
            "xs":component("xs"),"two_d":component("mesh"),"two_d_owners_file":"mesh-owners","two_d_cell_count":1,
            "storage":{"component":component("storage"),"owners_file":"storage-owners","area_count":2},"max_working_bytes":4_000_000});
        for (k, v) in extra.as_object().unwrap() {
            r[k] = v.clone();
        }
        serde_json::from_value::<ras_commander_fast_map::coupled_session::Request>(r).unwrap()
    };
    let raw = ras_commander_fast_map::coupled_session::run(
        &wire(serde_json::json!({})),
        &root,
        &root.join("raw"),
    )
    .unwrap();
    let report = ras_commander_fast_map::coupled_session::run(
        &wire(
            serde_json::json!({"final_format":{"nodata":-9999.0,"block_size":16},"epsg":26915,
            "aoi_mask_file":"mask","flooded_depth_range":[0.0,10000.0],"threads":2}),
        ),
        &root,
        &root.join("final"),
    )
    .unwrap();
    assert_eq!(report["counts"], raw["counts"]);
    assert_eq!(report["depth_finite_pixels"], 1);
    assert_eq!(report["flooded_pixels"], 1);
    for product in ["depth", "wse"] {
        let bits: Vec<f32> = fs::read(root.join(format!("raw/{product}.bin")))
            .unwrap()
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect();
        let mut raster = terrio::LazyRaster::open_tiff(
            root.join(format!("final/{product}.tif")).to_str().unwrap(),
        )
        .unwrap()
        .unwrap();
        let ds = raster
            .read_window(
                terrio::Window {
                    x: 0,
                    y: 0,
                    width: 3,
                    height: 1,
                },
                &terrio::WindowOptions {
                    bands: vec![1],
                    overview: None,
                },
            )
            .unwrap();
        let terrio::raster::RasterData::F32(values) = ds.raster.data else {
            panic!()
        };
        assert_eq!(values[0].to_bits(), bits[0].to_bits());
        assert_eq!(values[1], -9999.0); // composed NaN
        assert_eq!(values[2], -9999.0); // masked storage pixel
    }
    // threads without final_format and stored-grid final_format are refused.
    assert!(
        ras_commander_fast_map::coupled_session::run(
            &wire(serde_json::json!({"threads":2})),
            &root,
            &root.join("bad")
        )
        .is_err()
    );
    assert!(!root.join("bad").exists());
    fs::remove_dir_all(root).unwrap();
}
