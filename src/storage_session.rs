//! Bounded storage-only transport for the existing level-pool kernel.
use crate::{MapError, storage_area};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use terrio::ops::rasterize::Grid;

pub const SCHEMA: &str = "fast-map-storage/1";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub schema: String,
    pub storage_areas_file: PathBuf,
    pub storage_area_count: usize,
    pub levels_file: PathBuf,
    /// Little-endian float32 window terrain; NaN denotes nodata.
    pub terrain_file: PathBuf,
    pub width: usize,
    pub height: usize,
    pub geo_transform: [f64; 6],
    pub max_working_bytes: usize,
    pub threads: usize,
}

pub fn method_record() -> Value {
    let mut record = storage_area::method_record();
    record["native_parity_qualified"] = json!(false);
    record["scope"] = json!(
        "storage-only prepared aligned terrain buffers; no 2D cells required; transport qualification pending"
    );
    record["ownership"] = json!(
        "terrio pixel-centre polygons; physical labels retained for dry and terrain-nodata pixels"
    );
    record
}

pub fn run(req: &Request, base: &Path, output: &Path) -> Result<Value, MapError> {
    if req.schema != SCHEMA {
        return Err(MapError::Unsupported("expected fast-map-storage/1".into()));
    }
    let gt = req.geo_transform;
    if req.width == 0
        || req.height == 0
        || !(1..=4).contains(&req.threads)
        || req.storage_area_count == 0
        || req.storage_area_count >= u32::MAX as usize
        || !gt.iter().all(|v| v.is_finite())
        || gt[1] <= 0.
        || gt[5] >= 0.
        || gt[2] != 0.
        || gt[4] != 0.
        || !(gt[0] + req.width as f64 * gt[1]).is_finite()
        || !(gt[3] + req.height as f64 * gt[5]).is_finite()
    {
        return Err(MapError::Invalid(
            "invalid storage grid, area count or thread budget".into(),
        ));
    }
    if output.exists() {
        return Err(MapError::Invalid(
            "storage output directory already exists".into(),
        ));
    }
    let n = req
        .width
        .checked_mul(req.height)
        .ok_or_else(|| MapError::Budget("storage grid overflow".into()))?;
    let polygons = base.join(&req.storage_areas_file);
    let polygon_bytes = usize::try_from(fs::metadata(&polygons)?.len())
        .map_err(|_| MapError::Budget("storage polygon size overflow".into()))?;
    // Includes both geometry decodes, feature/ring metadata, rasterizer buffers,
    // labels, terrain, sparse pixel/label/ground vectors, products and IO scratch.
    // This is allocation admission, not a promise of a hard process RSS limit.
    let planned = n
        .checked_mul(128)
        .and_then(|v| polygon_bytes.checked_mul(32).and_then(|p| v.checked_add(p)))
        .and_then(|v| {
            req.storage_area_count
                .checked_mul(4096)
                .and_then(|p| v.checked_add(p))
        })
        .and_then(|v| v.checked_add(1024 * 1024))
        .ok_or_else(|| MapError::Budget("storage allocation plan overflow".into()))?;
    if planned > req.max_working_bytes {
        return Err(MapError::Budget(format!(
            "storage planned {planned} bytes exceeds budget"
        )));
    }
    let levels = storage_area::values(&base.join(&req.levels_file), req.storage_area_count)?;
    let terrain =
        crate::one_d_session::read_records(&base.join(&req.terrain_file), n, f32::from_le_bytes)?;
    if terrain.iter().any(|v| v.is_infinite()) {
        return Err(MapError::Invalid("infinite storage terrain".into()));
    }
    let mut owners = vec![0_u32; n];
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(req.threads)
        .build()
        .map_err(|e| MapError::Internal(e.to_string()))?;
    let grid = Grid {
        geo_transform: gt,
        cols: req.width,
        rows: req.height,
        crs: Default::default(),
    };
    let mut pixels = storage_area::prepare(
        &polygons,
        req.storage_area_count,
        &grid,
        &owners,
        None,
        &pool,
    )?;
    for ((&p, &label), ground) in pixels
        .pixels
        .iter()
        .zip(&pixels.labels)
        .zip(&mut pixels.ground)
    {
        owners[p] = label + 1;
        *ground = terrain[p];
    }
    let mut depth = vec![f32::NAN; n];
    let mut wse = vec![f32::NAN; n];
    let valid = storage_area::apply(&pixels, &levels, 0., &mut depth, &mut wse)?;
    let mut report = json!({"schema": SCHEMA, "method": method_record(), "status": "complete",
        "width": req.width, "height": req.height, "geo_transform": gt,
        "planned_bytes": planned, "threads": req.threads,
        "storage_owned_pixels": pixels.pixels.len(), "valid_pixels": valid,
        "depth_file": "depth.bin", "wse_file": "wse.bin", "owners_file": "owners.bin",
        "dtype": "little-endian float32", "owners_dtype": "little-endian uint32", "nodata": "NaN",
        "terrio_pool": crate::terrio_pool::status()});
    fs::create_dir(output)?;
    let result = (|| {
        crate::horizontal::write_values(&output.join("depth.bin"), &depth)?;
        crate::horizontal::write_values(&output.join("wse.bin"), &wse)?;
        let mut file = std::io::BufWriter::new(fs::File::create(output.join("owners.bin"))?);
        for owner in owners {
            file.write_all(&owner.to_le_bytes())?;
        }
        file.flush()?;
        Ok::<_, MapError>(())
    })();
    if let Err(e) = &result {
        report["status"] = json!("aborted");
        report["error"] = json!(e.to_string());
    }
    fs::write(
        output.join("report.json"),
        serde_json::to_vec_pretty(&report).map_err(|e| MapError::Internal(e.to_string()))?,
    )?;
    result?;
    Ok(report)
}
