//! Any CRS: requests in a CRS without an EPSG code carry its GeoKey tags
//! (`crs_geokeys`), which the engine writes back verbatim. Synthetic
//! ESRI:102630 (NAD83 / Alabama West, US survey feet) keys, as GDAL and RAS
//! Mapper write them; EPSG requests keep their output.
use ras_commander_fast_map::{
    crs::{GeoKeys, OutputCrs, read_geokeys},
    effective_terrain::{self, Effective, Grid},
    engine_method_record, one_d_delivery, sloping_session, terrain,
};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn scratch() -> Scratch {
    let dir = std::env::temp_dir().join(format!(
        "fast-map-crs-any-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&dir).unwrap();
    Scratch(dir)
}

/// GDAL's GeoKeys for ESRI:102630 (user-defined Transverse Mercator on NAD83,
/// US survey foot), as in RAS Mapper's terrain export.
fn alabama_west() -> GeoKeys {
    serde_json::from_value(json!({
        "directory": [1, 1, 0, 18, 1024, 0, 1, 1, 1025, 0, 1, 1, 1026, 34737, 48, 0,
            2048, 0, 1, 4269, 2049, 34737, 6, 48, 2054, 0, 1, 9102, 2057, 34736, 1, 6,
            2059, 34736, 1, 5, 3059, 0, 1, 1, 3072, 0, 1, 32767, 3074, 0, 1, 32767,
            3075, 0, 1, 1, 3076, 0, 1, 9003, 3080, 34736, 1, 1, 3081, 34736, 1, 0,
            3082, 34736, 1, 3, 3083, 34736, 1, 4, 3092, 34736, 1, 2],
        "doubles": [30.0, -87.5, 0.999933333333333, 1968500.0, 0.0, 298.257222101, 6378137.0],
        "ascii": "NAD_1983_StatePlane_Alabama_West_FIPS_0102_Feet|NAD83|"
    }))
    .unwrap()
}

fn keys_json() -> Value {
    serde_json::to_value(alabama_west()).unwrap()
}

fn effective(grid: Grid, values: Vec<f32>) -> Effective {
    Effective {
        grid,
        values,
        report: Value::Null,
    }
}

/// A terrain TIFF with no CRS at all, as RAS Mapper writes the sources of a
/// terrain whose CRS has no EPSG code.
fn write_untagged(path: &Path, grid: Grid, values: Vec<f32>) {
    use wbraster::{DataType, Raster, crs_info::CrsInfo, raster::RasterData};
    let t = grid.transform();
    let raster = Raster {
        cols: grid.width,
        rows: grid.height,
        bands: 1,
        x_min: t[0],
        y_min: t[3] + t[5] * grid.height as f64,
        cell_size_x: t[1],
        cell_size_y: -t[5],
        nodata: -9999.0,
        data_type: DataType::F32,
        crs: CrsInfo::default(),
        metadata: Vec::new(),
        data: RasterData::F32(values),
    };
    let dataset =
        terrio::RasterDataset::with_driver(raster, terrio::formats::RasterDriver::GTiff, true);
    terrio::write_raster_dataset(
        &dataset,
        path.to_str().unwrap(),
        &terrio::RasterWriteOptions {
            geo_transform: Some(t),
            creation_options: vec![
                ("TILED".into(), "YES".into()),
                ("BLOCKXSIZE".into(), "256".into()),
                ("BLOCKYSIZE".into(), "256".into()),
            ],
            ..Default::default()
        },
    )
    .unwrap();
}

/// Every pixel's bits and the georeferencing, read back with terrio.
fn pixels(path: &Path) -> (Vec<u32>, [f64; 6], Option<u64>) {
    let mut r = terrio::LazyRaster::open_tiff(path.to_str().unwrap())
        .unwrap()
        .unwrap();
    let w = terrio::Window::new(0, 0, r.width(), r.height());
    let ds = r
        .read_window(
            w,
            &terrio::WindowOptions {
                bands: vec![1],
                overview: None,
            },
        )
        .unwrap();
    let wbraster::raster::RasterData::F32(v) = ds.raster.data else {
        panic!("not Float32")
    };
    (
        v.iter().map(|x| x.to_bits()).collect(),
        r.geo_transform(),
        r.nodata().map(f64::to_bits),
    )
}

/// The ESRI:102630 keys decode, through terrio, to Transverse Mercator on
/// NAD83 in US survey feet.
fn assert_alabama_west(path: &Path) {
    assert_eq!(read_geokeys(path).unwrap(), Some(alabama_west()));
    let info = terrain::inspect(path.to_str().unwrap()).unwrap();
    assert_eq!(info.epsg, None);
    let wkt = info.crs.wkt.clone().unwrap();
    for part in [
        "Transverse_Mercator",
        "US survey foot",
        "\"central_meridian\",-87.5",
        "\"false_easting\",1968500",
        "North_American_Datum_1983",
    ] {
        assert!(wkt.contains(part), "{part} missing from {wkt}");
    }
    assert_eq!(info.geokeys_fnv1a64, Some(alabama_west().fnv1a64()));
}

fn grid(width: usize, height: usize) -> Grid {
    Grid {
        origin_x: 2_149_210.0,
        origin_y: 1_203_339.0,
        cell_size: 3.25,
        width,
        height,
    }
}

fn plane(g: &Grid) -> Vec<f32> {
    (0..g.width * g.height)
        .map(|i| {
            effective_terrain::quantize(
                400.0 + (i % g.width) as f64 * 0.5 + (i / g.width) as f64 * 0.25,
            )
        })
        .collect()
}

#[test]
fn geokeys_are_written_verbatim_and_epsg_keys_are_unchanged() {
    let dir = scratch();
    let g = grid(300, 280);
    let keys = OutputCrs::GeoKeys(alabama_west().into());
    effective_terrain::write_geotiff_crs(&dir.0.join("k.tif"), effective(g, plane(&g)), &keys)
        .unwrap();
    effective_terrain::write_geotiff(&dir.0.join("e.tif"), effective(g, plane(&g)), 2277).unwrap();
    assert_alabama_west(&dir.0.join("k.tif"));
    assert_eq!(
        read_geokeys(&dir.0.join("e.tif")).unwrap(),
        Some(GeoKeys::from_epsg(2277, false))
    );
    // Same pixels, transform and nodata; only the CRS tags differ.
    assert_eq!(pixels(&dir.0.join("k.tif")), pixels(&dir.0.join("e.tif")));
    // terrain-info prints the keys a request copies.
    let info =
        serde_json::to_value(terrain::inspect(dir.0.join("k.tif").to_str().unwrap()).unwrap())
            .unwrap();
    assert_eq!(info["geokeys"], keys_json());
}

fn mask(dir: &Path, name: &str, w: usize, h: usize) -> Value {
    let (tx, ty) = (w.div_ceil(256), h.div_ceil(256));
    let mut m = vec![0_u8; tx * ty * 65536];
    for r in 0..h {
        for c in 0..w {
            m[((r / 256) * tx + c / 256) * 65536 + (r % 256) * 256 + c % 256] = 1;
        }
    }
    fs::write(dir.join(name), m).unwrap();
    json!({"path": name, "dtype": "|u1", "shape": [tx * ty, 65536]})
}

fn effective_request(source: &str, mask: Value, crs: Value) -> effective_terrain::Request {
    let mut v = json!({
        "schema": "ras-commander-effective-terrain-request/2",
        "sources": [{"path": source, "mask": mask}],
        "modifications": 0, "threads": 2, "max_working_bytes": 1u64 << 28,
    });
    for (k, x) in crs.as_object().unwrap() {
        v[k] = x.clone();
    }
    serde_json::from_value(v).unwrap()
}

#[test]
fn effective_terrain_round_trips_untagged_and_tagged_sources() {
    let dir = scratch();
    let g = grid(300, 280);
    let m = mask(&dir.0, "m.u1", g.width, g.height);
    write_untagged(&dir.0.join("untagged.tif"), g, plane(&g));
    effective_terrain::write_geotiff_crs(
        &dir.0.join("tagged.tif"),
        effective(g, plane(&g)),
        &OutputCrs::GeoKeys(alabama_west().into()),
    )
    .unwrap();
    effective_terrain::write_geotiff(&dir.0.join("epsg.tif"), effective(g, plane(&g)), 2277)
        .unwrap();
    let run = |source: &str, crs: Value, out: &str| {
        effective_terrain::run(
            &effective_request(source, m.clone(), crs),
            &dir.0,
            &dir.0.join(out),
        )
    };
    let epsg = run(
        "epsg.tif",
        json!({"epsg": 2277, "crs_identity": {"kind": "epsg"}}),
        "epsg",
    )
    .unwrap();
    assert!(epsg.get("output_crs").is_none());
    let untagged = run(
        "untagged.tif",
        json!({"crs_geokeys": keys_json(), "crs_identity": {"kind": "untagged"}}),
        "untagged",
    )
    .unwrap();
    assert_eq!(untagged["output_crs"]["kind"], "geokeys");
    let tagged = run(
        "tagged.tif",
        json!({"epsg": 0, "crs_geokeys": keys_json(), "crs_identity": {"kind": "geokeys"}}),
        "tagged",
    )
    .unwrap();
    assert_eq!(tagged["status"], "complete");
    let out = |name: &str| dir.0.join(name).join("effective_terrain.tif");
    assert_alabama_west(&out("untagged"));
    assert_alabama_west(&out("tagged"));
    assert_eq!(
        read_geokeys(&out("epsg")).unwrap(),
        Some(GeoKeys::from_epsg(2277, false))
    );
    // The effective terrain is the same grid whatever the label.
    assert_eq!(pixels(&out("untagged")), pixels(&out("epsg")));
    assert_eq!(pixels(&out("tagged")), pixels(&out("epsg")));
}

#[test]
fn crs_refusals_happen_before_outputs() {
    let dir = scratch();
    let g = grid(20, 20);
    let m = mask(&dir.0, "m.u1", g.width, g.height);
    write_untagged(&dir.0.join("untagged.tif"), g, plane(&g));
    effective_terrain::write_geotiff_crs(
        &dir.0.join("tagged.tif"),
        effective(g, plane(&g)),
        &OutputCrs::GeoKeys(alabama_west().into()),
    )
    .unwrap();
    let refused = |source: &str, crs: Value| {
        let out = dir
            .0
            .join(format!("o{}", NEXT.fetch_add(1, Ordering::Relaxed)));
        let error =
            effective_terrain::run(&effective_request(source, m.clone(), crs), &dir.0, &out)
                .unwrap_err();
        assert!(!out.join("effective_terrain.tif").exists());
        error.to_string()
    };
    let mut other = alabama_west();
    other.doubles[3] = 1_968_500.5;
    let other = serde_json::to_value(other).unwrap();
    // A label that is not the tagged source's own keys.
    assert!(
        refused(
            "tagged.tif",
            json!({"crs_geokeys": other, "crs_identity": {"kind": "geokeys"}})
        )
        .contains("differ from crs_geokeys")
    );
    // geokeys identity on a source without keys; untagged on a tagged source.
    assert!(
        refused(
            "untagged.tif",
            json!({"crs_geokeys": keys_json(), "crs_identity": {"kind": "geokeys"}})
        )
        .contains("(none)")
    );
    assert!(
        refused(
            "tagged.tif",
            json!({"crs_geokeys": keys_json(), "crs_identity": {"kind": "untagged"}})
        )
        .contains("carries a CRS")
    );
    // No EPSG identity without a code; not both; no code-less label; the new kinds need keys.
    refused(
        "tagged.tif",
        json!({"crs_geokeys": keys_json(), "crs_identity": {"kind": "epsg"}}),
    );
    assert!(
        refused(
            "tagged.tif",
            json!({"epsg": 2277, "crs_geokeys": keys_json(), "crs_identity": {"kind": "geokeys"}})
        )
        .contains("not both")
    );
    assert!(
        refused(
            "untagged.tif",
            json!({"epsg": 0, "crs_identity": {"kind": "epsg"}})
        )
        .contains("crs_geokeys")
    );
    refused(
        "untagged.tif",
        json!({"epsg": 2277, "crs_identity": {"kind": "untagged"}}),
    );
    // PixelIsPoint keys are refused.
    let mut point = alabama_west();
    point.directory[11] = 2;
    assert!(
        refused(
            "tagged.tif",
            json!({"crs_geokeys": point, "crs_identity": {"kind": "geokeys"}})
        )
        .contains("PixelIsArea")
    );
}

fn table(dir: &Path, name: &str, bytes: Vec<u8>, rows: usize) -> Value {
    fs::write(dir.join(format!("{name}.bin")), bytes).unwrap();
    json!({"path": format!("{name}.bin"), "rows": rows})
}
fn le<T: Copy, const N: usize>(v: &[T], f: fn(T) -> [u8; N]) -> Vec<u8> {
    v.iter().flat_map(|x| f(*x)).collect()
}

/// One square 2D cell over a 4 x 4 terrain (as in `physical_ownership`).
fn sloping_model(dir: &Path, keys: bool) -> Value {
    let g = Grid {
        origin_x: 0.0,
        origin_y: 4.0,
        cell_size: 1.0,
        width: 4,
        height: 4,
    };
    let values: Vec<f32> = (0..16)
        .map(|i| {
            if i == 0 {
                f32::NAN
            } else {
                (i % 3) as f32 * 0.125
            }
        })
        .collect();
    let crs = if keys {
        OutputCrs::GeoKeys(alabama_west().into())
    } else {
        OutputCrs::Epsg(2277)
    };
    effective_terrain::write_geotiff_crs(&dir.join("terrain.tif"), effective(g, values), &crs)
        .unwrap();
    let arrays = json!({
        "centers": table(dir, "centers", le(&[2.,2.,-1.,2.,2.,5.,5.,2.,2.,-1.], f64::to_le_bytes), 5),
        "cell_info": table(dir, "cell_info", le(&[0,4,4,1,5,1,6,1,7,1], i32::to_le_bytes), 5),
        "cell_faces": table(dir, "cell_faces", le(&[0,1,1,1,2,1,3,1,0,-1,1,-1,2,-1,3,-1], i32::to_le_bytes), 8),
        "cell_minimum": table(dir, "cell_minimum", le(&[0.0f32; 5], f32::to_le_bytes), 5),
        "face_vertices": table(dir, "face_vertices", le(&[0,1,1,2,2,3,3,0], i32::to_le_bytes), 4),
        "face_cells": table(dir, "face_cells", le(&[0,1,0,2,0,3,0,4], i32::to_le_bytes), 4),
        "face_info": table(dir, "face_info", le(&[0i32; 8], i32::to_le_bytes), 4),
        "face_points": table(dir, "face_points", Vec::new(), 0),
        "face_minimum": table(dir, "face_minimum", le(&[0.0f32; 4], f32::to_le_bytes), 4),
        "vertices": table(dir, "vertices", le(&[0.,0.,0.,4.,4.,4.,4.,0.], f64::to_le_bytes), 4),
        "vertex_info": table(dir, "vertex_info", le(&[0,2,2,2,4,2,6,2], i32::to_le_bytes), 4),
        "vertex_faces": table(dir, "vertex_faces", le(&[3,1,0,1,0,1,1,1,1,1,2,1,2,1,3,1], i32::to_le_bytes), 8),
    });
    fs::write(
        dir.join("mesh.json"),
        json!({"schema": "ras-commander-sloping-mesh/1", "arrays": arrays}).to_string(),
    )
    .unwrap();
    let mut wkb = vec![1];
    for i in [3u32, 1, 5] {
        wkb.extend(i.to_le_bytes());
    }
    for p in [[0., 0.], [0., 4.], [3., 4.], [3., 0.], [0., 0.]] {
        wkb.extend(le(&p, f64::to_le_bytes));
    }
    let mut bytes = (wkb.len() as u32).to_le_bytes().to_vec();
    bytes.extend(wkb);
    fs::write(dir.join("cell.wkb"), bytes).unwrap();
    for (name, value) in [("a", 0.5f32), ("b", 1.25)] {
        fs::write(
            dir.join(format!("{name}.bin")),
            le(&[value, 0., 0., 0., 0.], f32::to_le_bytes),
        )
        .unwrap();
    }
    let mut model = json!({"schema": sloping_session::SCHEMA, "terrain": "terrain.tif",
        "terrain_transform": [0., 1., 0., 4., 0., -1.], "window": [0, 0, 4, 4],
        "geometry_file": "cell.wkb", "mesh_files": ["mesh.json"], "owner_cells": [[0, 0]],
        "profiles": [{"id": "a", "values_files": ["a.bin"]}, {"id": "b", "values_files": ["b.bin"]}],
        "threads": 2, "max_working_bytes": 256u64 << 20, "window_cache_bytes": 0,
        "stream_outputs": true, "geotiff_outputs": true});
    if keys {
        model["crs_geokeys"] = keys_json();
        model["crs_identity"] = json!({"kind": "geokeys"});
    } else {
        model["epsg"] = 2277.into();
        model["crs_identity"] = json!({"kind": "epsg"});
    }
    model
}

#[test]
fn sloping_depth_and_wse_carry_the_terrain_keys_in_every_writer() {
    let modes = [
        json!({}),
        json!({"row_stream_rows": 1}),
        json!({"compression": "DEFLATE", "final_format": {"nodata": -9999.0, "block_size": 16,
            "overview_factors": [2]}}),
        json!({"compression": "DEFLATE", "row_stream_rows": 2,
            "final_format": {"nodata": -9999.0, "block_size": 16}}),
    ];
    for (i, mode) in modes.iter().enumerate() {
        let (k, e) = (scratch(), scratch());
        let run = |dir: &Path, keys: bool| {
            let mut model = sloping_model(dir, keys);
            for (name, value) in mode.as_object().unwrap() {
                model[name] = value.clone();
            }
            let (req, output) =
                ras_commander_fast_map::native_output::parse_request(model).unwrap();
            sloping_session::run_with_output_options(
                &req,
                dir,
                &dir.join("out"),
                None,
                output.as_ref(),
            )
            .unwrap()
        };
        run(&k.0, true);
        run(&e.0, false);
        for profile in ["a", "b"] {
            for product in ["depth", "wse"] {
                let name = format!("{product}.tif");
                let (kp, ep) = (
                    k.0.join("out").join(profile).join(&name),
                    e.0.join("out").join(profile).join(&name),
                );
                assert_alabama_west(&kp);
                assert_eq!(
                    read_geokeys(&ep).unwrap(),
                    Some(GeoKeys::from_epsg(2277, false)),
                    "mode {i}"
                );
                assert_eq!(pixels(&kp), pixels(&ep), "mode {i} {profile} {product}");
            }
        }
    }
}

#[test]
fn sloping_refuses_a_label_other_than_the_terrain_keys() {
    let dir = scratch();
    let mut model = sloping_model(&dir.0, true);
    model["crs_geokeys"]["doubles"][2] = json!(0.9999);
    let req: sloping_session::Request = serde_json::from_value(model).unwrap();
    let error = sloping_session::run(&req, &dir.0, &dir.0.join("out")).unwrap_err();
    assert!(
        error.to_string().contains("differ from crs_geokeys"),
        "{error}"
    );
    let mut model = sloping_model(&dir.0, true);
    model["crs_identity"] = json!({"kind": "epsg"});
    let req: sloping_session::Request = serde_json::from_value(model).unwrap();
    assert!(sloping_session::run(&req, &dir.0, &dir.0.join("out2")).is_err());
}

#[test]
fn one_d_final_delivery_writes_keys() {
    let dir = scratch();
    let options: one_d_delivery::Options = serde_json::from_value(json!({
        "final_format": {"nodata": -9999.0, "block_size": 16},
        "crs_geokeys": keys_json(), "flooded_depth_range": [0.0, 1e30]
    }))
    .unwrap();
    let plan = options
        .plan(20, 18, [2_149_210.0, 3.25, 0.0, 1_203_339.0, 0.0, -3.25])
        .unwrap()
        .unwrap();
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap();
    let mut depth: Vec<f32> = (0..360)
        .map(|i| {
            if i % 7 == 0 {
                f32::NAN
            } else {
                i as f32 * 0.01
            }
        })
        .collect();
    let mut wse = depth.iter().map(|d| d + 400.0).collect::<Vec<_>>();
    let report = plan
        .deliver(&dir.0, None, &mut depth, &mut wse, &pool)
        .unwrap();
    assert_eq!(report["epsg"], Value::Null);
    assert_eq!(report["output_crs"]["fnv1a64"], alabama_west().fnv1a64());
    assert_alabama_west(&dir.0.join("depth.tif"));
    assert_alabama_west(&dir.0.join("wse.tif"));
    // Not both, and the keys need final_format.
    let both: one_d_delivery::Options = serde_json::from_value(json!({
        "final_format": {"nodata": -9999.0}, "epsg": 2277,
        "crs_geokeys": keys_json(), "flooded_depth_range": [0.0, 1.0]
    }))
    .unwrap();
    assert!(both.plan(4, 4, [0., 1., 0., 4., 0., -1.]).is_err());
    let alone: one_d_delivery::Options =
        serde_json::from_value(json!({"crs_geokeys": keys_json()})).unwrap();
    assert!(alone.plan(4, 4, [0., 1., 0., 4., 0., -1.]).is_err());
}

#[test]
fn capability_is_advertised_only_where_supported() {
    let record = engine_method_record();
    for method in [
        "effective_terrain",
        "sloping",
        "sloping_native_grid",
        "horizontal",
    ] {
        assert_eq!(record[method]["crs_any"], true, "{method}");
        assert!(
            record[method]["crs_identity_kinds"]
                .as_array()
                .unwrap()
                .contains(&json!("untagged")),
            "{method}"
        );
    }
    for method in [
        "step05_r2",
        "depth_groups",
        "boundary",
        "native_boundary",
        "batch_session_r1",
        "whole_simulation",
        "storage",
    ] {
        assert!(record[method].get("crs_any").is_none(), "{method}");
    }
}
