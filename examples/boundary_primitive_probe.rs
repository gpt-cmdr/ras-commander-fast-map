//! Bounded synthetic contour diagnostics; no HEC/model dependencies or parity claim.
use clipper2_rust::{ClipType, Clipper64, FillRule, Paths64, Point64};
use ras_commander_fast_map::boundary_contour::{self as contour, Grid, Limits};
use ras_commander_fast_map::boundary_native_contour as native;
use serde::Deserialize;
use serde_json::json;
use std::{error::Error, fs::File, io::Read};

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
enum Request {
    Rings {
        rings: Vec<Vec<[i64; 2]>>,
    },
    Grids {
        grids: Vec<SampleGrid>,
        #[serde(default)]
        native_nodata: bool,
        #[serde(default)]
        native_serial: bool,
    },
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SampleGrid {
    rows: usize,
    cols: usize,
    left: f64,
    top: f64,
    dx: f64,
    dy: f64,
    values: Vec<f32>,
}

fn merge(paths: &Paths64, partition: bool) -> Result<Vec<Vec<[i64; 2]>>, Box<dyn Error>> {
    let mut clipper = Clipper64::new();
    clipper.set_preserve_collinear(false);
    if partition && !paths.is_empty() {
        clipper.add_subject(&vec![paths[0].clone()]);
        if paths.len() > 1 {
            clipper.add_clip(&paths[1..].to_vec());
        }
    } else {
        clipper.add_subject(paths);
    }
    let mut output = vec![];
    if !clipper.execute(ClipType::Union, FillRule::NonZero, &mut output, None) {
        return Err("Clipper2 primitive union failed".into());
    }
    Ok(output
        .into_iter()
        .map(|ring| ring.into_iter().map(|point| [point.x, point.y]).collect())
        .collect())
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 2 {
        return Err("usage: boundary_primitive_probe request.json".into());
    }
    let mut data = vec![];
    File::open(&args[1])?
        .take(2 * 1024 * 1024 + 1)
        .read_to_end(&mut data)?;
    if data.len() > 2 * 1024 * 1024 {
        return Err("primitive request exceeds 2 MiB".into());
    }
    let request: Request = serde_json::from_slice(&data)?;
    let mut result = match request {
        Request::Rings { rings } => {
            if rings.len() > 64
                || rings.iter().map(Vec::len).sum::<usize>() > 16384
                || rings.iter().any(|ring| {
                    ring.len() < 3 || ring.iter().flatten().any(|v| v.unsigned_abs() > 1 << 50)
                })
            {
                return Err("primitive rings exceed admitted dimensions/range".into());
            }
            let paths: Paths64 = rings
                .into_iter()
                .map(|ring| ring.into_iter().map(|[x, y]| Point64::new(x, y)).collect())
                .collect();
            json!({"kind":"rings", "partition_first_subject":merge(&paths,true)?,
                "all_subject":merge(&paths,false)?})
        }
        Request::Grids {
            grids,
            native_nodata,
            native_serial,
        } => {
            if grids.is_empty() || grids.len() > 4 {
                return Err("primitive grid count must be 1 through 4".into());
            }
            let limits = Limits {
                max_working_bytes: 64 * 1024 * 1024,
            };
            let mut units = vec![];
            let mut unit_outputs = vec![];
            for (index, grid) in grids.into_iter().enumerate() {
                if !(2..=33).contains(&grid.rows)
                    || !(2..=33).contains(&grid.cols)
                    || grid.values.len() != grid.rows * grid.cols
                    || [grid.left, grid.top, grid.dx, grid.dy]
                        .iter()
                        .any(|v| !v.is_finite() || v.abs() > (1_u64 << 40) as f64)
                    || grid.values.iter().any(|v| !v.is_finite())
                {
                    return Err("invalid primitive grid".into());
                }
                let depths: Vec<_> = grid
                    .values
                    .into_iter()
                    .map(|value| {
                        if native_nodata && value == -9999. {
                            None
                        } else {
                            Some(value)
                        }
                    })
                    .collect();
                let build = if native_serial {
                    native::grid
                } else {
                    contour::grid
                };
                let unit = build(
                    index as u64,
                    Grid {
                        cols: grid.cols,
                        rows: grid.rows,
                        transform: [grid.left, grid.dx, 0., grid.top, 0., grid.dy],
                    },
                    &depths,
                    limits,
                )?;
                let finish = if native_serial {
                    native::finish
                } else {
                    contour::finish
                };
                let individual = finish(std::slice::from_ref(&unit), None, limits)?;
                unit_outputs.push(json!({"sequence":index,"coordinates":individual.coordinates}));
                units.push(unit);
            }
            let finish = if native_serial {
                native::finish
            } else {
                contour::finish
            };
            let output = finish(&units, None, limits)?;
            json!({"kind":"grids", "native_nodata":native_nodata,
                "native_serial":native_serial, "method_id":output.method_id, "units":unit_outputs,"coordinates":output.coordinates})
        }
    };
    result["schema"] = json!("fast-map-boundary-primitive-probe/1");
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}
