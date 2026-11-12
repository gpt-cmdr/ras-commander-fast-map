//! Effective terrain (`rasmapper-effective-terrain/2`): priority, footprint,
//! export-TIN gap fill, rounding, lattice, terrain modifications and refusals on small synthetic
//! sources.
use ras_commander_fast_map::effective_terrain::{
    self, Effective, Grid, METHOD_ID, NODATA, Request, quantize, snap_grid,
};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

static NEXT: AtomicUsize = AtomicUsize::new(0);
const EPSG: u32 = 26915;

fn scratch() -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "fast-map-effective-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&p).unwrap();
    p
}

fn write_source(dir: &Path, name: &str, grid: Grid, values: Vec<f32>) {
    let effective = Effective {
        grid,
        values,
        report: Value::Null,
    };
    effective_terrain::write_geotiff(&dir.join(name), effective, EPSG).unwrap();
}

/// Level-0 tile mask with bit 0 set where `f(row, col)`.
fn write_mask(dir: &Path, name: &str, w: usize, h: usize, f: impl Fn(usize, usize) -> bool) {
    let (tx, ty) = (w.div_ceil(256), h.div_ceil(256));
    let mut m = vec![0_u8; tx * ty * 65536];
    for r in 0..h {
        for c in 0..w {
            if f(r, c) {
                m[((r / 256) * tx + c / 256) * 65536 + (r % 256) * 256 + c % 256] = 1;
            }
        }
    }
    fs::write(dir.join(name), m).unwrap();
}

/// Level-0 tile mask as terrain creation writes it: bit 0 where block
/// `valid(row, col)`, plus bits 16/8/4/2 on valid blocks whose neighbour
/// above/left/right/below is off (or outside the raster).
fn write_mask_with_edges(
    dir: &Path,
    name: &str,
    w: usize,
    h: usize,
    valid: impl Fn(usize, usize) -> bool,
) {
    let (tx, ty) = (w.div_ceil(256), h.div_ceil(256));
    let mut m = vec![0_u8; tx * ty * 65536];
    let on = |r: Option<usize>, c: Option<usize>| match (r, c) {
        (Some(r), Some(c)) if r < h && c < w => valid(r, c),
        _ => false,
    };
    for r in 0..h {
        for c in 0..w {
            if !valid(r, c) {
                continue;
            }
            let mut b = 1_u8;
            if !on(r.checked_sub(1), Some(c)) {
                b |= 16;
            }
            if !on(Some(r), c.checked_sub(1)) {
                b |= 8;
            }
            if !on(Some(r), Some(c + 1)) {
                b |= 4;
            }
            if !on(Some(r + 1), Some(c)) {
                b |= 2;
            }
            m[((r / 256) * tx + c / 256) * 65536 + (r % 256) * 256 + c % 256] = b;
        }
    }
    fs::write(dir.join(name), m).unwrap();
}

fn mask(name: &str, w: usize, h: usize) -> Value {
    json!({"path": name, "dtype": "|u1", "shape": [w.div_ceil(256) * h.div_ceil(256), 65536]})
}

fn request(sources: Value, extra: Value) -> Request {
    let mut v = json!({
        "schema": "ras-commander-effective-terrain-request/1",
        "sources": sources, "epsg": EPSG, "crs_identity": {"kind": "epsg"},
        "modifications": 0, "threads": 2, "max_working_bytes": 1u64 << 28,
    });
    for (k, x) in extra.as_object().unwrap() {
        v[k] = x.clone();
    }
    serde_json::from_value(v).unwrap()
}

fn grid(x: f64, y: f64, cell: f64, w: usize, h: usize) -> Grid {
    Grid {
        origin_x: x,
        origin_y: y,
        cell_size: cell,
        width: w,
        height: h,
    }
}

/// Plane z = 100 + 0.5 x + 0.25 y sampled at pixel centres, already on the 1/32 grid.
fn plane(g: &Grid) -> Vec<f32> {
    let mut v = Vec::with_capacity(g.width * g.height);
    for r in 0..g.height {
        for c in 0..g.width {
            let x = g.origin_x + (c as f64 + 0.5) * g.cell_size;
            let y = g.origin_y - (r as f64 + 0.5) * g.cell_size;
            v.push(quantize(100.0 + 0.5 * x + 0.25 * y));
        }
    }
    v
}

#[test]
fn rounding_is_binary32_then_one_thirty_second_ties_even() {
    assert_eq!(quantize(1.0 / 64.0), 0.0);
    assert_eq!(quantize(3.0 / 64.0), 2.0 / 32.0);
    assert_eq!(quantize(700.28125 + 0.0001), 700.28125);
    assert_eq!(quantize(-0.999), -1.0);
}

#[test]
fn lattice_snaps_outward() {
    let g = snap_grid([5.0, -3.0, 31.0, 19.0], 0.0, 0.0, 10.0).unwrap();
    assert_eq!(g, grid(0.0, 20.0, 10.0, 4, 3));
    // Exact multiples within tolerance do not grow the grid.
    let g = snap_grid([0.0, 0.0, 30.000000000001, 20.0], 0.0, 0.0, 10.0).unwrap();
    assert_eq!((g.width, g.height), (3, 2));
}

#[test]
fn single_source_interior_is_source_and_edges_need_a_full_quad() {
    let dir = scratch();
    let g = grid(1000.0, 2000.0, 10.0, 20, 12);
    let values = plane(&g);
    write_source(&dir, "a.tif", g, values.clone());
    write_mask(&dir, "a.u1", 20, 12, |_, _| true);
    let req = request(
        json!([{"path": "a.tif", "mask": mask("a.u1", 20, 12)}]),
        json!({}),
    );
    let out = effective_terrain::compute(&req, &dir).unwrap();
    assert_eq!(out.grid, g);
    for r in 0..12 {
        for c in 0..20 {
            let v = out.values[r * 20 + c];
            if r == 11 || c == 19 {
                // The quad of the last row/column leaves the raster.
                assert_eq!(v, NODATA, "({r},{c})");
            } else {
                assert_eq!(v, values[r * 20 + c], "({r},{c})");
            }
        }
    }
    // Schema 1 runs the /2 rule, so the receipt names /2.
    assert_eq!(out.report["method_id"], METHOD_ID);
}

#[test]
fn coarse_source_uses_the_two_diagonal_mean() {
    let dir = scratch();
    // 3 x 3 coarse source; one fine-lattice pixel inside its first quad.
    let g = grid(0.0, 30.0, 10.0, 3, 3);
    let v = vec![0.0, 32.0, 0.0, 64.0, 0.0, 0.0, 0.0, 0.0, 0.0];
    write_source(&dir, "c.tif", g, v);
    write_mask(&dir, "c.u1", 3, 3, |_, _| true);
    let req = request(
        json!([{"path": "c.tif", "mask": mask("c.u1", 3, 3)}]),
        json!({"grid": {"origin_x": 0.0, "origin_y": 30.0, "cell_size": 2.0, "width": 15, "height": 15}}),
    );
    let out = effective_terrain::compute(&req, &dir).unwrap();
    // Pixel (row 3, col 4): x = 9, y = 23 -> tx = 0.4, ty = 0.2 in quad (0,0).
    let (tx, ty) = (0.4_f64, 0.2_f64);
    let (v00, v01, v10, v11) = (0.0, 32.0, 64.0, 0.0);
    let t0 = v00 + tx * (v01 - v00) + ty * (v11 - v01);
    let t1 = v00 + tx * (v01 - v00) + ty * (v10 - v00);
    let bilinear = (v00 * (1.0 - tx) + v01 * tx) * (1.0 - ty) + (v10 * (1.0 - tx) + v11 * tx) * ty;
    let got = out.values[3 * 15 + 4];
    assert_eq!(got, quantize((t0 + t1) / 2.0));
    assert_ne!(got, quantize(bilinear));
}

/// Two sources on one lattice: the high source has a hole; the low source
/// fills it, and its own pixels under the high source never show. Masks are
/// per render block (the quad between four cell centres), as terrain
/// creation writes them: a block is on when its four corners are valid and
/// no higher-priority source owns it.
#[test]
fn priority_footprint_and_fall_through() {
    let dir = scratch();
    let g = grid(0.0, 200.0, 10.0, 20, 20);
    let hole = |r: usize, c: usize| (8..12).contains(&r) && (8..12).contains(&c);
    let high: Vec<f32> = (0..400)
        .map(|i| if hole(i / 20, i % 20) { -9999.0 } else { 500.0 })
        .collect();
    let low = vec![100.0_f32; 400];
    write_source(&dir, "high.tif", g, high);
    write_source(&dir, "low.tif", g, low);
    let corners_valid = |r: usize, c: usize| {
        r + 1 < 20
            && c + 1 < 20
            && [(r, c), (r, c + 1), (r + 1, c), (r + 1, c + 1)]
                .iter()
                .all(|&(a, b)| !hole(a, b))
    };
    write_mask(&dir, "high.u1", 20, 20, corners_valid);
    // The low source owns only blocks the high source leaves off.
    write_mask(&dir, "low.u1", 20, 20, |r, c| {
        r + 1 < 20 && c + 1 < 20 && !corners_valid(r, c)
    });
    let srcs = json!([
        {"path": "high.tif", "mask": mask("high.u1", 20, 20)},
        {"path": "low.tif", "mask": mask("low.u1", 20, 20)},
    ]);
    let out = effective_terrain::compute(&request(srcs, json!({})), &dir).unwrap();
    let at = |r: usize, c: usize| out.values[r * 20 + c];
    assert_eq!(at(2, 2), 500.0);
    assert_eq!(at(5, 14), 500.0);
    assert_eq!(at(9, 9), 100.0, "hole filled from the lower source");
    assert_eq!(at(10, 10), 100.0);
    // Pixel (7,7) sits on a block corner: whichever adjacent block the
    // lattice picks, one of the two sources answers.
    assert!([100.0, 500.0].contains(&at(7, 7)));
    // Reversed priority with a mask that owns nothing: the low source never shows.
    write_mask(&dir, "none.u1", 20, 20, |_, _| false);
    let srcs = json!([
        {"path": "low.tif", "mask": mask("low.u1", 20, 20)},
        {"path": "high.tif", "mask": mask("none.u1", 20, 20)},
    ]);
    let out = effective_terrain::compute(&request(srcs, json!({})), &dir).unwrap();
    assert!(out.values.iter().all(|v| *v == 100.0 || *v == NODATA));
    assert_eq!(out.report["sources"][1]["pixels"], 0);
}

/// The request's stored stitch TIN is accepted for compatibility but no
/// longer part of the rule: RAS Mapper's export does not use it.
#[test]
fn stored_stitch_tin_is_accepted_and_not_used() {
    let dir = scratch();
    let g = grid(0.0, 100.0, 10.0, 10, 10);
    write_source(&dir, "a.tif", g, vec![50.0; 100]);
    write_mask(&dir, "a.u1", 10, 10, |_, _| true);
    let pts: [[f64; 4]; 3] = [
        [0.0, 100.0, 10.0, 10.0],
        [40.0, 100.0, 50.0, 10.0],
        [0.0, 60.0, 10.0, 10.0],
    ];
    let bytes: Vec<u8> = pts.iter().flatten().flat_map(|v| v.to_le_bytes()).collect();
    fs::write(dir.join("p.f8"), bytes).unwrap();
    let tri: Vec<u8> = [0_i32, 1, 2].iter().flat_map(|v| v.to_le_bytes()).collect();
    fs::write(dir.join("t.i4"), tri).unwrap();
    let tin = json!({"points": {"path": "p.f8", "dtype": "<f8", "shape": [3, 4]},
                     "triangles": {"path": "t.i4", "dtype": "<i4", "shape": [1, 3]}});
    let srcs = json!([{"path": "a.tif", "mask": mask("a.u1", 10, 10)}]);
    let out = effective_terrain::compute(&request(srcs.clone(), json!({"stitch_tin": tin})), &dir)
        .unwrap();
    assert_eq!(out.values[0], 50.0);
    assert_eq!(out.values[1], 50.0);
    assert_eq!(out.report["stitch_tin"]["supplied"], true);
    assert_eq!(out.report["stitch_tin"]["used"], false);
    // Still validated.
    let bad = json!({"points": {"path": "p.f8", "dtype": "<f4", "shape": [3, 4]},
                     "triangles": {"path": "t.i4", "dtype": "<i4", "shape": [1, 3]}});
    let err = effective_terrain::compute(&request(srcs, json!({"stitch_tin": bad})), &dir);
    assert_eq!(err.err().unwrap().kind(), "invalid");
}

/// A nodata source column `gap` cells wide: one cell (two off render
/// blocks) is bridged by the per-tile export TIN with the linear value
/// across the gap; two cells are not.
fn gap_terrain(gap: usize) -> (Vec<f32>, Vec<f32>) {
    let dir = scratch();
    let g = grid(1000.0, 2000.0, 10.0, 20, 12);
    let plane_values = plane(&g);
    let hole = |c: usize| (8..8 + gap).contains(&c);
    let values: Vec<f32> = (0..240)
        .map(|i| {
            if hole(i % 20) {
                NODATA
            } else {
                plane_values[i]
            }
        })
        .collect();
    write_source(&dir, "a.tif", g, values);
    write_mask_with_edges(&dir, "a.u1", 20, 12, |r, c| {
        r + 1 < 12 && c + 1 < 20 && !hole(c) && !hole(c + 1)
    });
    let req = request(
        json!([{"path": "a.tif", "mask": mask("a.u1", 20, 12)}]),
        json!({}),
    );
    let out = effective_terrain::compute(&req, &dir).unwrap();
    assert!(out.report["export_tin"]["segments"].as_u64().unwrap() > 0);
    assert_eq!(out.report["export_tin"]["failed_tiles"], 0);
    (out.values, plane_values)
}

#[test]
fn export_tin_bridges_a_one_cell_gap() {
    let (out, plane_values) = gap_terrain(1);
    for r in 1..11 {
        for c in [7, 8] {
            let i = r * 20 + c;
            assert_eq!(out[i], plane_values[i], "({r},{c})");
        }
    }
}

#[test]
fn export_tin_leaves_a_two_cell_gap() {
    let (out, _) = gap_terrain(2);
    for r in 0..12 {
        for c in [8, 9] {
            assert_eq!(out[r * 20 + c], NODATA, "({r},{c})");
        }
    }
}

#[test]
fn refusals_before_reads() {
    let dir = scratch();
    let srcs = json!([{"path": "missing.tif", "mask": mask("m.u1", 10, 10)}]);
    let mods = request(srcs.clone(), json!({"modifications": 1}));
    assert_eq!(
        effective_terrain::compute(&mods, &dir)
            .err()
            .unwrap()
            .kind(),
        "unsupported"
    );
    let schema = request(srcs.clone(), json!({"schema": "x"}));
    assert_eq!(
        effective_terrain::compute(&schema, &dir)
            .err()
            .unwrap()
            .kind(),
        "invalid"
    );
    let bad_mask = request(
        json!([{"path": "missing.tif", "mask": {"path": "m.u1", "dtype": "|u1", "shape": [1, 10]}}]),
        json!({}),
    );
    assert_eq!(
        effective_terrain::compute(&bad_mask, &dir)
            .err()
            .unwrap()
            .kind(),
        "invalid"
    );
    let none = request(json!([]), json!({}));
    assert!(effective_terrain::compute(&none, &dir).is_err());
}

#[test]
fn mask_shape_and_budget_are_checked() {
    let dir = scratch();
    let g = grid(0.0, 100.0, 10.0, 10, 10);
    write_source(&dir, "a.tif", g, vec![1.0; 100]);
    write_mask(&dir, "a.u1", 10, 10, |_, _| true);
    let wrong =
        json!([{"path": "a.tif", "mask": {"path": "a.u1", "dtype": "|u1", "shape": [2, 65536]}}]);
    assert_eq!(
        effective_terrain::compute(&request(wrong, json!({})), &dir)
            .err()
            .unwrap()
            .kind(),
        "invalid"
    );
    let srcs = json!([{"path": "a.tif", "mask": mask("a.u1", 10, 10)}]);
    let tiny = request(srcs, json!({"max_working_bytes": 64}));
    assert_eq!(
        effective_terrain::compute(&tiny, &dir)
            .err()
            .unwrap()
            .kind(),
        "budget"
    );
}

#[test]
fn run_writes_tiled_geotiff_and_report() {
    let dir = scratch();
    let g = grid(0.0, 100.0, 10.0, 10, 10);
    write_source(&dir, "a.tif", g, plane(&g));
    write_mask(&dir, "a.u1", 10, 10, |_, _| true);
    let req = request(
        json!([{"path": "a.tif", "mask": mask("a.u1", 10, 10)}]),
        json!({}),
    );
    let out = dir.join("out");
    let report = effective_terrain::run(&req, &dir, &out).unwrap();
    assert_eq!(report["status"], "complete");
    let info = ras_commander_fast_map::terrain::inspect(
        out.join("effective_terrain.tif").to_str().unwrap(),
    )
    .unwrap();
    assert_eq!((info.width, info.height), (10, 10));
    assert_eq!(info.nodata, Some(NODATA as f64));
    assert_eq!(info.block, (256, 256));
    assert!(out.join("report.json").is_file());
}

/// Schema 2: modification layers are applied after the mosaic, Priority 0
/// last (it wins), and every result is rounded like the terrain.
#[test]
fn modifications_follow_priority_and_round() {
    let dir = scratch();
    let g = grid(1000.0, 2000.0, 10.0, 20, 12);
    write_source(&dir, "a.tif", g, vec![50.0; 20 * 12]);
    write_mask(&dir, "a.u1", 20, 12, |_, _| true);
    // A SetValue pad (Priority 1) and a SetIfLower pad (Priority 0) over the
    // same cells: Priority 0 is applied last, so the lower value survives.
    let ring = json!([[
        [1040.0, 1920.0],
        [1120.0, 1920.0],
        [1120.0, 1980.0],
        [1040.0, 1980.0]
    ]]);
    let req = request(
        json!([{"path": "a.tif", "mask": mask("a.u1", 20, 12)}]),
        json!({
            "schema": "ras-commander-effective-terrain-request/2",
            "modifications": 2,
            "modification_layers": [
                {"kind": "polygon", "name": "cut", "priority": 0,
                 "features": [{"elevation_type": "SetIfLower", "elevation": 40.01, "rings": ring}]},
                {"kind": "polygon", "name": "pad", "priority": 1,
                 "features": [{"elevation_type": "SetValue", "elevation": 60.0, "rings": ring}]}
            ]
        }),
    );
    let out = effective_terrain::compute(&req, &dir).unwrap();
    // Pixel (row 5, col 6): centre (1065, 1945) is inside both pads.
    assert_eq!(out.values[5 * 20 + 6], quantize(40.01));
    assert_eq!(out.values[0], 50.0);
    let layers = out.report["modifications"]["layers"].as_array().unwrap();
    assert_eq!(layers[0]["name"], "pad");
    assert_eq!(layers[1]["name"], "cut");
    assert_eq!(out.report["method_id"], METHOD_ID);
}

/// Every accepted HEC-RAS release label gives the same pixels as its rule,
/// the report names both, and the method record advertises the table.
#[test]
fn hecras_release_labels_follow_their_rule() {
    let dir = scratch();
    let g = grid(1000.0, 2000.0, 10.0, 20, 12);
    write_source(&dir, "a.tif", g, vec![50.0; 20 * 12]);
    write_mask(&dir, "a.u1", 20, 12, |_, _| true);
    let ring = json!([[
        [1040.0, 1920.0],
        [1120.0, 1920.0],
        [1120.0, 1980.0],
        [1040.0, 1980.0]
    ]]);
    let build = |version: Option<&str>| {
        let mut extra = json!({
            "schema": "ras-commander-effective-terrain-request/2",
            "modifications": 1,
            "modification_layers": [{"kind": "polygon", "name": "pad", "priority": 0,
                "features": [{"elevation_type": "SetValue", "elevation": 60.0, "rings": ring}]}]
        });
        if let Some(v) = version {
            extra["hecras_version"] = json!(v);
        }
        let req = request(
            json!([{"path": "a.tif", "mask": mask("a.u1", 20, 12)}]),
            extra,
        );
        effective_terrain::compute(&req, &dir).unwrap()
    };
    let default = build(None);
    assert_eq!(default.report["modifications"]["hecras_rule"], "7.0.1");
    assert_eq!(
        default.report["modifications"]["hecras_version"],
        Value::Null
    );
    let record = effective_terrain::method_record();
    let table = record["hecras_versions"].as_object().unwrap();
    for (label, rule) in [
        ("6.6", "6.6"),
        ("6.7", "6.7"),
        ("6.7 Beta 4", "6.7"),
        ("6.7 Beta 5", "6.7"),
        ("7.0", "7.0.1"),
        ("7.0.0", "7.0.1"),
        ("7.0.1", "7.0.1"),
        ("7.0.2", "7.0.1"),
    ] {
        assert_eq!(table[label], rule, "{label}");
        let out = build(Some(label));
        assert_eq!(out.report["modifications"]["hecras_rule"], rule);
        assert_eq!(out.report["modifications"]["hecras_version"], label);
        assert!(
            out.values
                .iter()
                .zip(&default.values)
                .all(|(a, b)| a.to_bits() == b.to_bits())
        );
    }
    assert_eq!(table.len(), 8);
}

#[test]
fn unqualified_hecras_version_is_refused_before_reads() {
    // The source does not exist: the refusal must come before any read.
    let dir = scratch();
    let srcs = json!([{"path": "missing.tif", "mask": mask("m.u1", 10, 10)}]);
    for schema in [
        "ras-commander-effective-terrain-request/1",
        "ras-commander-effective-terrain-request/2",
    ] {
        for version in ["6.5", "6.4.1", "7.1", "7.0.3", "6.60", "latest"] {
            let req = request(
                srcs.clone(),
                json!({"schema": schema, "hecras_version": version}),
            );
            let err = effective_terrain::compute(&req, &dir).err().unwrap();
            assert_eq!(err.kind(), "unsupported", "{err:?}");
            let text = format!("{err:?}");
            assert!(
                text.contains("not a qualified HEC-RAS release") && text.contains(version),
                "{text}"
            );
        }
    }
}

#[test]
fn multi_part_polygon_is_refused_before_reads() {
    // The source does not exist: the refusal must come before any read.
    let dir = scratch();
    let srcs = json!([{"path": "missing.tif", "mask": mask("m.u1", 10, 10)}]);
    let outer = json!([[0.0, 0.0], [60.0, 0.0], [60.0, 60.0], [0.0, 60.0]]);
    let hole = json!([[20.0, 20.0], [20.0, 40.0], [40.0, 40.0], [40.0, 20.0]]);
    for version in ["7.0.1", "6.6", "6.7", "7.0.2"] {
        let req = request(
            srcs.clone(),
            json!({"schema": "ras-commander-effective-terrain-request/2", "modifications": 1,
                   "hecras_version": version, "modification_layers": [
                {"kind": "polygon", "name": "pad", "priority": 0,
                 "features": [{"elevation_type": "SetValue", "elevation": 1.0, "rings": [outer, hole]}]}]}),
        );
        let err = effective_terrain::compute(&req, &dir).err().unwrap();
        assert_eq!(err.kind(), "unsupported", "{err:?}");
        assert!(format!("{err:?}").contains("multi-part"), "{err:?}");
    }
}

#[test]
fn modification_count_must_match_and_v1_refuses_layers() {
    let dir = scratch();
    let srcs = json!([{"path": "missing.tif", "mask": mask("m.u1", 10, 10)}]);
    let layer = json!([{"kind": "ellipse", "name": "e", "priority": 0,
        "features": [{"elevation_type": "SetValue", "elevation": 1.0, "center": [0.0, 0.0], "radius_x": 1.0, "radius_y": 1.0}]}]);
    let mismatch = request(
        srcs.clone(),
        json!({"schema": "ras-commander-effective-terrain-request/2", "modifications": 2, "modification_layers": layer.clone()}),
    );
    assert_eq!(
        effective_terrain::compute(&mismatch, &dir)
            .err()
            .unwrap()
            .kind(),
        "invalid"
    );
    let v1 = request(
        srcs.clone(),
        json!({"modifications": 1, "modification_layers": layer}),
    );
    assert_eq!(
        effective_terrain::compute(&v1, &dir).err().unwrap().kind(),
        "unsupported"
    );
    let ground_line_set_value = request(
        srcs,
        json!({"schema": "ras-commander-effective-terrain-request/2", "modifications": 1, "modification_layers": [
            {"kind": "ground_line", "name": "g", "priority": 0, "subtype": "levee", "features": [
                {"elevation_type": "SetValue", "top_width": 2.0, "left_slope": 1.0, "right_slope": 1.0,
                 "max_reach": 10.0, "points": [[0.0, 0.0], [5.0, 0.0]], "profile": [[0.0, 1.0], [5.0, 1.0]]}]}]}),
    );
    assert_eq!(
        effective_terrain::compute(&ground_line_set_value, &dir)
            .err()
            .unwrap()
            .kind(),
        "unsupported"
    );
}
