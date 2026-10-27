//! Explicit sloping batch path with independently selected native rendering controls. Geometry, terrain and ownership are shared
//! across profiles; hydraulic connectivity is reconstructed for every profile.
use crate::retained::Retained;
use crate::{
    MapError,
    model::{self, CrsIdentity, NodataPolicy, QueryConvention},
    sink,
    sloping::{self, Cell, Corner, Face, Mesh},
    terrain::NativeTerrain,
};
use rayon::prelude::*;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    fs,
    io::Write,
    path::Path,
    sync::{Arc, Mutex},
    time::Instant,
};

type CachedGeometry = (Vec<Mesh>, Vec<Vec<Option<sloping::CellStencil>>>);
type WindowKey = (
    String,
    String,
    [usize; 4],
    Vec<[usize; 2]>,
    Vec<u8>,
    Option<Vec<u8>>,
);
struct WindowData {
    owner: Vec<u32>,
    ground: Vec<f32>,
    weights: Option<sloping::PixelWeights>,
}

/// One explicitly owned, serialized native session. One complete geometry and
/// terrain share bounded LRU window and hydraulic caches. Dropping releases all.
#[derive(Default)]
pub struct PreparedSession {
    terrain: Option<(String, Arc<NativeTerrain>)>,
    geometry: Option<(String, Arc<CachedGeometry>)>,
    windows: Retained<WindowKey, WindowData>,
    hydraulics: Mutex<Retained<String, Vec<sloping::PreparedProfile>>>,
    pool: Option<(usize, Arc<rayon::ThreadPool>)>,
}

fn identity(path: &Path) -> Result<String, MapError> {
    let path = path.canonicalize()?;
    let metadata = path.metadata()?;
    Ok(format!(
        "{:?}:{}:{:?}",
        path,
        metadata.len(),
        metadata.modified()?
    ))
}
fn geometry_key(req: &Request, base: &Path) -> Result<String, MapError> {
    let mut key = String::new();
    for filename in &req.mesh_files {
        let path = model::resolve(base, filename);
        key.push_str(&identity(&path)?);
        let document: Value = serde_json::from_reader(fs::File::open(&path)?)
            .map_err(|e| MapError::Invalid(e.to_string()))?;
        if let Some(arrays) = document["arrays"].as_object() {
            for descriptor in arrays.values() {
                if let Some(name) = descriptor["path"].as_str() {
                    key.push_str(&identity(&model::resolve(path.parent().unwrap(), name))?);
                }
            }
        }
    }
    Ok(key)
}

pub const SCHEMA: &str = "ras-commander-sloping-request/1";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub schema: String,
    #[serde(default)]
    pub options: sloping::Options,
    pub terrain: String,
    pub epsg: u32,
    pub crs_identity: CrsIdentity,
    pub terrain_transform: [f64; 6],
    pub window: [usize; 4],
    pub geometry_file: String,
    #[serde(default)]
    pub aoi_file: Option<String>,
    pub mesh_files: Vec<String>,
    pub owner_cells: Vec<[usize; 2]>,
    pub profiles: Vec<Profile>,
    pub threads: usize,
    #[serde(default = "one_profile")]
    pub max_inflight_profiles: usize,
    pub max_working_bytes: u64,
    #[serde(default)]
    pub stream_outputs: bool,
    /// Retained multi-window ownership/terrain/weight ceiling. Zero disables it.
    #[serde(default = "window_budget")]
    pub window_cache_bytes: u64,
    /// Retained reconstructed timestep ceiling. Zero disables it.
    #[serde(default)]
    pub hydraulic_cache_bytes: u64,
    /// Write native LZW GeoTIFF products and stream only their completion records.
    #[serde(default)]
    pub geotiff_outputs: bool,
    /// Per mesh: HDF-ordered float32 (x, y, polyline length) face normals.
    /// With it, every profile supplies `face_velocity_files` and a velocity
    /// magnitude product is rendered (method `rasmapper-sloping-velocity/0`).
    #[serde(default)]
    pub velocity_normals_files: Option<Vec<String>>,

    /// Declares `terrain` as the top-priority source of a registered terrain.
    #[serde(default)]
    pub terrain_scope: Option<crate::terrain_scope::TerrainScope>,
    /// Optional standalone storage-area polygons (framed WKB, one per area).
    #[serde(default)]
    pub storage_areas_file: Option<String>,
    #[serde(default)]
    pub storage_area_count: usize,
}

fn one_profile() -> usize {
    1
}
fn window_budget() -> u64 {
    64 << 20
}

fn admit_profiles(
    requested: usize,
    threads: usize,
    profiles: usize,
    budget: u64,
    serial: u64,
    additional: u64,
) -> Result<(usize, u64), MapError> {
    if serial > budget || additional == 0 || requested == 0 || threads == 0 || profiles == 0 {
        return Err(MapError::Budget(
            "sloping decoder plan exceeds budget".into(),
        ));
    }
    let limit = requested.min(threads).min(profiles);
    let extra = ((budget - serial) / additional).min((limit - 1) as u64);
    Ok((extra as usize + 1, serial + extra * additional))
}

fn map_batch<T: Sync, R: Send>(
    batch: &[T],
    pool: &rayon::ThreadPool,
    compute: impl Fn(&T) -> Result<R, MapError> + Sync + Send,
) -> Result<Vec<R>, MapError> {
    pool.install(|| batch.par_iter().map(compute).collect())
}

struct RenderedProfile {
    depth: Vec<f32>,
    wse: Vec<f32>,
    velocity: Option<Vec<f32>>,
    hydraulic_seconds: f64,
    render_seconds: f64,
    hydraulic_reused: bool,
    finite_pixels: usize,
    flooded_pixels: usize,
}

#[allow(clippy::too_many_arguments)] // Explicit immutable geometry/window/cache borrows.
fn render(
    profile: &Profile,
    base: &Path,
    meshes: &[Mesh],
    stencils: &[Vec<Option<sloping::CellStencil>>],
    window: &sloping::RasterWindow<'_>,
    pool: &rayon::ThreadPool,
    options: sloping::Options,
    weights: Option<&sloping::PixelWeights>,
    normals: Option<&[Vec<crate::velocity::FaceNormal>]>,
    foreign: &[usize],
    hydraulic_cache: Option<&Mutex<Retained<String, Vec<sloping::PreparedProfile>>>>,
    storage: Option<&crate::storage_area::StorageAreaPixels>,
) -> Result<RenderedProfile, MapError> {
    let tick = Instant::now();
    let cache_key = if hydraulic_cache.is_some() && normals.is_none() {
        let mut key =
            serde_json::to_string(&options).map_err(|e| MapError::Invalid(e.to_string()))?;
        for file in &profile.values_files {
            key.push_str(&identity(&model::resolve(base, file))?);
        }
        Some(key)
    } else {
        None
    };
    let cached = match (&cache_key, hydraulic_cache) {
        (Some(key), Some(cache)) => cache.lock().unwrap().get(key),
        _ => None,
    };
    let hydraulic_reused = cached.is_some();
    let mut new_prepared = Vec::new();
    let mut cell_values = Vec::new();
    if !hydraulic_reused {
        for ((mesh, stencil), filename) in meshes.iter().zip(stencils).zip(&profile.values_files) {
            let wse = values(&model::resolve(base, filename), mesh.cells.len())?;
            new_prepared.push(sloping::prepare_profile_with_options(
                mesh, stencil, &wse, 0.001, pool, options,
            )?);
            if normals.is_some() {
                cell_values.push(wse);
            }
        }
    }
    let prepared = cached.unwrap_or_else(|| Arc::new(new_prepared));
    if !hydraulic_reused && let (Some(key), Some(cache)) = (cache_key, hydraulic_cache) {
        let bytes = prepared.iter().map(|p| p.retained_bytes()).sum::<u64>()
            + (prepared.capacity() * std::mem::size_of::<sloping::PreparedProfile>()) as u64
            + key.capacity() as u64
            + (2 * std::mem::size_of::<(String, Arc<Vec<sloping::PreparedProfile>>, u64)>()
                + std::mem::size_of::<Vec<sloping::PreparedProfile>>()
                + 2 * std::mem::size_of::<usize>()) as u64;
        cache.lock().unwrap().insert(key, prepared.clone(), bytes);
    }
    crate::terrain_scope::deny_wet_foreign(foreign, window.owner, window.owner_cells, |m, c| {
        prepared[m].is_wet(c)
    })?;
    let hydraulic_seconds = tick.elapsed().as_secs_f64();
    let tick = Instant::now();
    let (mut depth, mut wse) =
        sloping::render_profile_cached(stencils, &prepared, window, 0.001, pool, weights)?;
    let render_seconds = tick.elapsed().as_secs_f64();
    let (mut finite_pixels, mut flooded_pixels) =
        crate::finalize::apply(&mut depth, &mut wse, pool);
    if let Some(storage) = storage {
        let path = profile
            .storage_area_values_file
            .as_ref()
            .ok_or_else(|| MapError::Invalid("storage-area profile values missing".into()))?;
        let values = crate::storage_area::values(&model::resolve(base, path), storage.count)?;
        finite_pixels += crate::storage_area::apply(storage, &values, 0.0, &mut depth, &mut wse)?;
        flooded_pixels += storage
            .pixels
            .iter()
            .filter(|&&p| depth[p] > 0.0 && depth[p] <= 10000.0)
            .count();
    }
    let velocity = match (normals, &profile.face_velocity_files) {
        (Some(normals), Some(files)) => {
            let mut face_velocity = Vec::new();
            for (mesh, file) in meshes.iter().zip(files) {
                face_velocity.push(crate::velocity::read_face_velocity(
                    &model::resolve(base, file),
                    mesh.faces.len(),
                )?);
            }
            let inputs: Vec<_> = meshes
                .iter()
                .zip(stencils)
                .zip(&cell_values)
                .zip(normals)
                .zip(&face_velocity)
                .map(|((((mesh, stencil), wse), normal), velocity)| {
                    (
                        mesh,
                        stencil.as_slice(),
                        wse.as_slice(),
                        normal.as_slice(),
                        velocity.as_slice(),
                    )
                })
                .collect();
            Some(crate::velocity::render(&inputs, window, &depth, pool)?)
        }
        _ => None,
    };
    Ok(RenderedProfile {
        depth,
        wse,
        velocity,
        hydraulic_seconds,
        render_seconds,
        hydraulic_reused,
        finite_pixels,
        flooded_pixels,
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub id: String,
    pub values_files: Vec<String>,
    /// Per mesh: HDF-ordered float32 face-normal velocities of this profile.
    #[serde(default)]
    pub face_velocity_files: Option<Vec<String>>,
    /// Storage-area WSE for this profile (f32 per area), required with storage areas.
    #[serde(default)]
    pub storage_area_values_file: Option<String>,
}

#[derive(Deserialize)]
pub struct Geometry {
    #[serde(default)]
    pub vertex_elevations: Option<Vec<f32>>,
    pub cells: Vec<InputCell>,
    pub faces: Vec<InputFace>,
    pub vertices: Vec<InputVertex>,
}
#[derive(Deserialize)]
pub struct InputCell {
    pub point: [f64; 2],
    pub minimum_elevation: f32,
    #[serde(rename = "virtual")]
    pub virtual_cell: bool,
    pub faces: Vec<usize>,
}
#[derive(Deserialize)]
pub struct InputFace {
    pub cells: [usize; 2],
    pub vertices: [usize; 2],
    pub minimum_elevation: f32,
    pub perimeter: bool,
    pub polyline: Vec<[f64; 2]>,
}
#[derive(Deserialize)]
pub struct InputVertex {
    pub point: [f64; 2],
    pub faces: Vec<usize>,
    pub ccw: Vec<usize>,
    pub cw: Vec<usize>,
}

impl Geometry {
    pub fn prepare(&self) -> Result<(Mesh, Vec<Option<sloping::CellStencil>>), MapError> {
        if self
            .cells
            .iter()
            .any(|c| c.virtual_cell != (c.faces.len() == 1))
        {
            return Err(MapError::Invalid(
                "sloping virtual-cell flag contradicts topology".into(),
            ));
        }
        let faces =
            self.faces
                .iter()
                .map(|f| {
                    let a = self.cells.get(f.cells[0]).ok_or_else(|| {
                        MapError::Invalid("sloping cell address out of range".into())
                    })?;
                    let b = self.cells.get(f.cells[1]).ok_or_else(|| {
                        MapError::Invalid("sloping cell address out of range".into())
                    })?;
                    Ok(Face {
                        cells: f.cells,
                        vertices: f.vertices,
                        minimum: f.minimum_elevation,
                        perimeter: f.perimeter,
                        application: sloping::face_application(&f.polyline, [a.point, b.point])?,
                    })
                })
                .collect::<Result<Vec<_>, MapError>>()?;
        let mesh = Mesh {
            cells: self
                .cells
                .iter()
                .map(|c| Cell {
                    minimum: c.minimum_elevation,
                    virtual_cell: c.virtual_cell,
                })
                .collect(),
            faces,
            corners: self
                .vertices
                .iter()
                .map(|v| Corner {
                    point: v.point,
                    faces: v.faces.clone(),
                    ccw: v.ccw.clone(),
                    cw: v.cw.clone(),
                })
                .collect(),
        };
        let cell_faces = self
            .cells
            .iter()
            .map(|c| c.faces.clone())
            .collect::<Vec<_>>();
        let mut stencils = sloping::prepare_stencils(&mesh, &cell_faces)?;
        for (cell, stencil) in stencils.iter_mut().enumerate() {
            if let Some(stencil) = stencil {
                for &face in &self.cells[cell].faces {
                    let f = &self.faces[face];
                    if f.cells[0] == cell {
                        stencil.physical_ring.extend(f.polyline.iter().copied());
                    } else {
                        stencil
                            .physical_ring
                            .extend(f.polyline.iter().rev().copied());
                    }
                }
                stencil.physical_ring.dedup();
            }
        }
        if let Some(elevations) = &self.vertex_elevations {
            sloping::set_vertex_elevations(&mut stencils, elevations, mesh.corners.len())?;
        }
        Ok((mesh, stencils))
    }
}

pub fn method_record() -> Value {
    json!({"method_id":"rasmapper-plain-sloping/1", "backend":"rust", "experimental":true,
        "native_version_qualified":"7.0.1", "stream_outputs":true, "native_ownership":true,"aoi_mask":crate::aoi::METHOD,
        "enhanced_options":true,"enhanced_method_id":"rasmapper-sloping-with-faces/1",
        "depth_weighted_faces":true,
        "native_donation_history":true,
        "bounded_profile_concurrency":true,
        "persistent_session":true,"multi_window_cache":true,"hydraulic_cache":true,"native_geotiff_outputs":true,"bounded_pixel_weights":true,
        "velocity":crate::velocity::METHOD_ID,
        "terrain_scope":crate::terrain_scope::METHOD_ID,
        "storage_area_surfaces":crate::storage_area::METHOD_ID,
        "options":{"CellRenderMode":"Sloping","CellStencilMode":"JustFacepoints",
            "FacepointAdjustmentMode":"None","ShallowBehaviorMode":"None",
            "UseDepthWeightedFaces":false,"FaceWSMode":"Adjusted"},
        "native_plot_tolerance":0.001_f32,"extra_depth_cutoff":null,
        "scope":"2D cells; plain cell-corner sloping; optional storage-area level pools; no 1D, slopingPretty or additional rendering options",
        "reuse":"geometry, terrio ownership and terrain prepared once; bounded Rayon pixel and reconstruction workers"})
}

fn selected_method(options: sloping::Options) -> Value {
    let mut record = method_record();
    if options.render_mode == sloping::RenderMode::WithFaces {
        record["method_id"] = "rasmapper-sloping-with-faces/1".into();
        record["options"]["CellStencilMode"] = "WithFaces".into();
        record["options"]["ShallowBehaviorMode"] = if options.reduce_shallow_to_horizontal {
            "ReduceToHorizontal"
        } else {
            "None"
        }
        .into();
        record["options"]["UseDepthWeightedFaces"] = options.use_depth_weighted_faces.into();
        record["scope"] = "physical 2D cells; cell-corner and face sloping; optional storage-area level pools; no 1D".into();
    }
    record
}

fn values(path: &Path, count: usize) -> Result<Vec<f32>, MapError> {
    let expected = count
        .checked_mul(4)
        .ok_or_else(|| MapError::Budget("sloping values size overflow".into()))?;
    if fs::metadata(path)?.len() != expected as u64 {
        return Err(MapError::Invalid("sloping profile length mismatch".into()));
    }
    let raw = fs::read(path)?;
    if raw.len() != expected {
        return Err(MapError::Invalid("sloping profile changed size".into()));
    }
    let values: Vec<_> = raw
        .as_chunks::<4>()
        .0
        .iter()
        .map(|v| f32::from_le_bytes(*v))
        .collect();
    if values.iter().any(|v| !v.is_finite()) {
        return Err(MapError::Invalid(
            "sloping profiles require finite cell WSE including virtual cells".into(),
        ));
    }
    Ok(values)
}

pub fn run(req: &Request, base: &Path, out: &Path) -> Result<Value, MapError> {
    run_with_session(req, base, out, None)
}

pub fn run_with_session(
    req: &Request,
    base: &Path,
    out: &Path,
    mut session: Option<&mut PreparedSession>,
) -> Result<Value, MapError> {
    req.options.validate()?;
    if req.geotiff_outputs && (!req.stream_outputs || req.velocity_normals_files.is_some()) {
        return Err(MapError::Unsupported(
            "native GeoTIFF requires stream_outputs and no velocity".into(),
        ));
    }
    if let Some(scope) = &req.terrain_scope {
        scope.validate()?;
        if req.options.render_mode == sloping::RenderMode::WithFaces {
            return Err(MapError::Unsupported(
                "terrain_scope is qualified for plain sloping only".into(),
            ));
        }
    }
    let started = Instant::now();
    let [col, row, width, height] = req.window;
    let count = width
        .checked_mul(height)
        .filter(|&n| n > 0 && n <= model::MAX_PROFILE_PIXELS as usize)
        .ok_or_else(|| MapError::Budget("sloping window empty or too large".into()))?;
    if req.schema != SCHEMA
        || !(1..=64).contains(&req.threads)
        || !(1..=64).contains(&req.max_inflight_profiles)
        || req.epsg == 0
        || req.mesh_files.is_empty()
        || req.owner_cells.is_empty()
        || req.owner_cells.len() >= u32::MAX as usize
        || req.profiles.is_empty()
    {
        return Err(MapError::Invalid("invalid sloping request".into()));
    }
    if let Some(files) = &req.velocity_normals_files
        && files.len() != req.mesh_files.len()
    {
        return Err(MapError::Invalid(
            "velocity_normals_files needs one file per mesh".into(),
        ));
    }
    if req.profiles.iter().any(|p| {
        p.face_velocity_files.is_some() != req.velocity_normals_files.is_some()
            || p.face_velocity_files
                .as_ref()
                .is_some_and(|f| f.len() != req.mesh_files.len())
    }) {
        return Err(MapError::Invalid(
            "velocity_normals_files and every profile's face_velocity_files (one per mesh) go together".into(),
        ));
    }
    let storage = crate::horizontal::validate_storage_fields(
        req.storage_areas_file.as_ref(),
        req.storage_area_count,
        req.profiles
            .iter()
            .map(|p| p.storage_area_values_file.as_ref()),
    )?;
    if storage && req.velocity_normals_files.is_some() {
        return Err(MapError::Unsupported(
            "velocity is not qualified with storage-area surfaces".into(),
        ));
    }
    let mut ids = HashSet::new();
    for profile in &req.profiles {
        model::validate_id(&profile.id)?;
        if !ids.insert(model::id_key(&profile.id))
            || profile.values_files.len() != req.mesh_files.len()
        {
            return Err(MapError::Invalid(
                "duplicate sloping profile id or mesh value count mismatch".into(),
            ));
        }
    }
    let geometry_bytes = req
        .mesh_files
        .iter()
        .chain(std::iter::once(&req.geometry_file))
        .chain(req.aoi_file.iter())
        .chain(req.storage_areas_file.iter())
        .try_fold(0_u64, |sum, p| {
            sum.checked_add(fs::metadata(model::resolve(base, p))?.len())
                .ok_or_else(|| MapError::Budget("sloping geometry size overflow".into()))
        })?;
    let binary_bytes = req.mesh_files.iter().try_fold(0_u64, |sum, name| {
        sum.checked_add(crate::sloping_binary::input_bytes(&model::resolve(
            base, name,
        ))?)
        .ok_or_else(|| MapError::Budget("sloping binary geometry size overflow".into()))
    })?;
    let geometry_bytes = geometry_bytes
        .checked_add(binary_bytes)
        .ok_or_else(|| MapError::Budget("sloping geometry size overflow".into()))?;
    let cache = 64_u64 << 20;
    // JSON parsing, topology, stencils, one hydraulic profile and IO copies are
    // conservatively admitted, rather than claiming an OS-enforced RSS limit.
    let persistent_bytes = if session.is_some() {
        req.window_cache_bytes
    } else {
        0
    };
    let hydraulic_bytes = if session.is_some() {
        req.hydraulic_cache_bytes
    } else {
        0
    };
    if let Some(s) = session.as_mut() {
        s.windows.resize(persistent_bytes);
        s.hydraulics.lock().unwrap().resize(hydraulic_bytes);
    }
    let aoi_scratch = crate::aoi::scratch_bytes(
        req.aoi_file
            .as_ref()
            .map(|p| model::resolve(base, p))
            .as_deref(),
        req.threads,
    )?;
    let planned = (count as u64)
        .checked_mul(
            if req.options.render_mode == sloping::RenderMode::WithFaces {
                56
            } else {
                40
            },
        )
        .and_then(|v| {
            geometry_bytes
                .checked_mul(
                    if req.options.render_mode == sloping::RenderMode::WithFaces {
                        20
                    } else {
                        16
                    },
                )
                .and_then(|g| v.checked_add(g))
        })
        .and_then(|v| v.checked_add(cache))
        .and_then(|v| v.checked_add(persistent_bytes))
        .and_then(|v| v.checked_add(hydraulic_bytes))
        .and_then(|v| v.checked_add(aoi_scratch))
        // Terrain scope: one validity byte per expanded-window pixel.
        .and_then(|v| {
            v.checked_add(if req.terrain_scope.is_some() {
                (width as u64 + 2) * (height as u64 + 2)
            } else {
                0
            })
        })
        // Storage areas: forward/reverse label burns, the AOI mask and sparse pixel lists.
        .and_then(|v| v.checked_add(if storage { count as u64 * 28 } else { 0 }))
        .ok_or_else(|| MapError::Budget("sloping array plan overflow".into()))?;
    if planned > req.max_working_bytes {
        return Err(MapError::Budget("sloping array plan exceeds budget".into()));
    }
    let key = if session.is_some() {
        geometry_key(req, base)?
    } else {
        String::new()
    };
    // Release an incompatible previous model before opening/loading this one.
    if let Some(s) = session.as_mut()
        && s.geometry.as_ref().is_some_and(|(old, _)| old != &key)
    {
        s.geometry = None;
        s.hydraulics.lock().unwrap().clear();
        s.windows.clear();
    }
    let terrain_path = model::resolve(base, &req.terrain);
    let terrain_key = if session.is_some() {
        format!(
            "{}:{}:{}",
            identity(&terrain_path)?,
            req.epsg,
            serde_json::to_string(&req.crs_identity)
                .map_err(|e| MapError::Invalid(e.to_string()))?
        )
    } else {
        String::new()
    };
    let terrain_reused = session.as_ref().is_some_and(|s| {
        s.terrain
            .as_ref()
            .is_some_and(|(key, _)| key == &terrain_key)
    });
    if !terrain_reused && let Some(s) = session.as_mut() {
        s.terrain = None;
        s.windows.clear();
    }
    let terrain = if terrain_reused {
        session
            .as_ref()
            .unwrap()
            .terrain
            .as_ref()
            .unwrap()
            .1
            .clone()
    } else {
        Arc::new(NativeTerrain::open(
            model::resolve(base, &req.terrain)
                .to_str()
                .ok_or_else(|| MapError::Invalid("terrain path is not UTF8".into()))?,
            req.epsg,
            &req.crs_identity,
            QueryConvention::PixelCenter,
            NodataPolicy::Dry,
            cache,
        )?)
    };
    if let Some(s) = session.as_mut() {
        s.terrain = Some((terrain_key.clone(), terrain.clone()));
    }
    if terrain.nodata.is_none()
        || terrain.gt != req.terrain_transform
        || (terrain.gt[1] + terrain.gt[5]).abs() > terrain.gt[1].abs() * 1e-12
        || col.checked_add(width).is_none_or(|v| v > terrain.width)
        || row.checked_add(height).is_none_or(|v| v > terrain.height)
        || terrain.max_block_bytes() > cache
    {
        return Err(MapError::Unsupported(
            "sloping terrain identity, square grid, block or window mismatch".into(),
        ));
    }
    let admitted = planned
        .checked_add(
            if req.options.render_mode == sloping::RenderMode::WithFaces {
                (terrain.block.0 as u64)
                    .checked_mul(terrain.block.1 as u64)
                    .and_then(|v| v.checked_mul(req.threads as u64))
                    .and_then(|v| v.checked_mul(64))
                    .ok_or_else(|| {
                        MapError::Budget("native donation replay tile plan overflow".into())
                    })?
            } else {
                0
            },
        )
        .ok_or_else(|| MapError::Budget("native donation replay plan overflow".into()))?
        .checked_add(
            terrain
                .decoder_transient_bytes_per_load()
                .checked_mul(req.threads as u64)
                .ok_or_else(|| MapError::Budget("sloping parallel decoder plan overflow".into()))?,
        )
        .ok_or_else(|| MapError::Budget("sloping decoder plan overflow".into()))?;
    // Retain the serial plan. Each additional slot conservatively reserves
    // output/work arrays and the existing full geometry/hydraulic allowance.
    // This is planned admission, not an OS-enforced RSS limit.
    let additional = (count as u64)
        .checked_mul(
            if req.options.render_mode == sloping::RenderMode::WithFaces {
                32
            } else {
                16
            },
        )
        .and_then(|v| {
            geometry_bytes
                .checked_mul(
                    if req.options.render_mode == sloping::RenderMode::WithFaces {
                        20
                    } else {
                        16
                    },
                )
                .and_then(|g| v.checked_add(g))
        })
        .ok_or_else(|| MapError::Budget("sloping concurrent profile plan overflow".into()))?;
    let (effective, admitted) = admit_profiles(
        req.max_inflight_profiles,
        req.threads,
        req.profiles.len(),
        req.max_working_bytes,
        admitted,
        additional,
    )?;
    fs::create_dir(out)?;
    let mut report = json!({"schema":"ras-commander-sloping-report/1","status":"in_progress", "method":selected_method(req.options),
        "threads":req.threads,"window":req.window,"planned_bytes":admitted,
        "requested_inflight_profiles":req.max_inflight_profiles,"effective_inflight_profiles":effective,
        "additional_profile_planned_bytes":additional,"profiles":[]});
    sink::write_json(&out.join("report.json"), &report)?;
    let execution = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
        || -> Result<(), MapError> {
            if let Some(s) = session.as_mut()
                && s.pool
                    .as_ref()
                    .is_some_and(|(threads, _)| *threads != req.threads)
            {
                s.pool = None;
            }
            let pool = if let Some((threads, pool)) = session
                .as_ref()
                .and_then(|s| s.pool.as_ref())
                .filter(|(threads, _)| *threads == req.threads)
            {
                let _ = threads;
                pool.clone()
            } else {
                Arc::new(
                    rayon::ThreadPoolBuilder::new()
                        .num_threads(req.threads)
                        .build()
                        .map_err(|e| MapError::Internal(e.to_string()))?,
                )
            };
            if let Some(s) = session.as_mut() {
                s.pool = Some((req.threads, pool.clone()));
            }
            let tick = Instant::now();
            let geometry_reused = session
                .as_ref()
                .is_some_and(|s| s.geometry.as_ref().is_some_and(|(old, _)| old == &key));
            if !geometry_reused && let Some(s) = session.as_mut() {
                s.geometry = None;
                s.hydraulics.lock().unwrap().clear();
                s.windows.clear();
            }
            let geometry = if geometry_reused {
                session
                    .as_ref()
                    .unwrap()
                    .geometry
                    .as_ref()
                    .unwrap()
                    .1
                    .clone()
            } else {
                let mut meshes = Vec::new();
                let mut stencils = Vec::new();
                for filename in &req.mesh_files {
                    let source = crate::sloping_binary::load(&model::resolve(base, filename))?;
                    let (mesh, stencil) = source.prepare()?;
                    meshes.push(mesh);
                    stencils.push(stencil);
                }
                Arc::new((meshes, stencils))
            };
            if let Some(s) = session.as_mut() {
                s.geometry = Some((key.clone(), geometry.clone()));
            }
            let (meshes, stencils) = &*geometry;
            let velocity_normals = req
                .velocity_normals_files
                .as_ref()
                .map(|files| -> Result<Vec<_>, MapError> {
                    files
                        .iter()
                        .zip(meshes)
                        .map(|(file, mesh)| {
                            crate::velocity::read_normals(
                                &model::resolve(base, file),
                                mesh.faces.len(),
                            )
                        })
                        .collect()
                })
                .transpose()?;
            if velocity_normals.is_some() {
                report["velocity"] = json!({"method": crate::velocity::method_record()});
            }
            report["session"] = json!({"terrain_reused":terrain_reused,"geometry_reused":geometry_reused,"retained_window_budget_bytes":persistent_bytes});
            for &[mesh, cell] in &req.owner_cells {
                if mesh >= meshes.len()
                    || cell >= meshes[mesh].cells.len()
                    || meshes[mesh].cells[cell].virtual_cell
                {
                    return Err(MapError::Invalid(
                        "sloping ownership requires physical mesh cells".into(),
                    ));
                }
            }
            report["geometry_prepare_seconds"] = tick.elapsed().as_secs_f64().into();
            report["ownership_prepare_seconds"] = 0.0.into();
            report["terrain_prepare_seconds"] = 0.0.into();
            let transform = [
                terrain.gt[0] + col as f64 * terrain.gt[1],
                terrain.gt[1],
                0.,
                terrain.gt[3] + row as f64 * terrain.gt[5],
                0.,
                terrain.gt[5],
            ];
            let grid = terrio::ops::rasterize::Grid {
                geo_transform: transform,
                cols: width,
                rows: height,
                crs: Default::default(),
            };
            let tick = Instant::now();
            let window_key = if session.is_some() {
                (
                    terrain_key.clone(),
                    key.clone(),
                    req.window,
                    req.owner_cells.clone(),
                    fs::read(model::resolve(base, &req.geometry_file))?,
                    req.aoi_file
                        .as_ref()
                        .map(|p| fs::read(model::resolve(base, p)))
                        .transpose()?,
                )
            } else {
                (
                    String::new(),
                    String::new(),
                    req.window,
                    Vec::new(),
                    Vec::new(),
                    None,
                )
            };
            let cached_window = session.as_mut().and_then(|s| s.windows.get(&window_key));
            let window_reused = cached_window.is_some();
            let mut data = if let Some(data) = cached_window {
                data
            } else {
                let cells = crate::ownership::load(
                    &model::resolve(base, &req.geometry_file),
                    req.owner_cells.len(),
                    false,
                )?;
                let mut owner = pool.install(|| crate::ownership::burn(cells, grid.clone()))?;
                if let Some(filename) = &req.aoi_file {
                    let mask = pool.install(|| {
                        crate::aoi::mask(&model::resolve(base, filename), terrain.gt, req.window)
                    })?;
                    pool.install(|| {
                        owner
                            .par_iter_mut()
                            .zip(mask.par_iter())
                            .for_each(|(label, &m)| {
                                if m == 0 {
                                    *label = 0;
                                }
                            })
                    });
                }
                report["ownership_prepare_seconds"] = tick.elapsed().as_secs_f64().into();
                let tick = Instant::now();
                let mut ground = vec![sloping::NODATA; count];
                let (bw, bh) = terrain.block;
                // Row stripes own disjoint output slices. Decodes run inside the same
                // bounded pool, and the shared single-flight cache handles boundary
                // blocks seen by adjacent stripes. Skip blocks outside ownership/AOI.
                let stripe_pixels = width
                    .checked_mul(bh)
                    .ok_or_else(|| MapError::Budget("sloping terrain stripe overflow".into()))?;
                pool.install(|| {
                    ground
                        .par_chunks_mut(stripe_pixels)
                        .enumerate()
                        .try_for_each(|(stripe, destination)| -> Result<(), MapError> {
                            let first = row + stripe * bh;
                            let last = first + destination.len() / width;
                            for by in first / bh..=(last - 1) / bh {
                                for bx in col / bw..=(col + width - 1) / bw {
                                    let win = terrain.block_window(bx, by);
                                    let r0 = first.max(win.y);
                                    let r1 = last.min(win.y + win.height);
                                    let c0 = col.max(win.x);
                                    let c1 = (col + width).min(win.x + win.width);
                                    if !(r0..r1).any(|r| {
                                        owner[(r - row) * width + c0 - col
                                            ..(r - row) * width + c1 - col]
                                            .iter()
                                            .any(|&label| label != 0)
                                    }) {
                                        continue;
                                    }
                                    let block = terrain.get_block(bx, by)?;
                                    for r in r0..r1 {
                                        for c in c0..c1 {
                                            if owner[(r - row) * width + c - col] == 0 {
                                                continue;
                                            }
                                            if let Some(value) = terrain.classify(
                                                block[(r - win.y) * win.width + c - win.x],
                                            ) {
                                                if !value.is_finite() {
                                                    return Err(MapError::Terrain(
                                                        "nonfinite sloping terrain".into(),
                                                    ));
                                                }
                                                destination[(r - first) * width + c - col] =
                                                    value as f32;
                                            }
                                        }
                                    }
                                }
                            }
                            Ok(())
                        })
                })?;
                report["terrain_prepare_seconds"] = tick.elapsed().as_secs_f64().into();
                Arc::new(WindowData {
                    owner,
                    ground,
                    weights: None,
                })
            };
            let window_base_bytes = count as u64 * 8
                + (2 * std::mem::size_of::<(WindowKey, Arc<WindowData>, u64)>()
                    + std::mem::size_of::<WindowData>()
                    + 2 * std::mem::size_of::<usize>()) as u64
                + window_key.0.len() as u64
                + window_key.1.len() as u64
                + (window_key.3.len() * std::mem::size_of::<[usize; 2]>()) as u64
                + window_key.4.len() as u64
                + window_key.5.as_ref().map_or(0, |v| v.len()) as u64;
            let weight_budget = persistent_bytes
                .saturating_sub(window_base_bytes)
                .min(32 << 20);
            let (bw, bh) = terrain.block;
            if session.is_some()
                && !window_reused
                && weight_budget > 0
                && req.options.render_mode != sloping::RenderMode::WithFaces
            {
                let window = sloping::RasterWindow {
                    source_transform: terrain.gt,
                    source_tile: [bw, bh],
                    width,
                    height,
                    transform,
                    owner: &data.owner,
                    terrain: &data.ground,
                    owner_cells: &req.owner_cells,
                };
                let weights = sloping::PixelWeights::prepare(
                    stencils,
                    &window,
                    weight_budget as usize,
                    &pool,
                )?;
                Arc::get_mut(&mut data).unwrap().weights = Some(weights);
            }
            let weight_bytes = data.weights.as_ref().map_or(0, |w| w.bytes());
            report["session"]["window_reused"] = window_reused.into();
            report["session"]["pixel_weight_cache_bytes"] = weight_bytes.into();
            let window_bytes = window_base_bytes + weight_bytes as u64;
            if let Some(s) = session.as_mut() {
                s.windows.insert(window_key, data.clone(), window_bytes);
                report["session"]["retained_window_bytes"] = s.windows.bytes().into();
                report["session"]["retained_window_count"] = s.windows.len().into();
            }
            let window = sloping::RasterWindow {
                source_transform: terrain.gt,
                source_tile: [bw, bh],
                width,
                height,
                transform,
                owner: &data.owner,
                terrain: &data.ground,
                owner_cells: &req.owner_cells,
            };
            let storage = req
                .storage_areas_file
                .as_ref()
                .map(|file| -> Result<_, MapError> {
                    let tick = Instant::now();
                    // The window cache retains the AOI-masked ownership only; the
                    // storage-area pixels use the same global-lattice mask.
                    let aoi = req
                        .aoi_file
                        .as_ref()
                        .map(|p| {
                            pool.install(|| {
                                crate::aoi::mask(&model::resolve(base, p), terrain.gt, req.window)
                            })
                        })
                        .transpose()?;
                    let mut pixels = crate::storage_area::prepare(
                        &model::resolve(base, file),
                        req.storage_area_count,
                        &grid,
                        &data.owner,
                        aoi.as_deref(),
                        &pool,
                    )?;
                    crate::storage_area::read_ground(&mut pixels, &terrain, req.window)?;
                    report["storage_areas"] = json!({
                        "method": crate::storage_area::method_record(),
                        "count": req.storage_area_count,
                        "pixels": pixels.pixels.len(),
                        "prepare_seconds": tick.elapsed().as_secs_f64(),
                    });
                    Ok(pixels)
                })
                .transpose()?;
            let foreign = match &req.terrain_scope {
                Some(scope) => {
                    let scoped = crate::terrain_scope::check(
                        scope,
                        base,
                        &terrain,
                        req.window,
                        &data.owner,
                        storage.as_ref().map_or(&[], |s| &s.pixels),
                    )?;
                    report["terrain_scope"] = scoped.record;
                    scoped.foreign
                }
                None => Vec::new(),
            };
            for batch in req.profiles.chunks(effective) {
                let results = map_batch(batch, &pool, |profile| {
                    render(
                        profile,
                        base,
                        meshes,
                        stencils,
                        &window,
                        &pool,
                        req.options,
                        data.weights.as_ref(),
                        velocity_normals.as_deref(),
                        &foreign,
                        session
                            .as_ref()
                            .filter(|_| hydraulic_bytes > 0)
                            .map(|s| &s.hydraulics),
                        storage.as_ref(),
                    )
                })?;
                // Indexed collection and ordered emission preserve the pipe/report
                // contract even when later profiles finish first.
                for (profile, rendered) in batch.iter().zip(results) {
                    let RenderedProfile {
                        depth,
                        wse,
                        velocity,
                        hydraulic_seconds,
                        render_seconds,
                        hydraulic_reused,
                        finite_pixels,
                        flooded_pixels,
                    } = rendered;
                    let dir = out.join(&profile.id);
                    fs::create_dir(&dir)?;
                    if req.stream_outputs {
                        let mut stream =
                            std::io::BufWriter::with_capacity(65536, std::io::stdout().lock());
                        let mut header =
                            json!({"id":profile.id,"status":"complete","pixels":count});
                        if velocity.is_some() {
                            header["velocity"] = true.into();
                        }
                        if req.geotiff_outputs {
                            let tick = Instant::now();
                            header["products"] = "native-geotiff/1".into();
                            header["depth_finite_pixels"] = finite_pixels.into();
                            header["flooded_pixels"] = flooded_pixels.into();
                            crate::geotiff::write(
                                &dir.join("depth.tif"),
                                depth,
                                width,
                                height,
                                transform,
                                req.epsg,
                            )?;
                            crate::geotiff::write(
                                &dir.join("wse.tif"),
                                wse,
                                width,
                                height,
                                transform,
                                req.epsg,
                            )?;
                            header["write_seconds"] = tick.elapsed().as_secs_f64().into();
                            writeln!(stream, "{header}")?;
                            stream.flush()?;
                        } else {
                            writeln!(stream, "{header}")?;
                            crate::horizontal::write_f32(&mut stream, &depth)?;
                            crate::horizontal::write_f32(&mut stream, &wse)?;
                            if let Some(velocity) = &velocity {
                                crate::horizontal::write_f32(&mut stream, velocity)?;
                            }
                            stream.flush()?;
                        }
                    } else {
                        crate::horizontal::write_values(&dir.join("depth.bin"), &depth)?;
                        crate::horizontal::write_values(&dir.join("wse.bin"), &wse)?;
                        if let Some(velocity) = &velocity {
                            crate::horizontal::write_values(&dir.join("velocity.bin"), velocity)?;
                        }
                    }
                    let unit = json!({"id":profile.id,"status":"complete","hydraulic_seconds":hydraulic_seconds,"render_seconds":render_seconds,"hydraulic_reused":hydraulic_reused});
                    sink::write_json(&dir.join("unit.json"), &unit)?;
                    report["profiles"].as_array_mut().unwrap().push(unit);
                    sink::write_json(&out.join("report.json"), &report)?;
                }
            }
            if let Some(s) = session.as_ref() {
                let cache = s.hydraulics.lock().unwrap();
                report["session"]["retained_hydraulic_bytes"] = cache.bytes().into();
                report["session"]["retained_hydraulic_count"] = cache.len().into();
            }
            Ok(())
        },
    ));
    match execution {
        Ok(Ok(())) => report["status"] = "complete".into(),
        other => {
            report["status"] = "aborted".into();
            report["error"] = match other {
                Ok(Err(e)) => e.to_string(),
                _ => "sloping internal panic".into(),
            }
            .into();
            for profile in &req.profiles {
                let dir = out.join(&profile.id);
                for filename in [
                    "depth.bin",
                    "wse.bin",
                    "depth.bin.partial",
                    "wse.bin.partial",
                    "depth.tif",
                    "wse.tif",
                ] {
                    let _ = fs::remove_file(dir.join(filename));
                }
                if dir.is_dir() {
                    sink::write_json(
                        &dir.join("unit.json"),
                        &json!({"id":profile.id,"status":"failed","error":"session aborted"}),
                    )?;
                }
            }
            report["profiles"] = req
                .profiles
                .iter()
                .map(|p| json!({"id":p.id,"status":"failed","error":"session aborted"}))
                .collect::<Vec<_>>()
                .into();
        }
    }
    report["total_seconds"] = started.elapsed().as_secs_f64().into();
    report["terrain_cache"] = json!(terrain.stats());
    sink::write_json(&out.join("report.json"), &report)?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Barrier,
        atomic::{AtomicUsize, Ordering},
    };

    #[test]
    fn admission_caps_threads_profiles_memory_and_overflow() {
        assert_eq!(admit_profiles(8, 4, 6, 1000, 100, 50).unwrap(), (4, 250));
        assert_eq!(admit_profiles(8, 4, 2, 1000, 100, 50).unwrap(), (2, 150));
        assert_eq!(admit_profiles(8, 4, 6, 149, 100, 50).unwrap(), (1, 100));
        assert!(admit_profiles(8, 4, 6, 99, 100, 50).is_err());
        assert_eq!(
            admit_profiles(64, 64, 64, u64::MAX, u64::MAX - 1, u64::MAX).unwrap(),
            (1, u64::MAX - 1)
        );
        assert!(admit_profiles(0, 1, 1, 100, 50, 10).is_err());
    }

    #[test]
    fn batches_really_overlap_without_exceeding_bound_and_keep_order() {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        let active = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let barrier = Barrier::new(2);
        let mut output = Vec::new();
        for batch in [0, 1, 2, 3].chunks(2) {
            output.extend(
                map_batch(batch, &pool, |&i| {
                    let n = active.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(n, Ordering::SeqCst);
                    barrier.wait();
                    active.fetch_sub(1, Ordering::SeqCst);
                    Ok(i)
                })
                .unwrap(),
            );
        }
        assert_eq!(peak.load(Ordering::SeqCst), 2);
        assert_eq!(output, vec![0, 1, 2, 3]);
        assert!(
            map_batch(&[0, 1], &pool, |_| -> Result<(), MapError> {
                Err(MapError::Invalid("failed profile".into()))
            })
            .is_err()
        );
    }
}
