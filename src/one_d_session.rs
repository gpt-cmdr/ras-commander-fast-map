//! Bounded diagnostic 1D CLI transport. Terrain is an explicitly prepared,
//! aligned Float32 buffer; GIS delivery and HDF extraction belong to Commander.
use crate::{
    MapError,
    one_d::{PreparedSurface, Segment},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    fs::{self, File},
    io::{BufReader, Read},
    path::{Path, PathBuf},
};

pub const SCHEMA: &str = "fast-map-one-d-diagnostic/1";
pub const NATIVE_SCHEMA: &str = "fast-map-one-d-diagnostic/2";
pub const CLIPPED_SCHEMA: &str = "fast-map-one-d-diagnostic/3";
pub const CLIPPING_SCOPE: &str = "levees-and-xs-end-obstructions-only";

#[derive(Deserialize, Clone, Copy)]
#[serde(deny_unknown_fields)]
pub struct Window {
    pub col: usize,
    pub row: usize,
    pub width: usize,
    pub height: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub schema: String,
    /// Must explicitly acknowledge that only the XS surface is rendered.
    pub scope: String,
    pub points_file: PathBuf,
    pub point_count: usize,
    pub triangles_file: PathBuf,
    pub triangle_count: usize,
    pub tin_info_file: PathBuf,
    pub xs_ids_file: PathBuf,
    pub segment_count: usize,
    pub cross_section_count: usize,
    pub levels_file: PathBuf,
    pub terrain_file: PathBuf,
    pub width: usize,
    pub height: usize,
    /// Same-CRS north-up pixel-corner geotransform; no reprojection.
    pub geo_transform: [f64; 6],
    pub max_working_bytes: usize,
    /// Schema /2 requires one little-endian UInt32 rank per stored segment.
    #[serde(default)]
    pub render_order_file: Option<PathBuf>,
    /// Source terrain TIFF tile size; changes native scanline operation order.
    #[serde(default)]
    pub tile_size: Option<usize>,
    /// Schema /2 crop on the original global terrain lattice, not a new origin.
    #[serde(default)]
    pub window: Option<Window>,
    /// Schema /3: caller-normalized Float64 strip M, one per global TIN point.
    #[serde(default)]
    pub normalized_m_file: Option<PathBuf>,
    /// Schema /3: eight Float32 fields per XS; inactive barriers are paired NaNs.
    #[serde(default)]
    pub xs_clip_file: Option<PathBuf>,
    /// Explicitly excludes unsupported encroachment and blocked-polygon mapping.
    #[serde(default)]
    pub clipping_scope: Option<String>,
}

pub(crate) fn read_records<const N: usize, T>(
    path: &Path,
    count: usize,
    decode: impl Fn([u8; N]) -> T,
) -> Result<Vec<T>, MapError> {
    let expected = count
        .checked_mul(N)
        .ok_or_else(|| MapError::Budget("1D payload size overflow".into()))?;
    let file = File::open(path)?;
    if file.metadata()?.len() != expected as u64 {
        return Err(MapError::Invalid(format!(
            "{}: expected {expected} bytes",
            path.display()
        )));
    }
    let mut data = Vec::new();
    data.try_reserve_exact(count)
        .map_err(|e| MapError::Budget(e.to_string()))?;
    let mut reader = BufReader::new(file);
    for _ in 0..count {
        let mut bytes = [0; N];
        reader.read_exact(&mut bytes)?;
        data.push(decode(bytes));
    }
    if reader.read(&mut [0])? != 0 {
        return Err(MapError::Invalid("1D payload grew during read".into()));
    }
    Ok(data)
}

fn nonnegative(value: i32) -> Result<usize, MapError> {
    usize::try_from(value)
        .map_err(|_| MapError::Invalid("1D negative stored range or XS ID".into()))
}

pub fn run(req: &Request, base: &Path, output: &Path) -> Result<Value, MapError> {
    let clipped = req.schema == CLIPPED_SCHEMA;
    let native = req.schema == NATIVE_SCHEMA || clipped;
    if !matches!(req.schema.as_str(), SCHEMA | NATIVE_SCHEMA | CLIPPED_SCHEMA)
        || req.scope != "xs-surface-only"
    {
        return Err(MapError::Unsupported(
            "expected diagnostic 1D schema and xs-surface-only scope".into(),
        ));
    }
    if native != (req.render_order_file.is_some() && req.tile_size.is_some())
        || (!native
            && (req.render_order_file.is_some() || req.tile_size.is_some() || req.window.is_some()))
    {
        return Err(MapError::Invalid(
            "1D schema /2 requires ranks and tile size; /1 excludes native options".into(),
        ));
    }
    if (clipped
        && (req.normalized_m_file.is_none()
            || req.xs_clip_file.is_none()
            || req.clipping_scope.as_deref() != Some(CLIPPING_SCOPE)))
        || (!clipped
            && (req.normalized_m_file.is_some()
                || req.xs_clip_file.is_some()
                || req.clipping_scope.is_some()))
    {
        return Err(MapError::Invalid("1D schema /3 requires normalized M, XS clips and explicit clipping scope; older schemas exclude clipping".into()));
    }
    let gt = req.geo_transform;
    let window = req.window.unwrap_or(Window {
        col: 0,
        row: 0,
        width: req.width,
        height: req.height,
    });
    if window.width == 0
        || window.height == 0
        || window
            .col
            .checked_add(window.width)
            .is_none_or(|v| v > req.width)
        || window
            .row
            .checked_add(window.height)
            .is_none_or(|v| v > req.height)
        || (native
            && (req.tile_size == Some(0)
                || req.tile_size.is_some_and(|v| v > 4096)
                || gt[1] != -gt[5]))
    {
        return Err(MapError::Invalid(
            "invalid native 1D source tile grid or crop window".into(),
        ));
    }
    if !gt.iter().all(|v| v.is_finite())
        || gt[1] <= 0.
        || gt[5] >= 0.
        || gt[2] != 0.
        || gt[4] != 0.
        || req.width == 0
        || req.height == 0
        || req.cross_section_count < 2
        || req.segment_count == 0
    {
        return Err(MapError::Invalid("invalid 1D grid or counts".into()));
    }
    let pixels = window
        .width
        .checked_mul(window.height)
        .ok_or_else(|| MapError::Budget("1D grid overflow".into()))?;
    if !(gt[0] + req.width as f64 * gt[1]).is_finite()
        || !(gt[3] + req.height as f64 * gt[5]).is_finite()
    {
        return Err(MapError::Invalid("1D grid coordinates overflow".into()));
    }
    if let Some(tile) = req.tile_size
        && (req.width.div_ceil(tile).checked_mul(tile).is_none()
            || req.height.div_ceil(tile).checked_mul(tile).is_none())
    {
        return Err(MapError::Budget(
            "native XS padded source grid overflow".into(),
        ));
    }
    let geometry = if clipped {
        crate::one_d_native::PreparedSurface::planned_clipped_bytes(
            req.point_count,
            req.triangle_count,
            req.segment_count,
            req.cross_section_count,
            req.tile_size.unwrap(),
        )?
    } else if native {
        crate::one_d_native::PreparedSurface::planned_bytes(
            req.point_count,
            req.triangle_count,
            req.segment_count,
            req.tile_size.unwrap(),
        )?
    } else {
        PreparedSurface::planned_bytes(req.point_count, req.triangle_count, req.segment_count)?
    };
    let planned = pixels
        .checked_mul(12)
        .and_then(|v| {
            req.cross_section_count
                .checked_mul(4)
                .and_then(|n| v.checked_add(n))
        })
        .and_then(|v| {
            req.segment_count
                .checked_mul(if native { 40 } else { 24 })
                .and_then(|n| v.checked_add(n))
        })
        .and_then(|v| v.checked_add(geometry))
        // Float32 conversion scratch, BufWriter, and bounded report overhead.
        .and_then(|v| v.checked_add(65536 + 8192 + 65536))
        .ok_or_else(|| MapError::Budget("1D request allocation overflow".into()))?;
    if planned > req.max_working_bytes {
        return Err(MapError::Budget(format!(
            "1D planned {planned} bytes exceeds {}",
            req.max_working_bytes
        )));
    }
    // No payload reads precede admission. Numeric records are little-endian.
    let points = read_records(
        &base.join(&req.points_file),
        req.point_count,
        |b: [u8; 32]| -> [f64; 4] {
            std::array::from_fn(|i| f64::from_le_bytes(b.as_chunks::<8>().0[i]))
        },
    )?;
    let triangles = read_records(
        &base.join(&req.triangles_file),
        req.triangle_count,
        |b: [u8; 12]| -> [i32; 3] {
            std::array::from_fn(|i| i32::from_le_bytes(b.as_chunks::<4>().0[i]))
        },
    )?;
    let info = read_records(
        &base.join(&req.tin_info_file),
        req.segment_count,
        |b: [u8; 16]| -> [i32; 4] {
            std::array::from_fn(|i| i32::from_le_bytes(b.as_chunks::<4>().0[i]))
        },
    )?;
    let ids = read_records(
        &base.join(&req.xs_ids_file),
        req.segment_count,
        |b: [u8; 8]| -> [i32; 2] {
            std::array::from_fn(|i| i32::from_le_bytes(b.as_chunks::<4>().0[i]))
        },
    )?;
    let segments = info
        .iter()
        .zip(&ids)
        .map(|(r, xs)| {
            Ok(Segment {
                point_start: nonnegative(r[0])?,
                point_count: nonnegative(r[1])?,
                triangle_start: nonnegative(r[2])?,
                triangle_count: nonnegative(r[3])?,
                upstream: nonnegative(xs[0])?,
                downstream: nonnegative(xs[1])?,
            })
        })
        .collect::<Result<Vec<_>, MapError>>()?;
    let levels = read_records(
        &base.join(&req.levels_file),
        req.cross_section_count,
        f32::from_le_bytes,
    )?;
    let terrain = read_records(&base.join(&req.terrain_file), pixels, f32::from_le_bytes)?;
    if terrain.iter().any(|v| v.is_infinite()) {
        return Err(MapError::Invalid("1D terrain must be finite or NaN".into()));
    }
    let mut depth = Vec::new();
    let mut wse = Vec::new();
    depth
        .try_reserve_exact(pixels)
        .map_err(|e| MapError::Budget(e.to_string()))?;
    wse.try_reserve_exact(pixels)
        .map_err(|e| MapError::Budget(e.to_string()))?;
    depth.resize(pixels, f32::NAN);
    wse.resize(pixels, f32::NAN);
    let valid = if native {
        let ranks = read_records(
            &base.join(req.render_order_file.as_ref().unwrap()),
            req.segment_count,
            u32::from_le_bytes,
        )?;
        let surface = if clipped {
            let normalized_m = read_records(
                &base.join(req.normalized_m_file.as_ref().unwrap()),
                req.point_count,
                f64::from_le_bytes,
            )?;
            let rows = read_records(
                &base.join(req.xs_clip_file.as_ref().unwrap()),
                req.cross_section_count,
                |b: [u8; 32]| -> [f32; 8] {
                    std::array::from_fn(|i| f32::from_le_bytes(b.as_chunks::<4>().0[i]))
                },
            )?;
            let mut clips = Vec::new();
            clips
                .try_reserve_exact(req.cross_section_count)
                .map_err(|e| MapError::Budget(e.to_string()))?;
            for row in rows {
                clips.push(crate::one_d_clip::decode(row)?);
            }
            crate::one_d_native::PreparedSurface::new_with_clipping(
                &points,
                &triangles,
                &segments,
                req.cross_section_count,
                &ranks,
                &normalized_m,
                &clips,
                geometry,
            )?
        } else {
            crate::one_d_native::PreparedSurface::new(
                &points,
                &triangles,
                &segments,
                req.cross_section_count,
                &ranks,
                geometry,
            )?
        };
        surface.render_into(
            &levels,
            &terrain,
            crate::one_d_native::Grid {
                width: req.width,
                height: req.height,
                geo_transform: gt,
                tile_size: req.tile_size.unwrap(),
            },
            crate::one_d_native::Window {
                col: window.col,
                row: window.row,
                width: window.width,
                height: window.height,
            },
            &mut depth,
            &mut wse,
        )?
    } else {
        let surface = PreparedSurface::new(
            &points,
            &triangles,
            &segments,
            req.cross_section_count,
            geometry,
        )?;
        let profile = surface.profile(&levels)?;
        let mut valid = 0;
        for (i, ground) in terrain.iter().enumerate() {
            let point = [
                gt[0] + (i % req.width) as f64 * gt[1] + 0.5 * gt[1],
                gt[3] + (i / req.width) as f64 * gt[5] + 0.5 * gt[5],
            ];
            let (d, z) = profile
                .sample(point, *ground)
                .unwrap_or((f32::NAN, f32::NAN));
            valid += usize::from(d.is_finite());
            depth[i] = d;
            wse[i] = z;
        }
        valid
    };
    let output_gt = [
        gt[0] + window.col as f64 * gt[1],
        gt[1],
        0.,
        gt[3] + window.row as f64 * gt[5],
        0.,
        gt[5],
    ];
    fs::create_dir(output)?;
    let mut report = json!({"schema": req.schema, "method": if clipped {crate::one_d_native::clipped_method_record()} else if native { crate::one_d_native::method_record() } else { crate::one_d::method_record() },
        "status": "complete", "scope": req.scope, "width": window.width, "height": window.height,
        "geo_transform": output_gt, "source_width": req.width, "source_height": req.height,
        "source_geo_transform":gt, "source_col":window.col, "source_row":window.row, "tile_size":req.tile_size, "clipping_scope":req.clipping_scope, "valid_pixels": valid, "planned_bytes": planned,
        "depth_file": "depth.bin", "wse_file": "wse.bin", "dtype": "little-endian float32", "nodata": "NaN"});
    let result = (|| {
        crate::horizontal::write_values(&output.join("depth.bin"), &depth)?;
        crate::horizontal::write_values(&output.join("wse.bin"), &wse)?;
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
