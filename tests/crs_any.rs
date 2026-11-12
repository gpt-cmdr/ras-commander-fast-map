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
        "doubles": ["30.0", "-87.5", "0.999933333333333", "1968500.0", "0.0", "298.257222101",
            "6378137.0"],
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
    point.directory[11] = 3;
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
        let report = run(&k.0, true);
        assert_eq!(report["output_crs"]["fnv1a64"], alabama_west().fnv1a64());
        assert!(run(&e.0, false).get("output_crs").is_none());
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
    model["crs_geokeys"]["doubles"][2] = json!("0.9999");
    let req: sloping_session::Request = serde_json::from_value(model).unwrap();
    let error = sloping_session::run(&req, &dir.0, &dir.0.join("out")).unwrap_err();
    assert!(
        error.to_string().contains("differ from crs_geokeys"),
        "{error}"
    );
    // Doubles as JSON numbers are refused: they would not parse exactly.
    let mut model = sloping_model(&dir.0, true);
    model["crs_geokeys"]["doubles"][2] = json!(0.999933333333333);
    let error = serde_json::from_value::<sloping_session::Request>(model)
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("must be decimal strings"), "{error}");

    let mut model = sloping_model(&dir.0, true);
    model["crs_identity"] = json!({"kind": "epsg"});
    let req: sloping_session::Request = serde_json::from_value(model).unwrap();
    assert!(sloping_session::run(&req, &dir.0, &dir.0.join("out2")).is_err());
}

#[test]
fn doubles_round_trip_exactly_as_strings() {
    // serde_json's default number parser reads each of these one unit in the
    // last place off; the string form is exact.
    let values = [
        0.30480060960121924,
        1.0715660391465826e-75,
        298.2572221008827,
        0.1 + 1_234_567.0 * 1.234_567_890_123e-7,
    ];
    let mut keys = alabama_west();
    keys.directory[31] = 0;
    keys.doubles = values.to_vec();
    keys.directory.splice(32..36, [2059, 34736, 1, 3]);
    let text = serde_json::to_string(&keys).unwrap();
    let back: GeoKeys = serde_json::from_str(&text).unwrap();
    let value: GeoKeys =
        serde_json::from_value(serde_json::from_str::<Value>(&text).unwrap()).unwrap();
    for parsed in [back, value] {
        assert_eq!(
            parsed
                .doubles
                .iter()
                .map(|v| v.to_bits())
                .collect::<Vec<_>>(),
            values.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
        );
    }
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
fn capability_is_advertised_on_every_method() {
    let record = engine_method_record();
    for method in [
        "effective_terrain",
        "sloping",
        "sloping_native_grid",
        "horizontal",
        "depth_groups",
        "boundary",
        "native_boundary",
        "batch_session_r1",
        "step05_r2",
        "whole_simulation",
        "storage",
        "coupled_diagnostic",
        "coupled_storage_diagnostic",
        "one_d_clipped_diagnostic",
    ] {
        assert_eq!(record[method]["crs_any"], true, "{method}");
    }
    assert_eq!(
        record["native_boundary"]["output_crs_kinds"],
        json!(["epsg", "wkt"])
    );
    assert_eq!(record["step05_r2"]["dst_crs_kinds"], json!(["epsg", "wkt"]));
}

const ESRI_WKT: &str = "PROJCS[\"NAD_1983_StatePlane_Alabama_West_FIPS_0102_Feet\",GEOGCS[\"NAD83\",DATUM[\"North_American_Datum_1983\",SPHEROID[\"GRS 1980\",6378137,298.257222101]],PRIMEM[\"Greenwich\",0],UNIT[\"degree\",0.0174532925199433]],PROJECTION[\"Transverse_Mercator\"],PARAMETER[\"latitude_of_origin\",30],PARAMETER[\"central_meridian\",-87.5],PARAMETER[\"scale_factor\",0.999933333333333],PARAMETER[\"false_easting\",1968500],PARAMETER[\"false_northing\",0],UNIT[\"US survey foot\",0.304800609601219],AXIS[\"Easting\",EAST],AXIS[\"Northing\",NORTH],AUTHORITY[\"ESRI\",\"102630\"]]";

/// A storage-only boundary request over a 4 x 4 terrain (as in `boundary_session`).
fn boundary_request(dir: &Path, keys: bool) -> Value {
    let g = Grid {
        origin_x: 0.,
        origin_y: 4.,
        cell_size: 1.,
        width: 4,
        height: 4,
    };
    let crs = if keys {
        OutputCrs::GeoKeys(alabama_west().into())
    } else {
        OutputCrs::Epsg(2227)
    };
    effective_terrain::write_geotiff_crs(
        &dir.join("terrain.tif"),
        effective(g, vec![0.; 16]),
        &crs,
    )
    .unwrap();
    fs::write(dir.join("mask"), vec![1u8; 65536]).unwrap();
    fs::write(dir.join("perimeter"), (-9999f32).to_le_bytes().repeat(513)).unwrap();
    fs::write(dir.join("levels"), 1f32.to_le_bytes()).unwrap();
    let mut polygon = vec![1u8];
    for n in [3u32, 1, 5] {
        polygon.extend(n.to_le_bytes());
    }
    for p in [[0.0f64, 0.0f64], [0., 4.], [4., 4.], [4., 0.], [0., 0.]] {
        polygon.extend(le(&p, f64::to_le_bytes));
    }
    let mut geometry = (polygon.len() as u32).to_le_bytes().to_vec();
    geometry.extend(polygon);
    fs::write(dir.join("storage.wkb"), geometry).unwrap();
    let mut v = json!({"schema":"ras-commander-boundary-request/2", "interpolation":"sloping",
        "contour_method":"rasmapper-serial-zero-depth-contours/1",
        "model":{"schema":"ras-commander-sloping-request/1", "terrain":"terrain.tif",
            "terrain_transform":[0,1,0,4,0,-1], "window":[0,0,4,4],
            "geometry_file":"unused", "mesh_files":[], "owner_cells":[], "storage_areas_file":"storage.wkb",
            "storage_area_count":1, "profiles":[{"id":"a", "values_files":[], "storage_area_values_file":"levels"}],
            "threads":1, "max_inflight_profiles":1, "max_working_bytes":134217728},
        "terrain_metadata":{"shape":[4,4], "transform":[0,1,0,4,0,-1], "tile_shape":[256,256],
            "tile_grid":[1,1], "selected_tiles":[0], "mask":{"path":"mask", "dtype":"|u1", "shape":[1,65536]},
            "perimeter":{"path":"perimeter", "dtype":"<f4", "shape":[1,513]}}});
    if keys {
        v["model"]["crs_geokeys"] = keys_json();
        v["model"]["crs_identity"] = json!({"kind": "geokeys"});
        v["crs_wkt"] = ESRI_WKT.into();
    } else {
        v["model"]["epsg"] = 2227.into();
        v["model"]["crs_identity"] = json!({"kind": "epsg"});
    }
    v
}

#[test]
fn boundary_geojson_names_the_request_wkt_and_keeps_epsg() {
    use ras_commander_fast_map::boundary_session;
    let (k, e) = (scratch(), scratch());
    let run = |dir: &Path, keys: bool| {
        let req: boundary_session::Request =
            serde_json::from_value(boundary_request(dir, keys)).unwrap();
        boundary_session::run(&req, dir, &dir.join("out")).unwrap();
        serde_json::from_slice::<Value>(&fs::read(dir.join("out").join("a.geojson")).unwrap())
            .unwrap()
    };
    let (kp, ep) = (run(&k.0, true), run(&e.0, false));
    assert_eq!(kp["crs"]["properties"]["name"], ESRI_WKT);
    assert_eq!(ep["crs"]["properties"]["name"], "EPSG:2227");
    assert_eq!(kp["features"], ep["features"]);
    // crs_wkt is required with crs_geokeys and refused without.
    for (keys, drop_wkt, add_wkt) in [(true, true, false), (false, false, true)] {
        let dir = scratch();
        let mut v = boundary_request(&dir.0, keys);
        if drop_wkt {
            v.as_object_mut().unwrap().remove("crs_wkt");
        }
        if add_wkt {
            v["crs_wkt"] = ESRI_WKT.into();
        }
        let req: boundary_session::Request = serde_json::from_value(v).unwrap();
        let error = boundary_session::run(&req, &dir.0, &dir.0.join("out")).unwrap_err();
        assert!(error.to_string().contains("crs_wkt"), "{error}");
    }
}

fn groups_request(dir: &Path, keys: bool, sink: &str) -> Value {
    let mut model = sloping_model(dir, keys);
    for flag in ["stream_outputs", "geotiff_outputs"] {
        model.as_object_mut().unwrap().remove(flag);
    }
    let mut v = json!({"schema": ras_commander_fast_map::depth_groups::SCHEMA, "model": model,
        "source_indices": [0, 1], "depth_units": "feet", "depth_cutoff_ft": 0.0,
        "max_accumulator_bytes": 1u64 << 20, "compression": "LZW", "sink": sink,
        "jobs": [{"job_id": "j", "reach_id": "r", "window": [0, 0, 4, 4],
            "groups": [{"group_id": "g", "timesteps": [1, 0]}]}]});
    if keys {
        v["crs_wkt"] = ESRI_WKT.into();
    }
    v
}

#[test]
fn depth_groups_carry_the_keys_and_the_request_wkt() {
    use ras_commander_fast_map::depth_groups;
    let (k, e) = (scratch(), scratch());
    let run = |dir: &Path, keys: bool| {
        let req: depth_groups::Request =
            serde_json::from_value(groups_request(dir, keys, "grouped-geotiff")).unwrap();
        depth_groups::run(&req, dir, &dir.join("out")).unwrap()
    };
    let (kr, er) = (run(&k.0, true), run(&e.0, false));
    let (kt, et) = (k.0.join("out/j/g.tif"), e.0.join("out/j/g.tif"));
    assert_alabama_west(&kt);
    assert_eq!(
        read_geokeys(&et).unwrap(),
        Some(GeoKeys::from_epsg(2277, false))
    );
    assert_eq!(pixels(&kt), pixels(&et));
    let (kt, et) = (kr.to_string(), er.to_string());
    assert!(
        kt.contains(&serde_json::to_string(ESRI_WKT).unwrap()),
        "{kt}"
    );
    assert!(kt.contains(&alabama_west().fnv1a64()));
    assert!(!et.contains("NAD_1983_StatePlane_Alabama") && !et.contains(&alabama_west().fnv1a64()));
    // A model with keys needs crs_wkt; an EPSG model refuses it.
    let dir = scratch();
    let mut v = groups_request(&dir.0, true, "grouped-geotiff");
    v.as_object_mut().unwrap().remove("crs_wkt");
    let req: depth_groups::Request = serde_json::from_value(v).unwrap();
    assert!(req.validate().unwrap_err().to_string().contains("crs_wkt"));
    let mut v = groups_request(&dir.0, false, "grouped-geotiff");
    v["crs_wkt"] = ESRI_WKT.into();
    let req: depth_groups::Request = serde_json::from_value(v).unwrap();
    assert!(req.validate().unwrap_err().to_string().contains("crs_wkt"));
}

#[test]
fn batch_session_samples_a_geokeys_terrain_like_its_epsg_twin() {
    use ras_commander_fast_map::{model::SessionRequest, session};
    let g = Grid {
        origin_x: 0.,
        origin_y: 100.,
        cell_size: 1.,
        width: 100,
        height: 100,
    };
    let values: Vec<f32> = (0..10_000)
        .map(|i| 95.0 + (i % 97) as f32 * 0.0625)
        .collect();
    let (k, e) = (scratch(), scratch());
    let mut outputs = Vec::new();
    for (dir, keys) in [(&k.0, true), (&e.0, false)] {
        let crs = if keys {
            OutputCrs::GeoKeys(alabama_west().into())
        } else {
            OutputCrs::Epsg(2277)
        };
        effective_terrain::write_geotiff_crs(
            &dir.join("terrain.tif"),
            effective(g, values.clone()),
            &crs,
        )
        .unwrap();
        let (n, step) = (9usize, 10.0);
        let samples: Vec<Value> = (0..n * n)
            .map(|i| {
                json!([
                    (i % n) as f64 * step,
                    (i / n) as f64 * step,
                    100.0 + 0.01 * i as f64
                ])
            })
            .collect();
        let mut simplices = Vec::new();
        for j in 0..n - 1 {
            for i in 0..n - 1 {
                let a = j * n + i;
                simplices.push(json!([a, a + 1, a + n + 1]));
                simplices.push(json!([a, a + n + 1, a + n]));
            }
        }
        let mut v = json!({"schema": "ras2fim-batch-session/1",
            "terrain": {"mode": "native_geotiff", "path": "terrain.tif", "query": "pixel_center",
                "nodata_policy": "dry"},
            "budgets": {"threads": 2, "max_cache_bytes": 1u64 << 24, "max_inflight_profiles": 1,
                "max_profile_bytes": 1u64 << 28, "output_tile": 16, "max_tiles_in_flight": 4},
            "products": ["wse", "depth"],
            "profiles": [{"id": "p", "source": {"kind": "inline", "samples": samples, "simplices": simplices,
                "grid": {"kind": "explicit", "width": 40, "height": 30, "x0": 0.5, "y0": 0.5, "dx": 1.2, "dy": 1.2},
                "mask_all_wet": true}}],
            "aois": [{"id": "full"}], "error_policy": "fail_fast"});
        if keys {
            v["crs_geokeys"] = keys_json();
            v["terrain"]["crs_identity"] = json!({"kind": "geokeys"});
        } else {
            v["crs_epsg"] = 2277.into();
            v["terrain"]["crs_identity"] = json!({"kind": "epsg"});
        }
        let req: SessionRequest = serde_json::from_value(v).unwrap();
        let report = session::run(&req, dir, &dir.join("out")).unwrap();
        assert_eq!(report["status"], "complete", "{report}");
        assert_eq!(report.get("output_crs").is_some(), keys);
        outputs.push(
            ["wse", "depth"]
                .map(|p| fs::read(dir.join("out/units/p/full").join(format!("{p}.bin"))).unwrap()),
        );
    }
    assert_eq!(outputs[0], outputs[1]);
}
