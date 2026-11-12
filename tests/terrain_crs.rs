//! Terrains whose projected CRS is user-defined in the GeoKeys (ProjectedCSTypeGeoKey 32767), as
//! HEC-RAS writes them: accepted under `crs_identity: epsg` only when they match the session EPSG
//! parameter for parameter (`crs_match`), and refused otherwise.
use ras_commander_fast_map::{
    boundary_session::{self, Request},
    model::{CrsIdentity, NodataPolicy, QueryConvention},
    terrain::{self, NativeTerrain},
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
        "fast-map-terrain-crs-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&p).unwrap();
    p
}

/// GeoKeys of a user-defined Lambert Conformal Conic 2SP CRS, as HEC-RAS writes the Texas South
/// Central (ftUS) zone: GTModelType projected, GeographicType `geographic`, ProjectedCSType and
/// Projection user-defined (32767), CoordTrans 8 (LCC 2SP), ProjLinearUnits `unit`.
struct Keys {
    geographic: u16,
    unit: u16,
    /// LatOrigin, CentralMeridian, StdParallel1, StdParallel2, FalseEasting, FalseNorthing.
    params: [f64; 6],
}

const TEXAS_SOUTH_CENTRAL_FTUS: Keys = Keys {
    geographic: 4269,
    unit: 9003,
    params: [
        27.8333333333333,
        -99.0,
        30.2833333333333,
        28.3833333333333,
        1968500.0,
        13123333.333,
    ],
};

/// A 4x4 Float32 GeoTIFF (one 256x256 tile, uncompressed, NoData -9999, zeros) carrying `keys`.
fn write_user_defined_tiff(path: &Path, keys: &Keys) {
    let (w, h, tile) = (4u32, 4u32, 256u32);
    let citation = "NAD83 / Texas South Central (ftUS)|NAD83|\0";
    let doubles: Vec<f64> = keys
        .params
        .iter()
        .copied()
        .chain([298.257222101, 6378137.0])
        .collect();
    let geo_keys: Vec<u16> = vec![
        1,
        1,
        0,
        12, //
        1024,
        0,
        1,
        1, // GTModelType projected
        1025,
        0,
        1,
        1, // RasterType PixelIsArea
        1026,
        34737,
        35,
        0, // GTCitation
        2048,
        0,
        1,
        keys.geographic, // GeographicType
        2049,
        34737,
        6,
        35, // GeogCitation
        2054,
        0,
        1,
        9102, // GeogAngularUnits degree
        3072,
        0,
        1,
        32767, // ProjectedCSType user-defined
        3074,
        0,
        1,
        32767, // Projection user-defined
        3075,
        0,
        1,
        8, // CoordTrans LCC 2SP
        3076,
        0,
        1,
        keys.unit, // ProjLinearUnits
        3078,
        34736,
        1,
        2, // StdParallel1
        3079,
        34736,
        1,
        3, // StdParallel2
    ]
    .into_iter()
    .chain([
        3084, 34736, 1, 1, // FalseOriginLong
        3085, 34736, 1, 0, // FalseOriginLat
        3086, 34736, 1, 4, // FalseOriginEasting
        3087, 34736, 1, 5, // FalseOriginNorthing
    ])
    .collect();
    let mut geo_keys = geo_keys;
    geo_keys[3] = ((geo_keys.len() - 4) / 4) as u16;

    // Layout: header, tile data, then out-of-line tag values, then the IFD.
    let mut out: Vec<u8> = b"II*\0\0\0\0\0".to_vec();
    let tile_offset = out.len() as u32;
    out.extend(vec![0u8; (tile * tile * 4) as usize]);
    let blob = |out: &mut Vec<u8>, bytes: Vec<u8>| -> u32 {
        if out.len() % 2 == 1 {
            out.push(0);
        }
        let at = out.len() as u32;
        out.extend(bytes);
        at
    };
    let le16 = |v: &[u16]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<_>>();
    let le64 = |v: &[f64]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<_>>();
    let scale = blob(&mut out, le64(&[3.0, 3.0, 0.0]));
    let tie = blob(&mut out, le64(&[0.0, 0.0, 0.0, 3086553.0, 13796469.0, 0.0]));
    let gk = blob(&mut out, le16(&geo_keys));
    let gd = blob(&mut out, le64(&doubles));
    let ga = blob(&mut out, citation.as_bytes().to_vec());
    let software = blob(&mut out, b"HEC-RAS\0".to_vec());
    let nodata = blob(&mut out, b"-9999\0".to_vec());
    // (tag, type, count, value-or-offset)
    let mut tags: Vec<(u16, u16, u32, u32)> = vec![
        (256, 4, 1, w),
        (257, 4, 1, h),
        (258, 3, 1, 32),
        (259, 3, 1, 1),
        (262, 3, 1, 1),
        (277, 3, 1, 1),
        (284, 3, 1, 1),
        (305, 2, 8, software),
        (322, 4, 1, tile),
        (323, 4, 1, tile),
        (324, 4, 1, tile_offset),
        (325, 4, 1, tile * tile * 4),
        (339, 3, 1, 3),
        (33550, 12, 3, scale),
        (33922, 12, 6, tie),
        (34735, 3, geo_keys.len() as u32, gk),
        (34736, 12, doubles.len() as u32, gd),
        (34737, 2, citation.len() as u32, ga),
        (42113, 2, 6, nodata),
    ];
    tags.sort_by_key(|t| t.0);
    if out.len() % 2 == 1 {
        out.push(0);
    }
    let ifd = out.len() as u32;
    out[4..8].copy_from_slice(&ifd.to_le_bytes());
    out.extend((tags.len() as u16).to_le_bytes());
    for (tag, typ, count, value) in tags {
        out.extend(tag.to_le_bytes());
        out.extend(typ.to_le_bytes());
        out.extend(count.to_le_bytes());
        if typ == 3 && count == 1 {
            out.extend((value as u16).to_le_bytes());
            out.extend([0, 0]);
        } else {
            out.extend(value.to_le_bytes());
        }
    }
    out.extend(0u32.to_le_bytes());
    fs::write(path, out).unwrap();
}

fn open(path: &Path, epsg: u32) -> Result<NativeTerrain, String> {
    NativeTerrain::open(
        path.to_str().unwrap(),
        epsg,
        &CrsIdentity::Epsg,
        QueryConvention::PixelCenter,
        NodataPolicy::Dry,
        1 << 20,
    )
    .map_err(|e| e.to_string())
}

#[test]
fn hec_ras_user_defined_projection_is_identified_as_the_session_epsg() {
    let folder = scratch();
    let tif = folder.join("terrain.tif");
    write_user_defined_tiff(&tif, &TEXAS_SOUTH_CENTRAL_FTUS);
    // terrio reads the CRS as GDAL does: no EPSG code.
    let info = terrain::inspect(tif.to_str().unwrap()).unwrap();
    assert_eq!(info.epsg, None);
    assert!(info.crs.wkt.as_deref().unwrap().starts_with("PROJCS["));
    // The same WKT terrio reads from the HEC-RAS-written South Belt terrain (FNV-1a 64).
    assert_eq!(info.crs_wkt_fnv1a64.as_deref(), Some("f2d2ea289ee9b151"));

    let t = open(&tif, 2278).unwrap();
    assert_eq!(t.crs_identity, "parameter_matched_epsg");
    let m = t.crs_epsg_match.as_ref().unwrap();
    assert_eq!(
        (m.epsg, m.geographic_epsg, m.datum_epsg),
        (2278, 4269, 6269)
    );
    assert_eq!(m.linear_unit_epsg, Some(9003));
    assert_eq!(m.rule, ras_commander_fast_map::crs_match::RULE);
    assert_eq!(t.nodata, Some(-9999.0));

    // Same parameters, other EPSG CRSs: NAD83(2011), the metre zone, another zone.
    for (epsg, needle) in [
        (6588, "geographic CRS is EPSG:4269, not EPSG:6318"),
        (32140, "linear unit"),
        (2277, "parameter"),
    ] {
        let e = open(&tif, epsg).err().unwrap();
        assert!(
            e.contains(&format!("does not match EPSG:{epsg}")) && e.contains(needle),
            "{e}"
        );
        assert!(e.contains("caller_verified_wkt"), "{e}");
    }
    fs::remove_dir_all(folder).unwrap();
}

#[test]
fn near_misses_of_the_session_epsg_are_refused() {
    let cases = [
        // International foot instead of US survey foot.
        (
            Keys {
                unit: 9002,
                ..TEXAS_SOUTH_CENTRAL_FTUS
            },
            "linear unit",
        ),
        // NAD83(HARN) instead of NAD83: terrio identifies this one itself (EPSG:2919, its
        // geographic-on-datum path, the one that fixed Davis), and the EPSG check refuses it.
        (
            Keys {
                geographic: 4152,
                ..TEXAS_SOUTH_CENTRAL_FTUS
            },
            "terrain EPSG:2919 differs from session EPSG:2278",
        ),
        // False easting 0.01 ft off (3 mm).
        (
            Keys {
                params: {
                    let mut p = TEXAS_SOUTH_CENTRAL_FTUS.params;
                    p[4] += 0.01;
                    p
                },
                ..TEXAS_SOUTH_CENTRAL_FTUS
            },
            "false_easting",
        ),
        // Standard parallel 1e-7 degree off (about 1 cm).
        (
            Keys {
                params: {
                    let mut p = TEXAS_SOUTH_CENTRAL_FTUS.params;
                    p[2] += 1e-7;
                    p
                },
                ..TEXAS_SOUTH_CENTRAL_FTUS
            },
            "standard_parallel_1",
        ),
    ];
    for (keys, needle) in cases {
        let folder = scratch();
        let tif = folder.join("terrain.tif");
        write_user_defined_tiff(&tif, &keys);
        let e = open(&tif, 2278).err().unwrap();
        assert!(e.contains(needle), "{e}");
        assert!(
            e.contains("does not match EPSG:2278") || e.contains("differs from session"),
            "{e}"
        );
        fs::remove_dir_all(folder).unwrap();
    }
}

/// HEC-RAS writes Brays Bayou's EPSG:2278 terrain with the two standard parallels swapped (and
/// the false northing to more digits); Lambert Conformal Conic 2SP is symmetric in them.
#[test]
fn swapped_standard_parallels_are_identified_and_recorded() {
    let folder = scratch();
    let tif = folder.join("terrain.tif");
    let mut params = TEXAS_SOUTH_CENTRAL_FTUS.params;
    params.swap(2, 3);
    params[5] = 13123333.3333333;
    write_user_defined_tiff(
        &tif,
        &Keys {
            params,
            ..TEXAS_SOUTH_CENTRAL_FTUS
        },
    );
    let t = open(&tif, 2278).unwrap();
    assert_eq!(t.crs_identity, "parameter_matched_epsg");
    assert!(
        t.crs_epsg_match
            .as_ref()
            .unwrap()
            .standard_parallels_swapped
    );
    fs::remove_dir_all(folder).unwrap();
}

#[test]
fn caller_verified_wkt_still_pins_a_parameter_matched_terrain() {
    let folder = scratch();
    let tif = folder.join("terrain.tif");
    write_user_defined_tiff(&tif, &TEXAS_SOUTH_CENTRAL_FTUS);
    let fnv = terrain::inspect(tif.to_str().unwrap())
        .unwrap()
        .crs_wkt_fnv1a64
        .unwrap();
    let t = NativeTerrain::open(
        tif.to_str().unwrap(),
        2278,
        &CrsIdentity::CallerVerifiedWkt { fnv1a64: fnv },
        QueryConvention::PixelCenter,
        NodataPolicy::Dry,
        1 << 20,
    )
    .unwrap();
    // The caller's pin, unchanged, is how this one was identified.
    assert_eq!(t.crs_identity, "caller_verified_wkt_fnv1a64");
    assert!(t.crs_epsg_match.is_none());
    fs::remove_dir_all(folder).unwrap();
}

/// The boundary session (where the downstream failure was seen) accepts the terrain and records
/// how its CRS was identified.
#[test]
fn boundary_session_accepts_and_records_a_parameter_matched_terrain() {
    let folder = scratch();
    write_user_defined_tiff(&folder.join("terrain.tif"), &TEXAS_SOUTH_CENTRAL_FTUS);
    fs::write(folder.join("mask"), vec![1u8; 65536]).unwrap();
    fs::write(
        folder.join("perimeter"),
        (-9999f32).to_le_bytes().repeat(513),
    )
    .unwrap();
    fs::write(folder.join("levels"), 1f32.to_le_bytes()).unwrap();
    let (x0, y0) = (3086553.0f64, 13796469.0f64);
    let mut polygon = vec![1u8];
    for n in [3u32, 1, 5] {
        polygon.extend(n.to_le_bytes());
    }
    for [x, y] in [
        [0.0f64, 0.0f64],
        [0., -12.],
        [12., -12.],
        [12., 0.],
        [0., 0.],
    ] {
        polygon.extend((x0 + x).to_le_bytes());
        polygon.extend((y0 + y).to_le_bytes());
    }
    let mut geometry = (polygon.len() as u32).to_le_bytes().to_vec();
    geometry.extend(polygon);
    fs::write(folder.join("storage.wkb"), geometry).unwrap();
    let transform = json!([x0, 3.0, 0, y0, 0, -3.0]);
    let request = |epsg: u32| -> Request {
        let v: Value = json!({"schema":"ras-commander-boundary-request/2", "interpolation":"sloping",
            "contour_method":"rasmapper-serial-zero-depth-contours/1", "diagnostic_units":true,
            "model":{"schema":"ras-commander-sloping-request/1", "terrain":"terrain.tif", "epsg":epsg,
                "crs_identity":{"kind":"epsg"}, "terrain_transform":transform, "window":[0,0,4,4],
                "geometry_file":"unused", "mesh_files":[], "owner_cells":[], "storage_areas_file":"storage.wkb",
                "storage_area_count":1, "profiles":[{"id":"a", "values_files":[], "storage_area_values_file":"levels"}],
                "threads":1, "max_inflight_profiles":1, "max_working_bytes":134217728},
            "terrain_metadata":{"shape":[4,4], "transform":transform, "tile_shape":[256,256],
                "tile_grid":[1,1], "selected_tiles":[0], "mask":{"path":"mask", "dtype":"|u1", "shape":[1,65536]},
                "perimeter":{"path":"perimeter", "dtype":"<f4", "shape":[1,513]}}});
        serde_json::from_value(v).unwrap()
    };
    let report = boundary_session::run(&request(2278), &folder, &folder.join("ok")).unwrap();
    assert_eq!(report["status"], "complete", "{report}");
    let crs = &report["terrain_crs"][0];
    assert_eq!(crs["crs_identity"], "parameter_matched_epsg");
    assert_eq!(crs["crs_epsg_match"]["epsg"], 2278);
    assert_eq!(
        crs["crs_epsg_match"]["rule"],
        "fast-map-projected-epsg-parameter-match/1"
    );
    assert_eq!(report["profiles"][0]["status"], "complete", "{report}");
    let e = boundary_session::run(&request(6588), &folder, &folder.join("refused"))
        .unwrap_err()
        .to_string();
    assert!(e.contains("does not match EPSG:6588"), "{e}");
    fs::remove_dir_all(folder).unwrap();
}

/// Opt-in check on a real terrain: `RAS_FAST_MAP_CRS_TERRAIN` names the GeoTIFF and
/// `RAS_FAST_MAP_CRS_EPSG` the EPSG code it must be identified as.
#[test]
#[ignore = "requires RAS_FAST_MAP_CRS_TERRAIN and RAS_FAST_MAP_CRS_EPSG"]
fn real_terrain_crs_is_identified_as_the_expected_epsg() {
    let path = std::env::var("RAS_FAST_MAP_CRS_TERRAIN").expect("terrain path required");
    let epsg: u32 = std::env::var("RAS_FAST_MAP_CRS_EPSG")
        .expect("EPSG code required")
        .parse()
        .unwrap();
    let info = terrain::inspect(&path).unwrap();
    let t = open(Path::new(&path), epsg).unwrap();
    println!(
        "terrio epsg {:?}; crs_identity {}; crs_wkt_fnv1a64 {:?}; crs_epsg_match {}",
        info.epsg,
        t.crs_identity,
        t.crs_wkt_fnv1a64,
        serde_json::to_string(&t.crs_epsg_match).unwrap()
    );
    assert!(matches!(
        t.crs_identity,
        "terrio_identified_epsg" | "parameter_matched_epsg"
    ));
}
