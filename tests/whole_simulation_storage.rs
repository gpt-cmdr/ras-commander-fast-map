//! Whole-simulation products with a storage-area level pool, end to end through
//! `run()`: the pool pixels' arrival, duration and percent equal those of a 2D
//! cell that carries the pool's series (ungated), bit for bit.
use ras_commander_fast_map::{
    effective_terrain::{self, Effective, Grid},
    whole_simulation::{self, Request},
};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

static NEXT: AtomicUsize = AtomicUsize::new(0);
const W: usize = 32;

fn scratch() -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "fast-map-ws-storage-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&p).unwrap();
    p
}

fn rectangle(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<u8> {
    let mut wkb = vec![1];
    for i in [3u32, 1, 5] {
        wkb.extend(i.to_le_bytes());
    }
    for p in [[x0, y0], [x0, y1], [x1, y1], [x1, y0], [x0, y0]] {
        for v in p {
            wkb.extend(v.to_le_bytes());
        }
    }
    let mut framed = (wkb.len() as u32).to_le_bytes().to_vec();
    framed.extend(wkb);
    framed
}

fn f32s(dir: &Path, name: &str, values: &[f32]) -> String {
    fs::write(
        dir.join(name),
        values
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect::<Vec<_>>(),
    )
    .unwrap();
    name.into()
}

/// Ground rises 0.25 per column; the right half is either a storage area
/// (`pool`) or a second 2D cell carrying the same series.
fn request(dir: &Path, pool: bool, series: &[f32], products: &[&str]) -> Request {
    let grid = Grid {
        origin_x: 0.0,
        origin_y: W as f64,
        cell_size: 1.0,
        width: W,
        height: W,
    };
    let values = (0..W * W).map(|i| (i % W) as f32 * 0.25).collect();
    effective_terrain::write_geotiff(
        &dir.join("terrain.tif"),
        Effective {
            grid,
            values,
            report: Value::Null,
        },
        26915,
    )
    .unwrap();
    let half = W as f64 / 2.0;
    let mut cells = rectangle(0.0, 0.0, half, W as f64);
    if !pool {
        cells.extend(rectangle(half, 0.0, W as f64, W as f64));
    }
    fs::write(dir.join("cells.wkb"), cells).unwrap();
    fs::write(
        dir.join("areas.wkb"),
        rectangle(half, 0.0, W as f64, W as f64),
    )
    .unwrap();
    let cell_count = if pool { 1 } else { 2 };
    let profiles: Vec<Value> = series
        .iter()
        .enumerate()
        .map(|(k, &v)| {
            // The left cell is always deep; the right half follows `series`.
            let cell_values: Vec<f32> = if pool { vec![20.0] } else { vec![20.0, v] };
            let mut p = json!({"id": format!("t{k}"),
                "values_file": f32s(dir, &format!("c{k}.f32"), &cell_values)});
            if pool {
                p["storage_area_values_file"] = f32s(dir, &format!("s{k}.f32"), &[v]).into();
            }
            p
        })
        .collect();
    let minimum: Vec<f32> = vec![-100.0; cell_count];
    let mut horizontal = json!({"schema": "ras2fim-horizontal-request/1", "terrain": "terrain.tif",
        "epsg": 26915, "crs_identity": {"kind": "epsg"}, "terrain_transform": [0., 1., 0., W as f64, 0., -1.],
        "window": [0, 0, W, W], "geometry_file": "cells.wkb", "cell_count": cell_count,
        "profiles": profiles, "min_depth": 0.0, "threads": 2, "max_working_bytes": 1u64 << 28});
    if pool {
        horizontal["storage_areas_file"] = "areas.wkb".into();
        horizontal["storage_area_count"] = 1.into();
    }
    serde_json::from_value(
        json!({"schema": whole_simulation::SCHEMA, "horizontal": horizontal,
        "times_hours": (0..series.len()).map(|k| k as f64 * 0.5).collect::<Vec<_>>(),
        "threshold": 0.0, "cell_minimum_file": f32s(dir, "minimum.f32", &minimum),
        "products": products}),
    )
    .unwrap()
}

fn product(out: &Path, name: &str) -> Vec<u32> {
    fs::read(out.join("whole_simulation").join(format!("{name}.bin")))
        .unwrap()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| u32::from_le_bytes(*b))
        .collect()
}

#[test]
fn pool_pixels_match_a_cell_with_the_pool_series() {
    // Rises to 7.5 and drains: each column (ground 4..7.75) wets and dries at
    // its own time; some dry inside a falling window, some at its end.
    let series = [4.0_f32, 5.5, 7.0, 7.5, 7.2, 6.4, 5.1, 4.3, 3.0];
    let products = ["arrival_time", "duration", "percent_inundated"];
    let (dp, dc) = (scratch(), scratch());
    whole_simulation::run(
        &request(&dp, true, &series, &products),
        &dp,
        &dp.join("out"),
    )
    .unwrap();
    whole_simulation::run(
        &request(&dc, false, &series, &products),
        &dc,
        &dc.join("out"),
    )
    .unwrap();
    let mut wet = 0;
    for name in products {
        let (p, c) = (
            product(&dp.join("out"), name),
            product(&dc.join("out"), name),
        );
        for row in 0..W {
            for col in W / 2..W {
                let i = row * W + col;
                assert_eq!(p[i], c[i], "{name} at ({row}, {col})");
                wet += usize::from(name == "duration" && !f32::from_bits(p[i]).is_nan());
            }
        }
    }
    // Most columns of the pool wet; durations end at different times.
    assert!(wet > W * 8, "{wet}");
    let durations: std::collections::BTreeSet<u32> = product(&dp.join("out"), "duration")[W / 2..W]
        .iter()
        .copied()
        .filter(|b| !f32::from_bits(*b).is_nan())
        .collect();
    assert!(durations.len() > 2, "{durations:?}");
}

#[test]
fn pool_extrema_are_planned() {
    // Duration with a pool is admitted against the budget before any output:
    // a budget below the plan refuses with no output folder.
    let series = [4.0_f32, 5.5, 7.0, 7.5, 7.2, 6.4, 5.1, 4.3, 3.0];
    let dir = scratch();
    let mut req = request(&dir, true, &series, &["duration"]);
    req.horizontal.max_working_bytes = 1;
    let err = whole_simulation::run(&req, &dir, &dir.join("out")).unwrap_err();
    assert!(err.to_string().contains("budget"), "{err}");
    assert!(!dir.join("out").exists());
}
