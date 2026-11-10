//! Write one raw Float32 product window as a final delivery GeoTIFF.
//! Run with: cargo run --release --example final_geotiff -- request.json
//! The request names a little-endian Float32 row-major file (NaN marks invalid
//! pixels), its window shape and geotransform, an EPSG code, a thread count and
//! the `final_format` wire object. It is a writer probe, not a mapping request.

use ras_commander_fast_map::{
    final_tiff::{Layout, Writer},
    native_output::FinalFormat,
};
use serde::Deserialize;
use std::{error::Error, fs, path::PathBuf, time::Instant};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    values_file: PathBuf,
    output: PathBuf,
    window_width: usize,
    window_height: usize,
    window_transform: [f64; 6],
    epsg: u32,
    threads: usize,
    /// Rows per `write_rows` call; zero passes one tile row at a time.
    #[serde(default)]
    rows_per_call: usize,
    final_format: FinalFormat,
}

fn main() -> Result<(), Box<dyn Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: final_geotiff request.json")?;
    let req: Request = serde_json::from_slice(&fs::read(path)?)?;
    let layout = Layout::new(
        &req.final_format,
        req.window_width,
        req.window_height,
        req.window_transform,
    )?;
    let raw = fs::read(&req.values_file)?;
    if raw.len() != req.window_width * req.window_height * 4 {
        return Err("values_file length does not match the window".into());
    }
    let values: Vec<f32> = raw
        .as_chunks::<4>()
        .0
        .iter()
        .map(|v| f32::from_le_bytes(*v))
        .collect();
    drop(raw);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(req.threads)
        .build()?;
    let rows = if req.rows_per_call == 0 {
        layout.block
    } else {
        req.rows_per_call
    };
    let tick = Instant::now();
    let mut writer = Writer::create(&req.output, &layout, req.epsg)?;
    for chunk in values.chunks(req.window_width * rows) {
        writer.write_rows(chunk, &pool)?;
    }
    let summary = writer.finish()?;
    println!(
        "{}",
        serde_json::json!({"write_seconds":tick.elapsed().as_secs_f64(),"threads":req.threads,
            "planned_bytes":layout.planned_bytes(req.threads),
            "valid_pixels":summary.valid_pixels,"tiles":summary.tiles,
            "shared_nodata_tiles":summary.shared_nodata_tiles,"file_bytes":summary.file_bytes})
    );
    Ok(())
}
