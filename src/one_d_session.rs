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
    time::Instant,
};

pub const SCHEMA: &str = "fast-map-one-d-diagnostic/1";
pub const NATIVE_SCHEMA: &str = "fast-map-one-d-diagnostic/2";
pub const CLIPPED_SCHEMA: &str = "fast-map-one-d-diagnostic/3";
pub const STORED_GRID_SCHEMA: &str = "fast-map-one-d-diagnostic/4";
pub const CLIPPING_SCOPE: &str = "levees-and-xs-end-obstructions-only";
pub const MAX_PROFILES: usize = 256;

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
    #[serde(default)]
    pub grid_contract: Option<crate::stored_grid::Contract>,
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
    /// One profile's XS levels. Exactly one of `levels_file` and `levels_files`.
    #[serde(default)]
    pub levels_file: Option<PathBuf>,
    /// Schemas /2-/4: up to 256 profiles sharing one read and surface prepare;
    /// profile `i` is written to `profile-{i:04}/` with its own report.
    #[serde(default)]
    pub levels_files: Option<Vec<PathBuf>>,
    /// Caller-staged little-endian Float32 window terrain (NaN is nodata).
    /// Exactly one of `terrain_file` and `terrain_tiff`.
    #[serde(default)]
    pub terrain_file: Option<PathBuf>,
    /// Schemas /2-/4: the source GeoTIFF of the request grid, read with terrio.
    #[serde(default)]
    pub terrain_tiff: Option<PathBuf>,
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
    /// Optional render workers for schemas /2-/4 (default 1). Source tile rows
    /// render independently, so outputs are identical for every worker count.
    #[serde(default)]
    pub threads: Option<usize>,
    /// Opt-in engine-written delivery GeoTIFFs (schemas /2-/4); see `one_d_delivery`.
    #[serde(default)]
    pub final_format: Option<crate::native_output::FinalFormat>,
    #[serde(default)]
    pub epsg: Option<u32>,
    #[serde(default)]
    pub aoi_mask_file: Option<PathBuf>,
    #[serde(default)]
    pub flooded_depth_range: Option<[f32; 2]>,
    /// Schemas /2-/3: storage level pools composed in process (see `InlineStorage`).
    #[serde(default)]
    pub storage: Option<InlineStorage>,
}

/// In-process equivalent of `storage` + `coupled-diagnostic` /2 with zero 2D
/// cells: physical storage labels are rasterized once on the window grid, then
/// every profile's level pools and composition use the unchanged kernels.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InlineStorage {
    pub storage_areas_file: PathBuf,
    pub storage_area_count: usize,
    /// One finite Float32 WSE file per profile, in profile order.
    pub levels_files: Vec<PathBuf>,
    /// Window geotransform; must equal the 1D output window exactly.
    pub geo_transform: [f64; 6],
}

pub fn terrain_capability() -> Value {
    json!({"control": "terrain_tiff", "reader": "terrio", "schemas": [NATIVE_SCHEMA, CLIPPED_SCHEMA, STORED_GRID_SCHEMA],
        "source": "single-band Float32 tiled GeoTIFF with the request width, height and geotransform",
        "nodata": "GDAL Float32 nodata mask (ARE_REAL_EQUAL) to NaN; other values keep their bits",
        "equivalent": "masked GDAL window read of the same file, as caller-staged terrain_file"})
}

pub fn storage_capability() -> Value {
    json!({"control": "storage", "schemas": [NATIVE_SCHEMA, CLIPPED_SCHEMA],
        "storage_method_id": crate::storage_area::METHOD_ID,
        "composition_method_id": crate::coupled::STORAGE_METHOD_ID,
        "equivalent": "storage /1 then coupled-diagnostic /2 with zero 2D cells; labels rasterized once",
        "outputs": "composed Depth/WSE bit-identical to the three-call route"})
}

impl Request {
    fn delivery(&self) -> crate::one_d_delivery::Options {
        crate::one_d_delivery::Options {
            final_format: self.final_format.clone(),
            epsg: self.epsg,
            aoi_mask_file: self.aoi_mask_file.clone(),
            flooded_depth_range: self.flooded_depth_range,
        }
    }
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
    let started = Instant::now();
    let stored_grid = req.schema == STORED_GRID_SCHEMA;
    let clipped = req.schema == CLIPPED_SCHEMA || stored_grid;
    let native = req.schema == NATIVE_SCHEMA || clipped;
    if !matches!(
        req.schema.as_str(),
        SCHEMA | NATIVE_SCHEMA | CLIPPED_SCHEMA | STORED_GRID_SCHEMA
    ) || req.scope != "xs-surface-only"
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
    let level_paths: Vec<&PathBuf> = match (&req.levels_file, &req.levels_files) {
        (Some(one), None) => vec![one],
        (None, Some(many)) if native && !many.is_empty() && many.len() <= MAX_PROFILES => {
            many.iter().collect()
        }
        _ => {
            return Err(MapError::Invalid(format!(
                "1D requires exactly one of levels_file or levels_files (1-{MAX_PROFILES}; schemas /2-/4)"
            )));
        }
    };
    let multi = req.levels_files.is_some();
    if req.terrain_file.is_some() == req.terrain_tiff.is_some()
        || (req.terrain_tiff.is_some() && !native)
    {
        return Err(MapError::Invalid(
            "1D requires exactly one of terrain_file or terrain_tiff (schemas /2-/4)".into(),
        ));
    }
    if let Some(storage) = &req.storage
        && (stored_grid
            || !native
            || storage.storage_area_count == 0
            || storage.storage_area_count >= u32::MAX as usize
            || storage.levels_files.len() != level_paths.len())
    {
        return Err(MapError::Invalid(
            "inline storage needs schema /2 or /3, a valid area count and one levels file per profile".into(),
        ));
    }
    let threads = req.threads.unwrap_or(1);
    if threads == 0 || threads > 64 || (!native && threads != 1) {
        return Err(MapError::Invalid(
            "1D threads must be 1-64, and 1 for schema /1".into(),
        ));
    }
    let gt = req.geo_transform;
    let window = req.window.unwrap_or(Window {
        col: 0,
        row: 0,
        width: req.width,
        height: req.height,
    });
    let layout = match (stored_grid, req.grid_contract.as_ref()) {
        (false, None) => None,
        (true, Some(contract)) => {
            if contract.source_width != req.width
                || contract.source_height != req.height
                || !crate::stored_grid::same(contract.source_geo_transform, gt)
                || req.tile_size != Some(256)
            {
                return Err(MapError::Invalid(
                    "XS stored grid descriptor does not match original source".into(),
                ));
            }
            Some(contract.validate([window.col, window.row, window.width, window.height])?)
        }
        _ => {
            return Err(MapError::Unsupported(
                "XS schema and stored grid contract must be selected together".into(),
            ));
        }
    };
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
    let output_gt = layout
        .map(|l| l.window_transform([window.col, window.row, window.width, window.height]))
        .unwrap_or([
            gt[0] + window.col as f64 * gt[1],
            gt[1],
            0.,
            gt[3] + window.row as f64 * gt[5],
            0.,
            gt[5],
        ]);
    if !native && req.delivery().requested() {
        return Err(MapError::Unsupported(
            "1D final_format requires schemas /2-/4".into(),
        ));
    }
    let delivery = req
        .delivery()
        .plan(window.width, window.height, output_gt)?;
    let delivery_bytes = match &delivery {
        Some(plan) => plan.planned_bytes(threads)?,
        None => 0,
    };
    // Each extra render worker: tile claim bytes plus triangle candidate references.
    let worker_extra = req
        .tile_size
        .unwrap_or(0)
        .checked_mul(req.tile_size.unwrap_or(0))
        .and_then(|n| {
            req.triangle_count
                .checked_mul(std::mem::size_of::<usize>())
                .and_then(|m| n.checked_add(m))
        })
        .and_then(|n| n.checked_mul(threads - 1))
        .ok_or_else(|| MapError::Budget("1D worker scratch overflow".into()))?;
    let surface_budget = geometry
        .checked_add(worker_extra)
        .ok_or_else(|| MapError::Budget("1D worker scratch overflow".into()))?;
    let planned = pixels
        .checked_mul(12)
        .and_then(|v| {
            req.cross_section_count
                .checked_mul(4)
                .and_then(|n| n.checked_mul(level_paths.len()))
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
        .and_then(|v| v.checked_add(worker_extra))
        .ok_or_else(|| MapError::Budget("1D request allocation overflow".into()))?;
    // Inline storage: rasterization scratch and labels as in the storage
    // transport, plus storage products, the dry mesh and composed outputs.
    let storage_bytes = match &req.storage {
        Some(storage) => {
            if storage.geo_transform != output_gt {
                return Err(MapError::Invalid(
                    "inline storage geotransform must equal the 1D output window".into(),
                ));
            }
            let polygon_bytes =
                usize::try_from(fs::metadata(base.join(&storage.storage_areas_file))?.len())
                    .map_err(|_| MapError::Budget("storage polygon size overflow".into()))?;
            pixels
                .checked_mul(128)
                .and_then(|v| polygon_bytes.checked_mul(32).and_then(|p| v.checked_add(p)))
                .and_then(|v| {
                    storage
                        .storage_area_count
                        .checked_mul(4096 + 4 * storage.levels_files.len())
                        .and_then(|p| v.checked_add(p))
                })
                .and_then(|v| v.checked_add(1024 * 1024))
                .ok_or_else(|| MapError::Budget("storage allocation plan overflow".into()))?
        }
        None => 0,
    };
    let terrain_source = match &req.terrain_tiff {
        Some(path) => Some(crate::one_d_terrain::inspect(
            &base.join(path),
            req.width,
            req.height,
            gt,
        )?),
        None => None,
    };
    let terrain_bytes = match &terrain_source {
        Some(source) => source
            .planned_bytes(window.width, threads)
            .ok_or_else(|| MapError::Budget("terrain read plan overflow".into()))?,
        None => 0,
    };
    let planned = planned
        .checked_add(terrain_bytes)
        .and_then(|v| v.checked_add(storage_bytes))
        .and_then(|v| v.checked_add(delivery_bytes))
        .ok_or_else(|| MapError::Budget("1D request allocation overflow".into()))?;
    if planned > req.max_working_bytes {
        return Err(MapError::Budget(format!(
            "1D planned {planned} bytes exceeds {}",
            req.max_working_bytes
        )));
    }
    // No payload reads precede admission. Numeric records are little-endian.
    let tick = Instant::now();
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
    let mut profiles = Vec::new();
    profiles
        .try_reserve_exact(level_paths.len())
        .map_err(|e| MapError::Budget(e.to_string()))?;
    for path in &level_paths {
        profiles.push(read_records(
            &base.join(path),
            req.cross_section_count,
            f32::from_le_bytes,
        )?);
    }
    let terrain_tick = Instant::now();
    let terrain = match (&terrain_source, &req.terrain_tiff, &req.terrain_file) {
        (Some(source), Some(path), _) => {
            let mut values = Vec::new();
            values
                .try_reserve_exact(pixels)
                .map_err(|e| MapError::Budget(e.to_string()))?;
            values.resize(pixels, f32::NAN);
            let read_pool = if threads > 1 {
                Some(
                    rayon::ThreadPoolBuilder::new()
                        .num_threads(threads)
                        .build()
                        .map_err(|e| MapError::Internal(e.to_string()))?,
                )
            } else {
                None
            };
            source.read(
                &base.join(path),
                [window.col, window.row, window.width, window.height],
                &mut values,
                read_pool.as_ref(),
            )?;
            values
        }
        (_, _, Some(path)) => read_records(&base.join(path), pixels, f32::from_le_bytes)?,
        _ => unreachable!("terrain source validated above"),
    };
    let terrain_seconds = terrain_tick.elapsed().as_secs_f64();
    let mask = match &delivery {
        Some(plan) => plan.read_mask(base)?,
        None => None,
    };
    let storage_levels = match &req.storage {
        Some(storage) => storage
            .levels_files
            .iter()
            .map(|p| crate::storage_area::values(&base.join(p), storage.storage_area_count))
            .collect::<Result<Vec<_>, _>>()?,
        None => Vec::new(),
    };
    if terrain.iter().any(|v| v.is_infinite()) {
        return Err(MapError::Invalid("1D terrain must be finite or NaN".into()));
    }
    let read_seconds = tick.elapsed().as_secs_f64();
    let tick = Instant::now();
    let mut depth = Vec::new();
    let mut wse = Vec::new();
    depth
        .try_reserve_exact(pixels)
        .map_err(|e| MapError::Budget(e.to_string()))?;
    wse.try_reserve_exact(pixels)
        .map_err(|e| MapError::Budget(e.to_string()))?;
    depth.resize(pixels, f32::NAN);
    wse.resize(pixels, f32::NAN);
    let alloc_seconds = tick.elapsed().as_secs_f64();
    let tick = Instant::now();
    let grid = crate::one_d_native::Grid {
        width: req.width,
        height: req.height,
        geo_transform: gt,
        tile_size: req.tile_size.unwrap_or(1),
    };
    let native_window = crate::one_d_native::Window {
        col: window.col,
        row: window.row,
        width: window.width,
        height: window.height,
    };
    let surface = if native {
        let ranks = read_records(
            &base.join(req.render_order_file.as_ref().unwrap()),
            req.segment_count,
            u32::from_le_bytes,
        )?;
        Some(if clipped {
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
                surface_budget,
            )?
        } else {
            crate::one_d_native::PreparedSurface::new(
                &points,
                &triangles,
                &segments,
                req.cross_section_count,
                &ranks,
                surface_budget,
            )?
        })
    } else {
        None
    };
    let prepare_seconds = tick.elapsed().as_secs_f64();
    let pool = if threads > 1 || delivery.is_some() {
        Some(
            rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .map_err(|e| MapError::Internal(e.to_string()))?,
        )
    } else {
        None
    };
    let mut inline = match &req.storage {
        Some(storage) => {
            if terrain.iter().any(|v| v.is_infinite()) {
                return Err(MapError::Invalid("infinite storage terrain".into()));
            }
            let mut labels = vec![0_u32; pixels];
            let storage_pool = rayon::ThreadPoolBuilder::new()
                .num_threads(threads.min(4))
                .build()
                .map_err(|e| MapError::Internal(e.to_string()))?;
            let grid = terrio::ops::rasterize::Grid {
                geo_transform: storage.geo_transform,
                cols: window.width,
                rows: window.height,
                crs: Default::default(),
            };
            let mut prepared = crate::storage_area::prepare(
                &base.join(&storage.storage_areas_file),
                storage.storage_area_count,
                &grid,
                &labels,
                None,
                &storage_pool,
            )?;
            for ((&p, &label), ground) in prepared
                .pixels
                .iter()
                .zip(&prepared.labels)
                .zip(&mut prepared.ground)
            {
                labels[p] = label + 1;
                *ground = terrain[p];
            }
            Some(Inline {
                prepared,
                labels,
                area_count: storage.storage_area_count,
                zero_owners: vec![0_u32; pixels],
                dry: vec![f32::NAN; pixels],
                storage_depth: vec![f32::NAN; pixels],
                storage_wse: vec![f32::NAN; pixels],
                depth: vec![f32::NAN; pixels],
                wse: vec![f32::NAN; pixels],
                chunk: 1 << 20,
            })
        }
        None => None,
    };
    let method = if clipped {
        crate::one_d_native::clipped_method_record()
    } else if native {
        crate::one_d_native::method_record()
    } else {
        crate::one_d::method_record()
    };
    let profile_report = |valid: usize| {
        let mut report = json!({"schema": req.schema, "method": method,
            "status": "complete", "scope": req.scope, "width": window.width, "height": window.height,
            "geo_transform": output_gt, "source_width": req.width, "source_height": req.height,
            "source_geo_transform":gt, "source_col":window.col, "source_row":window.row, "tile_size":req.tile_size, "clipping_scope":req.clipping_scope, "valid_pixels": valid, "planned_bytes": planned,
            "depth_file": "depth.bin", "wse_file": "wse.bin", "dtype": "little-endian float32", "nodata": "NaN",
            "threads": threads});
        if let Some(layout) = layout {
            report["grid_contract"] = json!(req.grid_contract);
            report["output_geo_transform"] = json!(layout.output);
        }
        report
    };
    // The output folder is created only after the first successful render, so
    // refused requests leave no output (unchanged single-profile behaviour).
    let mut created = false;
    let mut summaries = Vec::new();
    let mut last_report = Value::Null;
    let mut failure = None;
    for (index, levels) in profiles.iter().enumerate() {
        let folder = if multi {
            output.join(format!("profile-{index:04}"))
        } else {
            output.to_path_buf()
        };
        let tick = Instant::now();
        let rendered = match &surface {
            Some(surface) => surface.render_into_with(
                levels,
                &terrain,
                grid,
                native_window,
                &mut depth,
                &mut wse,
                pool.as_ref(),
            ),
            None => PreparedSurface::new(
                &points,
                &triangles,
                &segments,
                req.cross_section_count,
                geometry,
            )
            .and_then(|surface| {
                let profile = surface.profile(levels)?;
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
                Ok(valid)
            }),
        };
        let valid = match rendered {
            Ok(valid) => valid,
            Err(e) if multi => {
                failure = Some((index, e));
                break;
            }
            Err(e) => return Err(e),
        };
        let storage_fields = match inline.as_mut() {
            Some(inline) => {
                match inline.compose(&storage_levels[index], &mut depth, &mut wse, pool.as_ref()) {
                    Ok(fields) => Some(fields),
                    Err(e) if multi => {
                        failure = Some((index, e));
                        break;
                    }
                    Err(e) => return Err(e),
                }
            }
            None => None,
        };
        let render_seconds = tick.elapsed().as_secs_f64();
        let tick = Instant::now();
        let mut report = profile_report(valid);
        if let Some(fields) = storage_fields {
            for (key, value) in fields.as_object().into_iter().flatten() {
                report[key] = value.clone();
            }
        }
        if !created {
            fs::create_dir(output)?;
            created = true;
        }
        let result = (|| {
            if multi {
                fs::create_dir(&folder)?;
            }
            if let (Some(plan), Some(pool)) = (&delivery, &pool) {
                return plan
                    .deliver(&folder, mask.as_deref(), &mut depth, &mut wse, pool)
                    .map(Some);
            }
            crate::horizontal::write_values(&folder.join("depth.bin"), &depth)?;
            crate::horizontal::write_values(&folder.join("wse.bin"), &wse)?;
            Ok::<_, MapError>(None)
        })();
        let write_seconds = tick.elapsed().as_secs_f64();
        // Additive report fields; wall seconds on the calling thread. Shared
        // read/prepare time is reported once, on the first profile.
        let shared = index == 0;
        report["timing"] = json!({
            "read_seconds": if shared { read_seconds } else { 0.0 },
            "terrain_read_seconds": if shared { terrain_seconds } else { 0.0 },
            "allocate_seconds": if shared { alloc_seconds } else { 0.0 },
            "surface_prepare_seconds": if shared { prepare_seconds } else { 0.0 },
            "render_seconds": render_seconds,
            "write_seconds": write_seconds,
            "total_seconds": started.elapsed().as_secs_f64(),
        });
        match &result {
            Ok(Some(fields)) => {
                for (key, value) in fields.as_object().into_iter().flatten() {
                    report[key] = value.clone();
                }
            }
            Ok(None) => {}
            Err(e) => {
                report["status"] = json!("aborted");
                report["error"] = json!(e.to_string());
            }
        }
        if multi && folder.is_dir() {
            fs::write(
                folder.join("report.json"),
                serde_json::to_vec_pretty(&report)
                    .map_err(|e| MapError::Internal(e.to_string()))?,
            )?;
        }
        summaries.push(json!({"index": index, "levels_file": level_paths[index],
            "folder": if multi { format!("profile-{index:04}") } else { ".".into() },
            "status": report["status"], "valid_pixels": valid,
            "render_seconds": render_seconds, "write_seconds": write_seconds}));
        last_report = report;
        if let Err(e) = result {
            if multi {
                failure = Some((index, e));
                break;
            }
            fs::write(
                output.join("report.json"),
                serde_json::to_vec_pretty(&last_report)
                    .map_err(|e| MapError::Internal(e.to_string()))?,
            )?;
            return Err(e);
        }
    }
    if !created {
        // Only reachable when the first multi-profile render refused.
        return Err(failure
            .map(|(_, e)| e)
            .unwrap_or_else(|| MapError::Internal("1D produced no profile".into())));
    }
    let report = if multi {
        let mut report = profile_report(0);
        let obj = report.as_object_mut().unwrap();
        for key in ["valid_pixels", "depth_file", "wse_file"] {
            obj.remove(key);
        }
        report["profile_count"] = json!(profiles.len());
        report["profiles"] = json!(summaries);
        report["timing"] = json!({
            "read_seconds": read_seconds,
            "terrain_read_seconds": terrain_seconds,
            "allocate_seconds": alloc_seconds,
            "surface_prepare_seconds": prepare_seconds,
            "total_seconds": started.elapsed().as_secs_f64(),
        });
        if let Some((index, e)) = &failure {
            report["status"] = json!("aborted");
            report["error"] = json!(format!("profile {index}: {e}"));
        }
        report
    } else {
        last_report
    };
    fs::write(
        output.join("report.json"),
        serde_json::to_vec_pretty(&report).map_err(|e| MapError::Internal(e.to_string()))?,
    )?;
    if let Some((_, e)) = failure {
        return Err(e);
    }
    Ok(report)
}

/// Per-request inline storage state; buffers are reused for every profile.
struct Inline {
    prepared: crate::storage_area::StorageAreaPixels,
    labels: Vec<u32>,
    area_count: usize,
    zero_owners: Vec<u32>,
    dry: Vec<f32>,
    storage_depth: Vec<f32>,
    storage_wse: Vec<f32>,
    depth: Vec<f32>,
    wse: Vec<f32>,
    /// Pixels per composition chunk.
    chunk: usize,
}

impl Inline {
    /// Level pools then coverage-first composition; the composed products are
    /// swapped into `depth`/`wse` (the XS buffers are re-filled by the next render).
    fn compose(
        &mut self,
        levels: &[f32],
        depth: &mut Vec<f32>,
        wse: &mut Vec<f32>,
        pool: Option<&rayon::ThreadPool>,
    ) -> Result<Value, MapError> {
        for &p in &self.prepared.pixels {
            self.storage_depth[p] = f32::NAN;
            self.storage_wse[p] = f32::NAN;
        }
        let storage_valid = crate::storage_area::apply(
            &self.prepared,
            levels,
            0.,
            &mut self.storage_depth,
            &mut self.storage_wse,
        )?;
        // The kernel validates and writes every pixel of its slices and its
        // counts are integer sums, so independent chunks give identical bits;
        // the first error in chunk order is reported.
        let chunk = self.chunk;
        let chunks: Vec<_> = self
            .depth
            .chunks_mut(chunk)
            .zip(self.wse.chunks_mut(chunk))
            .enumerate()
            .collect();
        let (xs_depth, xs_wse): (&[f32], &[f32]) = (depth, wse);
        let compose = |(i, (out_depth, out_wse)): (usize, (&mut [f32], &mut [f32]))| {
            let range = i * chunk..i * chunk + out_depth.len();
            crate::coupled::compose_with_storage_into(
                &self.zero_owners[range.clone()],
                0,
                crate::coupled::Products {
                    depth: &xs_depth[range.clone()],
                    wse: &xs_wse[range.clone()],
                },
                crate::coupled::Products {
                    depth: &self.dry[range.clone()],
                    wse: &self.dry[range.clone()],
                },
                crate::coupled::Storage {
                    owners: &self.labels[range.clone()],
                    area_count: self.area_count,
                    products: crate::coupled::Products {
                        depth: &self.storage_depth[range.clone()],
                        wse: &self.storage_wse[range],
                    },
                },
                out_depth,
                out_wse,
            )
        };
        let parts: Vec<_> = match pool {
            Some(pool) => {
                use rayon::prelude::*;
                pool.install(|| chunks.into_par_iter().map(compose).collect())
            }
            None => chunks.into_iter().map(compose).collect(),
        };
        let mut counts = crate::coupled::StorageCounts {
            base: crate::coupled::Counts {
                valid_pixels: 0,
                two_d_owned_pixels: 0,
                dry_two_d_suppressed_wet_xs: 0,
            },
            storage_owned_pixels: 0,
            dry_storage_suppressed_wet_xs: 0,
        };
        for part in parts {
            let part = part?;
            counts.base.valid_pixels += part.base.valid_pixels;
            counts.base.two_d_owned_pixels += part.base.two_d_owned_pixels;
            counts.base.dry_two_d_suppressed_wet_xs += part.base.dry_two_d_suppressed_wet_xs;
            counts.storage_owned_pixels += part.storage_owned_pixels;
            counts.dry_storage_suppressed_wet_xs += part.dry_storage_suppressed_wet_xs;
        }
        std::mem::swap(depth, &mut self.depth);
        std::mem::swap(wse, &mut self.wse);
        Ok(json!({
            "storage": {"method": crate::storage_session::method_record(),
                "storage_owned_pixels": self.prepared.pixels.len(), "valid_pixels": storage_valid},
            "composition": {"method": crate::coupled::storage_method_record(),
                "counts": serde_json::to_value(counts).map_err(|e| MapError::Internal(e.to_string()))?},
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunked_parallel_composition_matches_one_kernel_call() {
        let n = 1003;
        let mut seed = 7_u64;
        let mut next = move || {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (seed >> 33) as f32 / (1u64 << 31) as f32
        };
        let (mut xd, mut xw) = (vec![f32::NAN; n], vec![f32::NAN; n]);
        let mut labels = vec![0_u32; n];
        let (mut pixels, mut owners, mut ground) = (vec![], vec![], vec![]);
        for i in 0..n {
            if next() < 0.6 {
                xd[i] = next() * 3.0;
                xw[i] = 10.0 + next();
            }
            if next() < 0.3 {
                labels[i] = 1 + (i % 3) as u32;
                pixels.push(i);
                owners.push(labels[i] - 1);
                ground.push(if i % 17 == 0 { f32::NAN } else { 9.0 + next() });
            }
        }
        let prepared = crate::storage_area::StorageAreaPixels {
            pixels,
            labels: owners,
            ground,
            count: 3,
        };
        let levels = [9.5_f32, 10.5, 8.0];
        // Reference: one kernel call over the whole arrays.
        let (mut sd, mut sw) = (vec![f32::NAN; n], vec![f32::NAN; n]);
        crate::storage_area::apply(&prepared, &levels, 0., &mut sd, &mut sw).unwrap();
        let (mut rd, mut rw) = (vec![0.; n], vec![0.; n]);
        let zero = vec![0_u32; n];
        let dry = vec![f32::NAN; n];
        let reference = crate::coupled::compose_with_storage_into(
            &zero,
            0,
            crate::coupled::Products {
                depth: &xd,
                wse: &xw,
            },
            crate::coupled::Products {
                depth: &dry,
                wse: &dry,
            },
            crate::coupled::Storage {
                owners: &labels,
                area_count: 3,
                products: crate::coupled::Products {
                    depth: &sd,
                    wse: &sw,
                },
            },
            &mut rd,
            &mut rw,
        )
        .unwrap();
        for (chunk, threads) in [(7, 3), (64, 2), (n, 1), (5000, 4)] {
            let mut inline = Inline {
                prepared: crate::storage_area::StorageAreaPixels {
                    pixels: prepared.pixels.clone(),
                    labels: prepared.labels.clone(),
                    ground: prepared.ground.clone(),
                    count: 3,
                },
                labels: labels.clone(),
                area_count: 3,
                zero_owners: zero.clone(),
                dry: dry.clone(),
                storage_depth: vec![f32::NAN; n],
                storage_wse: vec![f32::NAN; n],
                depth: vec![1.0; n],
                wse: vec![1.0; n],
                chunk,
            };
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .unwrap();
            // Twice: the second profile must not see the first profile's state.
            for _ in 0..2 {
                let (mut d, mut w) = (xd.clone(), xw.clone());
                let fields = inline
                    .compose(&levels, &mut d, &mut w, Some(&pool))
                    .unwrap();
                assert!(d.iter().zip(&rd).all(|(a, b)| a.to_bits() == b.to_bits()));
                assert!(w.iter().zip(&rw).all(|(a, b)| a.to_bits() == b.to_bits()));
                assert_eq!(
                    fields["composition"]["counts"],
                    serde_json::to_value(&reference).unwrap()
                );
            }
        }
    }
}
