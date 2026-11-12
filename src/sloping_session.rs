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

type PinnedHydraulics = Mutex<Option<(String, Arc<Vec<sloping::PreparedProfile>>)>>;

/// Complete geometry, or the compact stencil closure of a staged cell subset.
type CachedGeometry = (
    Vec<Mesh>,
    Vec<Vec<Option<sloping::CellStencil>>>,
    Option<StagedGeometry>,
);
struct StagedGeometry {
    /// Per mesh: staged global cell ids; compact cell i is staged[i].
    cells: Vec<Vec<u32>>,
    equivalent_bytes: u64,
    record: Value,
}
type WindowKey = (
    String,
    String,
    [usize; 4],
    Vec<[usize; 2]>,
    Vec<u8>,
    Option<Vec<u8>>,
    bool,
);
struct WindowData {
    owner: Vec<u32>,
    ground: Vec<f32>,
    validity: Option<Vec<bool>>,
    weights: Option<sloping::PixelWeights>,
}

/// One explicitly owned, serialized native session. One complete geometry and
/// terrain share bounded LRU window and hydraulic caches. Dropping releases all.
#[derive(Default)]
pub struct PreparedSession {
    pinned_hydraulics: PinnedHydraulics,
    grouped_profile: bool,
    terrain_preparations: u64,
    geometry_preparations: u64,
    terrain: Option<(String, Arc<NativeTerrain>)>,
    geometry: Option<(String, Arc<CachedGeometry>)>,
    /// Admitted resident allowance of `geometry`, kept beside a staged load
    /// until the new geometry is admitted.
    geometry_resident: u64,
    windows: Retained<WindowKey, WindowData>,
    hydraulics: Mutex<Retained<String, Vec<sloping::PreparedProfile>>>,
    pool: Option<(usize, Arc<rayon::ThreadPool>)>,
}

impl PreparedSession {
    pub(crate) fn preparation_counts(&self) -> (u64, u64) {
        (self.terrain_preparations, self.geometry_preparations)
    }

    /// Drop geometry a caller prepared for a request it then refused.
    pub(crate) fn release_geometry(&mut self) {
        self.geometry = None;
        self.geometry_resident = 0;
        *self.pinned_hydraulics.lock().unwrap() = None;
        self.hydraulics.lock().unwrap().clear();
        self.windows.clear();
    }

    pub(crate) fn install<R: Send>(
        &self,
        work: impl FnOnce() -> Result<R, MapError> + Send,
    ) -> Result<R, MapError> {
        self.pool
            .as_ref()
            .ok_or_else(|| MapError::Internal("session has no pool".into()))?
            .1
            .install(work)
    }
    pub(crate) fn begin_grouped_profile(&mut self) {
        self.grouped_profile = true;
        *self.pinned_hydraulics.lock().unwrap() = None;
    }
    pub(crate) fn end_grouped_profile(&mut self) {
        self.grouped_profile = false;
        *self.pinned_hydraulics.lock().unwrap() = None;
    }
}

pub(crate) fn identity(path: &Path) -> Result<String, MapError> {
    let path = path.canonicalize()?;
    let metadata = path.metadata()?;
    Ok(format!(
        "{:?}:{}:{:?}",
        path,
        metadata.len(),
        metadata.modified()?
    ))
}
pub(crate) fn geometry_key(req: &Request, base: &Path) -> Result<String, MapError> {
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
    // A staged subset is a different compact geometry, even over the same mesh,
    // and its stencils exist only for the owner cells it was loaded for.
    if let Some(files) = &req.staged_cells_files {
        for filename in files {
            key.push_str("|staged:");
            key.push_str(&identity(&model::resolve(base, filename))?);
        }
        let mut owners = req.owner_cells.clone();
        owners.sort_unstable();
        owners.dedup();
        let encoded: Vec<u8> = owners
            .iter()
            .flat_map(|&[m, c]| {
                (m as u64)
                    .to_le_bytes()
                    .into_iter()
                    .chain((c as u64).to_le_bytes())
            })
            .collect();
        key.push_str(&format!(
            "|owners:{}:{:016x}",
            owners.len(),
            crate::sink::fnv1a64(&encoded)
        ));
    }
    Ok(key)
}

pub const SCHEMA: &str = "ras-commander-sloping-request/1";
pub const NATIVE_GRID_SCHEMA: &str = "ras-commander-sloping-request/2";

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub schema: String,
    #[serde(default)]
    pub grid_contract: Option<crate::stored_grid::Contract>,
    #[serde(default)]
    pub options: sloping::Options,
    pub terrain: String,
    /// EPSG code of the terrain and products; 0 or absent with `crs_geokeys`.
    #[serde(default)]
    pub epsg: u32,
    /// Terrain and product CRS as verbatim GeoKey tags, for a CRS without an
    /// EPSG code (see [`crate::crs`]).
    #[serde(default)]
    pub crs_geokeys: Option<crate::crs::GeoKeys>,
    pub crs_identity: CrsIdentity,
    pub terrain_transform: [f64; 6],
    pub window: [usize; 4],
    pub geometry_file: String,
    #[serde(default)]
    pub aoi_file: Option<String>,
    /// Export unfiltered physical polygon labels, independent of wetness/terrain nodata.
    #[serde(default)]
    pub export_physical_ownership: bool,
    pub mesh_files: Vec<String>,
    pub owner_cells: Vec<[usize; 2]>,
    /// Per mesh: a `<u4` file of strictly ascending global cell ids
    /// (`ras-commander-staged-cells/1`). With it every profile values file
    /// holds exactly those cells' WSE in that order, and geometry is the
    /// compact stencil closure of `owner_cells` (which stay global). Absent,
    /// the complete mesh is loaded and staged as before.
    #[serde(default)]
    pub staged_cells_files: Option<Vec<String>>,
    /// Internal: budget for a staged load when `max_working_bytes` is only a
    /// planning probe (grouped admission). Never deserialized.
    #[serde(skip)]
    pub(crate) load_budget: Option<u64>,
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
    /// Depth that clips velocity and feeds `derived_products`: the rendered
    /// depth (default) or the horizontal cell-constant depth.
    #[serde(default)]
    pub velocity_depth: crate::velocity::VelocityDepth,
    /// Per-profile products from that depth and velocity, in
    /// `velocity::DERIVED` names (requires velocity).
    #[serde(default)]
    pub derived_products: Vec<String>,
    /// Float32 gravity for `froude` in model units (32.174 or 9.80665).
    #[serde(default)]
    pub gravity: Option<f32>,
    /// Pixelwise maximum of each derived product over every profile, written
    /// to `max/`; per-profile products are then not written or streamed.
    #[serde(default)]
    pub reduce_max: bool,
    /// Max shear stress (`rasmapper-face-shear-max/1`): per-mesh face tables
    /// and stored output-time series; the request's single profile must be
    /// the Max profile (cell `Maximum Water Surface`).
    #[serde(default)]
    pub shear_stress: Option<crate::shear::Request>,

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
    derived: Vec<Vec<f32>>,
    shear: Option<Vec<f32>>,
    shear_seconds: f64,
    hydraulic_seconds: f64,
    render_seconds: f64,
    finalize_seconds: f64,
    velocity_seconds: f64,
    derived_seconds: f64,
    storage_render_seconds: f64,
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
    pinned: Option<&PinnedHydraulics>,
    storage: Option<&crate::storage_area::StorageAreaPixels>,
    products: &DerivedPlan<'_>,
    shear: Option<&ShearPlan>,
    source_shape: [usize; 2],
) -> Result<RenderedProfile, MapError> {
    let tick = Instant::now();
    let needs_cells = normals.is_some() || shear.is_some();
    let cache_key = if (hydraulic_cache.is_some() || pinned.is_some()) && !needs_cells {
        let mut key =
            serde_json::to_string(&options).map_err(|e| MapError::Invalid(e.to_string()))?;
        for file in &profile.values_files {
            key.push_str(&identity(&model::resolve(base, file))?);
        }
        Some(key)
    } else {
        None
    };
    let pinned_value = pinned.and_then(|p| {
        let guard = p.lock().unwrap();
        guard
            .as_ref()
            .filter(|(key, _)| Some(key) == cache_key.as_ref())
            .map(|(_, p)| p.clone())
    });
    let cached = pinned_value.or_else(|| match (&cache_key, hydraulic_cache) {
        (Some(key), Some(cache)) => cache.lock().unwrap().get(key),
        _ => None,
    });
    let hydraulic_reused = cached.is_some();
    let mut new_prepared = Vec::new();
    let mut cell_values = Vec::new();
    if !hydraulic_reused {
        for ((mesh, stencil), filename) in meshes.iter().zip(stencils).zip(&profile.values_files) {
            let wse = values(&model::resolve(base, filename), mesh.cells.len())?;
            // Only a staged mesh without owner cells has no faces; nothing reads it.
            new_prepared.push(if mesh.faces.is_empty() {
                sloping::PreparedProfile::unread(options)
            } else {
                sloping::prepare_profile_with_options(mesh, stencil, &wse, 0.001, pool, options)?
            });
            if needs_cells {
                cell_values.push(wse);
            }
        }
    }
    let prepared = cached.unwrap_or_else(|| Arc::new(new_prepared));
    if let (Some(key), Some(pin)) = (&cache_key, pinned) {
        *pin.lock().unwrap() = Some((key.clone(), prepared.clone()));
    }
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
    let tick = Instant::now();
    let (mut finite_pixels, mut flooded_pixels) =
        crate::finalize::apply(&mut depth, &mut wse, pool);
    let finalize_seconds = tick.elapsed().as_secs_f64();
    // Keep render_seconds scoped to the existing 2D render. Pool timing includes
    // profile-value reads, rendering and the storage flooded-pixel count.
    let mut storage_render_seconds = 0.0;
    if let Some(storage) = storage {
        let tick = Instant::now();
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
        storage_render_seconds = tick.elapsed().as_secs_f64();
    }
    let mut derived = Vec::new();
    let mut velocity_seconds = 0.0;
    let mut derived_seconds = 0.0;
    let velocity = match (normals, &profile.face_velocity_files) {
        (Some(normals), Some(files)) => {
            let velocity_tick = Instant::now();
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
            let horizontal;
            let clip = match products.depth {
                crate::velocity::VelocityDepth::Rendered => &depth,
                crate::velocity::VelocityDepth::Horizontal => {
                    horizontal = crate::velocity::horizontal_depth_in_pool(
                        window,
                        meshes,
                        &cell_values,
                        pool,
                    )?;
                    &horizontal
                }
            };
            let velocity =
                crate::velocity::render_on_source_grid(&inputs, window, clip, pool, source_shape)?;
            velocity_seconds = velocity_tick.elapsed().as_secs_f64();
            let derived_tick = Instant::now();
            derived = crate::velocity::derive_all_in_pool(
                products.names,
                clip,
                &velocity,
                products.gravity,
                pool,
            )?;
            derived_seconds = derived_tick.elapsed().as_secs_f64();
            Some(velocity)
        }
        _ => None,
    };
    let mut shear_seconds = 0.0;
    let shear = match shear {
        Some(plan) => {
            let shear_tick = Instant::now();
            let clip =
                crate::shear::clip_depth(plan.depth, &depth, window, meshes, &cell_values, pool)?;
            let inputs: Vec<_> = meshes
                .iter()
                .zip(stencils)
                .zip(&cell_values)
                .zip(&plan.facepoints)
                .map(|(((mesh, stencil), wse), fp)| {
                    (mesh, stencil.as_slice(), wse.as_slice(), fp.as_slice())
                })
                .collect();
            let values = crate::shear::render(&inputs, window, &clip, pool, source_shape)?;
            shear_seconds = shear_tick.elapsed().as_secs_f64();
            Some(values)
        }
        None => None,
    };
    Ok(RenderedProfile {
        depth,
        wse,
        velocity,
        derived,
        shear,
        shear_seconds,
        hydraulic_seconds,
        render_seconds,
        finalize_seconds,
        velocity_seconds,
        derived_seconds,
        storage_render_seconds,
        hydraulic_reused,
        finite_pixels,
        flooded_pixels,
    })
}

/// Max shear stress of one request: facepoint values per mesh and the clip.
struct ShearPlan {
    depth: crate::velocity::VelocityDepth,
    facepoints: Vec<Vec<f32>>,
}

/// Validates the shear-stress block before any read.
fn validate_shear(req: &Request) -> Result<(), MapError> {
    let Some(shear) = &req.shear_stress else {
        return Ok(());
    };
    crate::shear::factor(&shear.units)?;
    if shear.meshes.len() != req.mesh_files.len() {
        return Err(MapError::Invalid(
            "shear_stress needs one mesh entry per mesh file".into(),
        ));
    }
    if shear
        .meshes
        .iter()
        .any(|m| m.times == 0 || m.table_rows == 0)
    {
        return Err(MapError::Invalid(
            "shear_stress needs stored output times and face tables".into(),
        ));
    }
    if req.profiles.len() != 1
        || req.options != sloping::Options::default()
        || req.reduce_max
        || req.geotiff_outputs
        || req.storage_areas_file.is_some()
        || req.storage_area_count != 0
    {
        return Err(MapError::Unsupported(
            "shear stress is the Max profile only, on plain sloping without storage areas, reduction or native GeoTIFF".into(),
        ));
    }
    Ok(())
}

/// Velocity clipping depth and derived products of one request.
struct DerivedPlan<'a> {
    depth: crate::velocity::VelocityDepth,
    names: &'a [String],
    gravity: f32,
}

/// Validates the velocity-derived controls before any read.
fn validate_derived(req: &Request) -> Result<(), MapError> {
    let velocity = req.velocity_normals_files.is_some();
    let names = &req.derived_products;
    let mut seen = HashSet::new();
    if names
        .iter()
        .any(|n| !crate::velocity::DERIVED.contains(&n.as_str()) || !seen.insert(n))
    {
        return Err(MapError::Invalid(format!(
            "derived_products must be unique names from {:?}",
            crate::velocity::DERIVED
        )));
    }
    if !velocity
        && (!names.is_empty()
            || req.reduce_max
            || req.velocity_depth != crate::velocity::VelocityDepth::Rendered)
    {
        return Err(MapError::Invalid(
            "velocity_depth, derived_products and reduce_max require velocity".into(),
        ));
    }
    if names.iter().any(|n| n == "froude") && !req.gravity.is_some_and(|g| g.is_finite() && g > 0.0)
    {
        return Err(MapError::Invalid(
            "froude requires a finite positive gravity".into(),
        ));
    }
    if req.reduce_max && (names.is_empty() || req.stream_outputs || req.geotiff_outputs) {
        return Err(MapError::Invalid(
            "reduce_max requires derived_products and file outputs".into(),
        ));
    }
    Ok(())
}

#[derive(Clone, Deserialize)]
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
                sloping::set_physical_ring(
                    stencil,
                    cell,
                    self.cells[cell].point,
                    self.cells[cell].faces.iter().map(|&face| {
                        let f = &self.faces[face];
                        (f.cells, f.polyline.as_slice())
                    }),
                );
            }
        }
        if let Some(elevations) = &self.vertex_elevations {
            sloping::set_vertex_elevations(&mut stencils, elevations, mesh.corners.len())?;
        }
        Ok((mesh, stencils))
    }
}

/// Validate and load (or reuse) the compact geometry of a staged cell subset
/// before the array plan, which sizes geometry from the compact rows. Every
/// load allocation is charged before it is made against the load budget less
/// what the session keeps resident meanwhile (`resident`). Nothing is stored
/// in the session here: the caller installs the geometry only once the plan is
/// admitted. Returns the geometry and whether this call prepared it.
fn staged_geometry(
    req: &Request,
    files: &[String],
    base: &Path,
    session: Option<&PreparedSession>,
    key: &str,
    resident: u64,
) -> Result<(Arc<CachedGeometry>, bool), MapError> {
    if files.len() != req.mesh_files.len() {
        return Err(MapError::Invalid(
            "staged_cells_files needs one file per mesh".into(),
        ));
    }
    if req.velocity_normals_files.is_some()
        || req.shear_stress.is_some()
        || req.storage_areas_file.is_some()
        || req.storage_area_count != 0
        || req.terrain_scope.is_some()
        || req.export_physical_ownership
        || req.grid_contract.is_some()
        || req.reduce_max
        || !req.derived_products.is_empty()
    {
        return Err(MapError::Unsupported(
            "staged_cells is qualified for sloping depth/WSE without velocity, shear, storage areas, terrain scope, stored grid or ownership export".into(),
        ));
    }
    if let Some(s) = session
        && let Some((old, geometry)) = &s.geometry
        && old == key
    {
        return Ok((geometry.clone(), false));
    }
    let allowed = req.load_budget.unwrap_or(req.max_working_bytes);
    let limit = allowed.checked_sub(resident).ok_or_else(|| {
        MapError::Budget(format!(
            "staged geometry load: resident caches need {resident} bytes, allowed {allowed} bytes"
        ))
    })?;
    let budget = crate::sloping_binary::LoadBudget::new(limit);
    let mut owners = vec![Vec::new(); req.mesh_files.len()];
    for &[mesh, cell] in &req.owner_cells {
        owners
            .get_mut(mesh)
            .ok_or_else(|| {
                MapError::Invalid("sloping ownership requires physical mesh cells".into())
            })?
            .push(cell);
    }
    let mut meshes = Vec::new();
    let mut stencils = Vec::new();
    let mut cells = Vec::new();
    let mut records = Vec::new();
    let mut equivalent_bytes = 0_u64;
    for ((mesh, staged), mut owned) in req.mesh_files.iter().zip(files).zip(owners) {
        owned.sort_unstable();
        owned.dedup();
        let staged = crate::sloping_binary::read_staged(&model::resolve(base, staged), &budget)?;
        let compact = crate::sloping_binary::load_compact(
            &model::resolve(base, mesh),
            staged,
            &owned,
            &budget,
        )?;
        records.push(
            json!({"staged_cells":compact.staged.len(),"owner_cells":owned.len(),
            "faces":compact.mesh.faces.len(),"corners":compact.mesh.corners.len(),
            "parent_cells":compact.parent[0],"parent_faces":compact.parent[1],
            "parent_corners":compact.parent[2],"equivalent_bytes":compact.equivalent_bytes}),
        );
        equivalent_bytes = equivalent_bytes
            .checked_add(compact.equivalent_bytes)
            .ok_or_else(|| MapError::Budget("staged geometry size overflow".into()))?;
        meshes.push(compact.mesh);
        stencils.push(compact.stencils);
        cells.push(compact.staged);
    }
    let record = json!({"method":crate::sloping_binary::STAGED_METHOD,
        "load_budget_bytes":limit,"load_peak_charged_bytes":budget.peak(),
        "resident_charged_bytes":budget.used(),"meshes":records});
    Ok((
        Arc::new((
            meshes,
            stencils,
            Some(StagedGeometry {
                cells,
                equivalent_bytes,
                record,
            }),
        )),
        true,
    ))
}

pub fn method_record() -> Value {
    let mut record = json!({"method_id":"rasmapper-plain-sloping/1", "backend":"rust", "experimental":true,
        "native_version_qualified":"7.0.1", "stream_outputs":true, "export_physical_ownership":true, "native_ownership":true,"aoi_mask":crate::aoi::METHOD,
        "enhanced_options":true,"enhanced_method_id":"rasmapper-sloping-with-faces/1",
        "depth_weighted_faces":true,
        "native_donation_history":true,
        "native_row_streaming":{"control":"row_stream_rows","default":0,"max":4096,
            "render_mode":"sloping","products":["depth","wse"],
            "unsupported":["slopingPretty","velocity","storage","reduce_max","consumer"],
            "retained_full_window_buffers":["ownership","terrain"]},
        "bounded_profile_concurrency":true,
        "persistent_session":true,"multi_window_cache":true,"hydraulic_cache":true,"native_geotiff_outputs":true,"native_output_products":["depth","wse"],"native_output_compression":["NONE","LZW"],"bounded_pixel_weights":true,
        "native_final_format":{"control":"final_format","compression":"DEFLATE","layout":"tiled",
            "block_size":{"default":256,"multiple_of":16,"min":16,"max":4096},
            "zlevel":{"default":6,"min":1,"max":9},"nodata":"finite Float32","full_grid":true,
            "overview_resampling":["average"],"overview_factors":"consecutive powers of two from 2",
            "overview_arithmetic":"gdal-average-cascade/1","shared_nodata_tile":true,
            "row_stream_rows":true},
        "velocity":crate::velocity::METHOD_ID,
        "velocity_depth":["rendered","horizontal"],
        "velocity_render_modes":["horizontal","sloping","slopingPretty"],
        "velocity_with_faces":crate::velocity::WITH_FACES_METHOD_ID,
        "velocity_with_faces_options":["reduce_shallow_to_horizontal","use_depth_weighted_faces"],
        "derived_products":crate::velocity::DERIVED,
        "derived_method_id":crate::velocity::DERIVED_METHOD_ID,
        "derived_max":"reduce_max/1",
        "shear_stress":crate::shear::METHOD_ID,
        "shear_stress_depth":["rendered","horizontal"],

        "terrain_scope":crate::terrain_scope::METHOD_ID,
        "storage_area_surfaces":crate::storage_area::METHOD_ID,
        "options":{"CellRenderMode":"Sloping","CellStencilMode":"JustFacepoints",
            "FacepointAdjustmentMode":"None","ShallowBehaviorMode":"None",
            "UseDepthWeightedFaces":false,"FaceWSMode":"Adjusted"},
        "native_plot_tolerance":0.001_f32,"extra_depth_cutoff":null,
        "scope":"2D cells; plain cell-corner sloping; optional storage-area level pools; no 1D, slopingPretty or additional rendering options",
        "reuse":"geometry, terrio ownership and terrain prepared once; bounded Rayon pixel and reconstruction workers"});
    record["staged_cells"] = crate::sloping_binary::STAGED_METHOD.into();
    crate::crs::advertise(&mut record);
    record
}

pub fn native_grid_method_record() -> Value {
    let mut record = json!({"method_id":"rasmapper-plain-sloping/2","backend":"rust","experimental":true,
        "native_parity_qualified":false,"grid_contracts":[crate::stored_grid::ID],
        "stream_outputs":true,"export_physical_ownership":true,"native_ownership":true,
        "aoi_mask":crate::aoi::METHOD,"bounded_profile_concurrency":true,
        "persistent_session":true,"multi_window_cache":true,"hydraulic_cache":true,
        "bounded_pixel_weights":true,"native_geotiff_outputs":true,
        "native_output_products":["depth","wse"],"native_output_compression":["NONE","LZW"],
        "native_row_streaming":{"control":"row_stream_rows","default":0,"max":4096,
            "render_mode":"sloping","products":["depth","wse"],
            "unsupported":["slopingPretty","velocity","storage","reduce_max","consumer"],
            "retained_full_window_buffers":["ownership","terrain"]},
        "native_plot_tolerance":0.001_f32,"scope":"explicit original single-source 256-tile plain corner Depth/WSE; no terrain scope, storage or velocity"});
    crate::crs::advertise(&mut record);
    record
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

// A fixed buffer is admitted before input reads; no full-window serialization clone.
const OWNERS_IO_BYTES: usize = 64 * 1024;
fn write_physical_ownership(path: &Path, owner: &[u32]) -> Result<(), MapError> {
    let partial = path.with_extension("bin.partial");
    let mut file = fs::File::create(&partial)?;
    let mut bytes = [0u8; OWNERS_IO_BYTES];
    for labels in owner.chunks(OWNERS_IO_BYTES / 4) {
        for (label, destination) in labels.iter().zip(bytes.as_chunks_mut::<4>().0) {
            destination.copy_from_slice(&label.to_le_bytes());
        }
        file.write_all(&bytes[..labels.len() * 4])?;
    }
    file.sync_all()?;
    drop(file);
    fs::rename(partial, path)?;
    Ok(())
}

/// Commit a session report, removing exported ownership if publication fails.
fn commit_report(out: &Path, report: &Value, exported_owners: bool) -> Result<(), MapError> {
    let result = sink::write_json(&out.join("report.json"), report);
    if result.is_err() && exported_owners {
        let _ = fs::remove_file(out.join("owners.bin"));
        let _ = fs::remove_file(out.join("owners.bin.partial"));
    }
    result
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
    session: Option<&mut PreparedSession>,
) -> Result<Value, MapError> {
    run_with_output_options(req, base, out, session, None)
}

/// Run with optional GeoTIFF product/compression controls. Existing callers keep defaults.
pub fn run_with_output_options(
    req: &Request,
    base: &Path,
    out: &Path,
    session: Option<&mut PreparedSession>,
    output: Option<&crate::native_output::OutputOptions>,
) -> Result<Value, MapError> {
    run_internal(req, base, out, session, output, None, false)
}

pub(crate) type FrameSink<'a> = dyn FnMut(&Profile, Vec<f32>, Value) -> Result<(), MapError> + 'a;

pub(crate) fn run_to_sink(
    req: &Request,
    base: &Path,
    out: &Path,
    session: &mut PreparedSession,
    consumer: &mut FrameSink<'_>,
) -> Result<Value, MapError> {
    run_internal(req, base, out, Some(session), None, Some(consumer), false)
}

pub(crate) fn plan_with_session(
    req: &Request,
    base: &Path,
    session: &mut PreparedSession,
) -> Result<u64, MapError> {
    Ok(
        run_internal(req, base, Path::new(""), Some(session), None, None, true)?["planned_bytes"]
            .as_u64()
            .unwrap(),
    )
}

fn run_internal(
    req: &Request,
    base: &Path,
    out: &Path,
    mut session: Option<&mut PreparedSession>,
    output: Option<&crate::native_output::OutputOptions>,
    mut consumer: Option<&mut FrameSink<'_>>,
    plan_only: bool,
) -> Result<Value, MapError> {
    let native_grid = match (req.schema.as_str(), req.grid_contract.as_ref()) {
        (SCHEMA, None) => None,
        (NATIVE_GRID_SCHEMA, Some(contract)) => {
            if !crate::stored_grid::same(contract.source_geo_transform, req.terrain_transform)
                || req.options != sloping::Options::default()
                || req.terrain_scope.is_some()
                || req.storage_areas_file.is_some()
                || req.storage_area_count != 0
                || req.velocity_normals_files.is_some()
                || !req.derived_products.is_empty()
                || req.gravity.is_some()
                || req.reduce_max
                || req.shear_stress.is_some()
                || consumer.is_some()
                || req.profiles.iter().any(|p| {
                    p.storage_area_values_file.is_some() || p.face_velocity_files.is_some()
                })
            {
                return Err(MapError::Unsupported("stored tile grid requires original plain corner Depth/WSE without storage, terrain scope, velocity or consumer".into()));
            }
            Some(contract.validate(req.window)?)
        }
        _ => {
            return Err(MapError::Unsupported(
                "sloping schema and stored grid contract must be selected together".into(),
            ));
        }
    };
    if req.export_physical_ownership
        && (req.aoi_file.is_some()
            || req.terrain_scope.is_some()
            || req.storage_areas_file.is_some()
            || req.storage_area_count != 0
            || req
                .profiles
                .iter()
                .any(|p| p.storage_area_values_file.is_some())
            || consumer.is_some())
    {
        return Err(MapError::Unsupported(
            "export_physical_ownership requires unfiltered physical geometry without AOI, terrain_scope, embedded storage or consumer".into(),
        ));
    }
    if let Some(output) = output {
        output.validate()?;
        if !req.geotiff_outputs {
            return Err(MapError::Unsupported(
                "output options require native geotiff_outputs".into(),
            ));
        }
    }
    let defaults = crate::native_output::OutputOptions::default();
    let selected = output.unwrap_or(&defaults);
    let deliver_files = consumer.is_none();
    let row_stream_rows = selected.row_stream_rows;
    if row_stream_rows > 0
        && (!req.geotiff_outputs
            || !req.stream_outputs
            || req.options.render_mode != sloping::RenderMode::Corners
            || req.velocity_normals_files.is_some()
            || req.storage_areas_file.is_some()
            || req.reduce_max
            || req.shear_stress.is_some()
            || consumer.is_some())
    {
        return Err(MapError::Unsupported("row streaming requires plain-corner native GeoTIFF without velocity, storage, reduction or consumer".into()));
    }

    req.options.validate()?;
    validate_derived(req)?;
    validate_shear(req)?;
    if req.shear_stress.is_some() && consumer.is_some() {
        return Err(MapError::Unsupported(
            "shear stress is delivered as files or streams, not to a consumer".into(),
        ));
    }
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
    // Final delivery placement is resolved from the request alone, before any read.
    let final_layout = selected
        .final_format
        .as_ref()
        .map(|format| {
            let gt = req.terrain_transform;
            crate::final_tiff::Layout::new(
                format,
                width,
                height,
                [
                    gt[0] + col as f64 * gt[1],
                    gt[1],
                    0.,
                    gt[3] + row as f64 * gt[5],
                    0.,
                    gt[5],
                ],
            )
        })
        .transpose()?;
    // Final delivery is qualified against terrain-derived framing only.
    if final_layout.is_some() && (native_grid.is_some() || req.shear_stress.is_some()) {
        return Err(MapError::Unsupported(
            "final_format is not qualified with the stored tile grid contract or shear stress"
                .into(),
        ));
    }
    let crs = crate::crs::OutputCrs::resolve(req.epsg, req.crs_geokeys.as_ref())?;
    if final_layout.is_some() {
        // Same refusal as final_tiff::Writer::create, before any read.
        crate::final_tiff::check_crs(&crs)?;
    }
    if !matches!(req.schema.as_str(), SCHEMA | NATIVE_GRID_SCHEMA)
        || !(1..=64).contains(&req.threads)
        || !(1..=64).contains(&req.max_inflight_profiles)
        || crs == crate::crs::OutputCrs::Epsg(0)
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
    let cache = 64_u64 << 20;
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
    let staged = match &req.staged_cells_files {
        Some(files) => {
            if files.len() != req.mesh_files.len() {
                return Err(MapError::Invalid(
                    "staged_cells_files needs one file per mesh".into(),
                ));
            }
            let key = geometry_key(req, base)?;
            // Resident while it loads: the terrain block cache, the retained
            // window/hydraulic budgets and any current geometry it may replace.
            let old = session
                .as_ref()
                .filter(|s| s.geometry.as_ref().is_some_and(|(k, _)| k != &key))
                .map_or(0, |s| s.geometry_resident);
            let resident = cache
                .checked_add(persistent_bytes)
                .and_then(|v| v.checked_add(hydraulic_bytes))
                .and_then(|v| v.checked_add(old))
                .ok_or_else(|| MapError::Budget("staged geometry plan overflow".into()))?;
            Some(staged_geometry(
                req,
                files,
                base,
                session.as_deref(),
                &key,
                resident,
            )?)
        }
        None => None,
    };
    let binary_bytes = match &staged {
        // The compact rows' binary size takes the complete mesh's place in the
        // same conservative geometry allowance.
        Some((geometry, _)) => geometry.2.as_ref().unwrap().equivalent_bytes,
        None => req.mesh_files.iter().try_fold(0_u64, |sum, name| {
            sum.checked_add(crate::sloping_binary::input_bytes(&model::resolve(
                base, name,
            ))?)
            .ok_or_else(|| MapError::Budget("sloping binary geometry size overflow".into()))
        })?,
    };
    let geometry_bytes = geometry_bytes
        .checked_add(binary_bytes)
        .ok_or_else(|| MapError::Budget("sloping geometry size overflow".into()))?;
    // JSON parsing, topology, stencils, one hydraulic profile and IO copies are
    // conservatively admitted, rather than claiming an OS-enforced RSS limit.
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
            } else if row_stream_rows > 0 {
                32
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
        .and_then(|v| {
            v.checked_add(if row_stream_rows > 0 {
                (width as u64)
                    .checked_mul(height.min(row_stream_rows) as u64)?
                    .checked_mul(8)?
                    .checked_add((width as u64).checked_mul(8)?.max(2 * 256 * 1024))?
            } else {
                0
            })
        })
        // Final delivery writer: one per product while row streaming, else one at a time.
        .and_then(|v| match &final_layout {
            None => Some(v),
            Some(layout) => v.checked_add(layout.planned_bytes(req.threads)?.checked_mul(
                if row_stream_rows > 0 {
                    selected.products.len() as u64
                } else {
                    1
                },
            )?),
        })
        .and_then(|v| v.checked_add(cache))
        .and_then(|v| v.checked_add(persistent_bytes))
        .and_then(|v| v.checked_add(hydraulic_bytes))
        .and_then(|v| v.checked_add(aoi_scratch))
        .and_then(|v| {
            v.checked_add(if req.export_physical_ownership {
                OWNERS_IO_BYTES as u64
            } else {
                0
            })
        })
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
        *s.pinned_hydraulics.lock().unwrap() = None;
        s.hydraulics.lock().unwrap().clear();
        s.windows.clear();
    }
    let terrain_path = model::resolve(base, &req.terrain);
    let terrain_key = if session.is_some() {
        format!(
            "{}:{}:{}",
            identity(&terrain_path)?,
            match &crs {
                crate::crs::OutputCrs::Epsg(code) => code.to_string(),
                crate::crs::OutputCrs::GeoKeys(keys) => format!("geokeys-{}", keys.fnv1a64()),
            },
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
    if !terrain_reused && let Some(s) = session.as_mut() {
        s.terrain_preparations += 1;
    }
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
        Arc::new(pool.install(|| {
            NativeTerrain::open_crs(
                model::resolve(base, &req.terrain)
                    .to_str()
                    .ok_or_else(|| MapError::Invalid("terrain path is not UTF8".into()))?,
                &crs,
                &req.crs_identity,
                QueryConvention::PixelCenter,
                NodataPolicy::Dry,
                cache,
            )
        })?)
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
    if let Some(layout) = native_grid
        && (layout.width != terrain.width
            || layout.height != terrain.height
            || !crate::stored_grid::same(layout.source, terrain.gt)
            || terrain.block != (256, 256))
    {
        return Err(MapError::Unsupported(
            "stored grid source transform, dimensions or original tile layout mismatch".into(),
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
    // Velocity, an optional horizontal clipping depth and each derived array
    // per in-flight profile; the max accumulators once.
    let velocity_arrays = if req.velocity_normals_files.is_some() {
        1 + u64::from(req.velocity_depth == crate::velocity::VelocityDepth::Horizontal)
            + req.derived_products.len() as u64
    } else {
        0
    };
    // Shear: its output and an optional horizontal clipping depth.
    let velocity_arrays = velocity_arrays
        + req.shear_stress.as_ref().map_or(0, |s| {
            1 + u64::from(s.depth == crate::velocity::VelocityDepth::Horizontal)
        });
    let per_profile = (count as u64)
        .checked_mul(4 * velocity_arrays)
        .ok_or_else(|| MapError::Budget("velocity product plan overflow".into()))?;
    let reduce = if req.reduce_max {
        (count as u64)
            .checked_mul(4 * req.derived_products.len() as u64)
            .ok_or_else(|| MapError::Budget("max reduction plan overflow".into()))?
    } else {
        0
    };
    // The shear face pass (tables, normals, one output time's rows and
    // per-face/facepoint arrays) runs once, before any profile.
    let shear_face_pass = match &req.shear_stress {
        Some(shear) => crate::shear::planned_bytes(base, shear)?,
        None => 0,
    };
    let admitted = admitted
        .checked_add(per_profile)
        .and_then(|v| v.checked_add(reduce))
        .and_then(|v| v.checked_add(shear_face_pass))
        .ok_or_else(|| MapError::Budget("velocity product plan overflow".into()))?;
    let additional = additional
        .checked_add(per_profile)
        .ok_or_else(|| MapError::Budget("velocity product plan overflow".into()))?;
    let (effective, admitted) = admit_profiles(
        if row_stream_rows > 0 {
            1
        } else {
            req.max_inflight_profiles
        },
        req.threads,
        req.profiles.len(),
        req.max_working_bytes,
        admitted,
        additional,
    )?;
    // A staged geometry prepared for this request joins the session only now
    // that the request is admitted; a refusal above leaves the session as it was.
    if let Some((geometry, true)) = &staged
        && let Some(s) = session.as_mut()
    {
        s.geometry = Some((key.clone(), geometry.clone()));
        s.geometry_resident = geometry_bytes.saturating_mul(16);
        s.geometry_preparations += 1;
    }
    if plan_only {
        return Ok(json!({"planned_bytes":admitted}));
    }
    if deliver_files {
        fs::create_dir(out)?;
    }
    let mut report = json!({"schema":if native_grid.is_some(){"ras-commander-sloping-report/2"}else{"ras-commander-sloping-report/1"},"status":"in_progress", "method":if native_grid.is_some(){native_grid_method_record()}else{selected_method(req.options)},
        "threads":req.threads,"window":req.window,"planned_bytes":admitted,
        "requested_inflight_profiles":req.max_inflight_profiles,"effective_inflight_profiles":effective,
        "additional_profile_planned_bytes":additional,"profiles":[]});
    if crs.geokeys().is_some() {
        report["output_crs"] = crs.echo();
    }
    if let Some(layout) = native_grid {
        report["grid_contract"] = json!(req.grid_contract);
        report["source_geo_transform"] = json!(terrain.gt);
        report["output_geo_transform"] = json!(layout.output);
        report["geo_transform"] = json!(layout.window_transform(req.window));
    }
    if deliver_files {
        commit_report(out, &report, req.export_physical_ownership)?;
    }
    let execution = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
        || -> Result<(), MapError> {
            let tick = Instant::now();
            let staged_loaded = staged.as_ref().is_some_and(|(_, loaded)| *loaded);
            let geometry_reused = !staged_loaded
                && session
                    .as_ref()
                    .is_some_and(|s| s.geometry.as_ref().is_some_and(|(old, _)| old == &key));
            if !geometry_reused
                && !staged_loaded
                && let Some(s) = session.as_mut()
            {
                s.geometry = None;
                s.hydraulics.lock().unwrap().clear();
                s.windows.clear();
            }
            if !geometry_reused
                && !staged_loaded
                && let Some(s) = session.as_mut()
            {
                s.geometry_preparations += 1;
            }
            let geometry = if let Some((geometry, _)) = &staged {
                geometry.clone()
            } else if geometry_reused {
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
                Arc::new((meshes, stencils, None))
            };
            if let Some(s) = session.as_mut() {
                s.geometry = Some((key.clone(), geometry.clone()));
                s.geometry_resident = geometry_bytes.saturating_mul(16);
            }
            let (meshes, stencils, staged_cells) = &*geometry;
            // Pixel ownership labels stay global; stencils/profiles are compact.
            let owner_cells: Vec<[usize; 2]> = match staged_cells {
                Some(subset) => req
                    .owner_cells
                    .iter()
                    .map(|&[mesh, cell]| {
                        subset
                            .cells
                            .get(mesh)
                            .and_then(|cells| {
                                u32::try_from(cell)
                                    .ok()
                                    .and_then(|c| cells.binary_search(&c).ok())
                            })
                            .map(|local| [mesh, local])
                            .ok_or_else(|| {
                                MapError::Invalid(
                                    "staged cells must include every owner cell".into(),
                                )
                            })
                    })
                    .collect::<Result<_, _>>()?,
                None => req.owner_cells.clone(),
            };
            if let Some(subset) = staged_cells {
                report["staged_cells"] = subset.record.clone();
            }
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
                report["velocity"] = json!({"method": crate::velocity::method_record(),
                    "velocity_depth": match req.velocity_depth {
                        crate::velocity::VelocityDepth::Rendered => "rendered",
                        crate::velocity::VelocityDepth::Horizontal => "horizontal",
                    },
                    "derived_products": req.derived_products, "reduce_max": req.reduce_max});
            }
            let plan = DerivedPlan {
                depth: req.velocity_depth,
                names: &req.derived_products,
                gravity: req.gravity.unwrap_or(f32::NAN),
            };
            let shear_plan = match &req.shear_stress {
                Some(shear) => {
                    let shear_tick = Instant::now();
                    let factor = crate::shear::factor(&shear.units)?;
                    let mut facepoints = Vec::with_capacity(meshes.len());
                    let mut faces = Vec::with_capacity(meshes.len());
                    for (m, (mesh, input)) in meshes.iter().zip(&shear.meshes).enumerate() {
                        let normals = crate::velocity::read_normals(
                            &model::resolve(base, &input.normals_file),
                            mesh.faces.len(),
                        )?;
                        let tables = crate::shear::read_tables(
                            &model::resolve(base, &input.tables_info_file),
                            &model::resolve(base, &input.tables_values_file),
                            mesh.faces.len(),
                            input.table_rows,
                        )?;
                        let geometry = crate::shear::FaceGeometry::new(mesh, &normals, &tables)?;
                        let max = crate::shear::face_max_from_files(
                            &geometry,
                            &model::resolve(base, &input.water_surface_file),
                            &model::resolve(base, &input.face_velocity_file),
                            input.times,
                            factor,
                            &pool,
                        )?;
                        if deliver_files {
                            crate::horizontal::write_values(
                                &out.join(format!("shear_face_max-{m}.bin")),
                                &max,
                            )?;
                        }
                        faces.push(json!({"faces": max.len(), "times": input.times,
                            "nonzero_faces": max.iter().filter(|&&v| v > 0.0).count()}));
                        facepoints.push(crate::shear::facepoint_values(mesh, &max));
                    }
                    report["shear_stress"] = json!({"method": crate::shear::method_record(),
                        "units": shear.units, "factor": factor,
                        "depth": match shear.depth {
                            crate::velocity::VelocityDepth::Rendered => "rendered",
                            crate::velocity::VelocityDepth::Horizontal => "horizontal",
                        },
                        "face_files": deliver_files.then(|| (0..meshes.len()).map(|m| format!("shear_face_max-{m}.bin")).collect::<Vec<_>>()),
                        "meshes": faces,
                        "face_seconds": shear_tick.elapsed().as_secs_f64()});
                    Some(ShearPlan {
                        depth: shear.depth,
                        facepoints,
                    })
                }
                None => None,
            };
            let mut maxima = if req.reduce_max {
                vec![vec![f32::NAN; count]; req.derived_products.len()]
            } else {
                Vec::new()
            };
            report["session"] = json!({"terrain_reused":terrain_reused,"geometry_reused":geometry_reused,"retained_window_budget_bytes":persistent_bytes});
            for &[mesh, cell] in &owner_cells {
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
            let transform = native_grid
                .map(|layout| layout.window_transform(req.window))
                .unwrap_or([
                    terrain.gt[0] + col as f64 * terrain.gt[1],
                    terrain.gt[1],
                    0.,
                    terrain.gt[3] + row as f64 * terrain.gt[5],
                    0.,
                    terrain.gt[5],
                ]);
            let grid = terrio::ops::rasterize::Grid {
                geo_transform: transform,
                cols: width,
                rows: height,
                crs: Default::default(),
            };
            let tick = Instant::now();
            let window_key = if session.is_some() {
                (
                    format!("{}:{:?}", terrain_key, req.grid_contract),
                    key.clone(),
                    req.window,
                    req.owner_cells.clone(),
                    fs::read(model::resolve(base, &req.geometry_file))?,
                    req.aoi_file
                        .as_ref()
                        .map(|p| fs::read(model::resolve(base, p)))
                        .transpose()?,
                    req.terrain_scope.is_some(),
                )
            } else {
                (
                    String::new(),
                    String::new(),
                    req.window,
                    Vec::new(),
                    Vec::new(),
                    None,
                    req.terrain_scope.is_some(),
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
                let mut owner = pool.install(|| {
                    if let Some(layout) = native_grid {
                        let squares: Vec<_> = owner_cells
                            .iter()
                            .map(|&[mesh, cell]| {
                                stencils[mesh][cell].as_ref().and_then(|s| s.square)
                            })
                            .collect();
                        crate::ownership::burn_stored(cells, &layout, req.window, &squares)
                    } else {
                        crate::ownership::burn(cells, grid.clone())
                    }
                })?;
                if let Some(filename) = &req.aoi_file {
                    let mask = pool.install(|| {
                        crate::aoi::mask(
                            &model::resolve(base, filename),
                            native_grid.map_or(terrain.gt, |l| l.output),
                            req.window,
                        )
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
                // Disjoint stripes end on original source block rows, even when
                // the request starts partway through a block.
                let mut validity = req.terrain_scope.as_ref().map(|_| vec![false; count]);
                let mut remaining_ground = ground.as_mut_slice();
                let mut remaining_validity = validity.as_deref_mut();
                let mut stripes = Vec::new();
                for (offset, rows) in source_row_stripes(row, height, bh) {
                    let (destination, rest) = remaining_ground.split_at_mut(rows * width);
                    remaining_ground = rest;
                    let flags = if let Some(values) = remaining_validity.take() {
                        let (flags, rest) = values.split_at_mut(rows * width);
                        remaining_validity = Some(rest);
                        Some(flags)
                    } else {
                        None
                    };
                    stripes.push((row + offset, destination, flags));
                }
                pool.install(|| {
                    stripes.into_par_iter().try_for_each(
                        |(first, destination, mut validity)| -> Result<(), MapError> {
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
                                                let index = (r - first) * width + c - col;
                                                destination[index] = value as f32;
                                                if let Some(flags) = validity.as_mut() {
                                                    flags[index] = true;
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            Ok(())
                        },
                    )
                })?;
                report["terrain_prepare_seconds"] = tick.elapsed().as_secs_f64().into();
                Arc::new(WindowData {
                    owner,
                    ground,
                    validity,
                    weights: None,
                })
            };
            if req.export_physical_ownership {
                write_physical_ownership(&out.join("owners.bin"), &data.owner)?;
                report["physical_ownership"] = json!({
                    "kind":"physical_2d", "dtype":"<u4", "count":count,
                    "path":"owners.bin", "bytes":count as u64 * 4,
                    "width":width, "height":height, "window":req.window,
                    "geo_transform":transform, "epsg":crs.epsg(),
                    "crs_identity":req.crs_identity,
                    "owner_cell_count":req.owner_cells.len(),
                    "owned_pixels":data.owner.iter().filter(|&&label| label != 0).count(),
                });
                if crs.geokeys().is_some() {
                    report["physical_ownership"]["output_crs"] = crs.echo();
                }
            }
            let window_base_bytes = count as u64 * (8 + u64::from(req.terrain_scope.is_some()))
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
                && row_stream_rows == 0
                && weight_budget > 0
                && req.options.render_mode != sloping::RenderMode::WithFaces
            {
                let window = sloping::RasterWindow {
                    native_grid: native_grid.map(|l| (l, [col, row])),
                    source_transform: terrain.gt,
                    source_tile: [bw, bh],
                    width,
                    height,
                    transform,
                    owner: &data.owner,
                    terrain: &data.ground,
                    owner_cells: &owner_cells,
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
                native_grid: native_grid.map(|l| (l, [col, row])),
                source_transform: terrain.gt,
                source_tile: [bw, bh],
                width,
                height,
                transform,
                owner: &data.owner,
                terrain: &data.ground,
                owner_cells: &owner_cells,
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
                    let ground_tick = Instant::now();
                    crate::storage_area::read_ground(&mut pixels, &terrain, req.window)?;
                    report["storage_ground_seconds"] = ground_tick.elapsed().as_secs_f64().into();
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
                    let tick = Instant::now();
                    let scoped = crate::terrain_scope::check_with_validity(
                        scope,
                        base,
                        &terrain,
                        req.window,
                        &data.owner,
                        storage.as_ref().map_or(&[], |s| &s.pixels),
                        |p| {
                            if data.owner[p] != 0 {
                                data.validity.as_ref().unwrap()[p]
                            } else {
                                storage
                                    .as_ref()
                                    .and_then(|s| {
                                        s.pixels
                                            .binary_search(&p)
                                            .ok()
                                            .map(|i| s.ground[i].is_finite())
                                    })
                                    .unwrap_or(false)
                            }
                        },
                    )?;
                    report["terrain_scope_seconds"] = tick.elapsed().as_secs_f64().into();
                    report["terrain_scope"] = scoped.record;
                    scoped.foreign
                }
                None => Vec::new(),
            };
            for batch in req.profiles.chunks(effective) {
                if row_stream_rows > 0 {
                    for profile in batch {
                        let dir = out.join(&profile.id);
                        fs::create_dir(&dir)?;
                        // TIFF encoding uses Rayon internally, so keep delivery in the
                        // same bounded pool as rendering and hydraulic preparation.
                        let unit = pool.install(|| {
                            render_native_rows(
                                profile,
                                base,
                                meshes,
                                stencils,
                                &window,
                                &pool,
                                req.options,
                                &foreign,
                                selected,
                                final_layout.as_ref(),
                                &dir,
                                transform,
                                &crs,
                            )
                        })?;
                        sink::write_json(&dir.join("unit.json"), &unit)?;
                        report["profiles"].as_array_mut().unwrap().push(unit);
                        sink::write_json(&out.join("report.json"), &report)?;
                    }
                    continue;
                }
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
                        session
                            .as_ref()
                            .filter(|s| s.grouped_profile)
                            .map(|s| &s.pinned_hydraulics),
                        storage.as_ref(),
                        &plan,
                        shear_plan.as_ref(),
                        [terrain.width, terrain.height],
                    )
                })?;
                // Indexed collection and ordered emission preserve the pipe/report
                // contract even when later profiles finish first.
                for (profile, rendered) in batch.iter().zip(results) {
                    let RenderedProfile {
                        depth,
                        wse,
                        velocity,
                        derived,
                        shear,
                        shear_seconds,
                        hydraulic_seconds,
                        render_seconds,
                        finalize_seconds,
                        velocity_seconds,
                        derived_seconds,
                        storage_render_seconds,
                        hydraulic_reused,
                        finite_pixels,
                        flooded_pixels,
                    } = rendered;
                    if let Some(consumer) = consumer.as_mut() {
                        let unit = json!({"id":profile.id,"status":"complete",
                            "hydraulic_seconds":hydraulic_seconds,"render_seconds":render_seconds,"finalize_seconds":finalize_seconds,
                            "velocity_seconds":velocity_seconds,"derived_seconds":derived_seconds,
                            "storage_render_seconds":storage_render_seconds,
                            "hydraulic_reused":hydraulic_reused});
                        drop(wse);
                        consumer(profile, depth, unit.clone())?;
                        report["profiles"].as_array_mut().unwrap().push(unit);
                        continue;
                    }
                    let dir = out.join(&profile.id);
                    fs::create_dir(&dir)?;
                    if req.reduce_max {
                        for (acc, values) in maxima.iter_mut().zip(&derived) {
                            crate::velocity::accumulate_max_in_pool(acc, values, &pool);
                        }
                    } else if req.stream_outputs {
                        let stream_tick = Instant::now();
                        let mut stream =
                            std::io::BufWriter::with_capacity(65536, std::io::stdout().lock());
                        let mut header =
                            json!({"id":profile.id,"status":"complete","pixels":count});
                        header["storage_render_seconds"] = storage_render_seconds.into();
                        if velocity.is_some() {
                            header["velocity"] = true.into();
                        }
                        if !derived.is_empty() {
                            header["derived"] = json!(req.derived_products);
                        }
                        if shear.is_some() {
                            header["shear_stress"] = true.into();
                        }
                        if req.geotiff_outputs {
                            let tick = Instant::now();
                            header["products"] = "native-geotiff/1".into();
                            header["depth_finite_pixels"] = finite_pixels.into();
                            header["flooded_pixels"] = flooded_pixels.into();
                            for (name, values) in [("depth", depth), ("wse", wse)] {
                                if !selected.products.iter().any(|p| p == name) {
                                    continue;
                                }
                                let path = dir.join(format!("{name}.tif"));
                                match &final_layout {
                                    Some(layout) => {
                                        let summary = crate::final_tiff::write_crs(
                                            &path, &values, layout, &crs, &pool,
                                        )?;
                                        header[format!("{name}_final")] = json!({
                                            "valid_pixels":summary.valid_pixels,"tiles":summary.tiles,
                                            "shared_nodata_tiles":summary.shared_nodata_tiles,
                                            "file_bytes":summary.file_bytes});
                                    }
                                    // On a request worker (its stack, not the main
                                    // thread's); terrio's calls run on helper threads
                                    // while the worker waits (`terrio_pool::call`).
                                    None => pool.install(|| {
                                        crate::geotiff::write(
                                            &path,
                                            values,
                                            width,
                                            height,
                                            transform,
                                            &crs,
                                            selected.compression,
                                        )
                                    })?,
                                }
                            }
                            if output.is_some() {
                                header["selected_products"] = json!(selected.products);
                                header["compression"] = json!(selected.compression);
                            }
                            if let Some(format) = &selected.final_format {
                                header["final_format"] = json!(format);
                            }
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
                            for values in &derived {
                                crate::horizontal::write_f32(&mut stream, values)?;
                            }
                            if let Some(values) = &shear {
                                crate::horizontal::write_f32(&mut stream, values)?;
                            }
                            stream.flush()?;
                        }
                        report["stream_seconds"] =
                            (report["stream_seconds"].as_f64().unwrap_or(0.0)
                                + stream_tick.elapsed().as_secs_f64())
                            .into();
                    } else {
                        crate::horizontal::write_values(&dir.join("depth.bin"), &depth)?;
                        crate::horizontal::write_values(&dir.join("wse.bin"), &wse)?;
                        if let Some(velocity) = &velocity {
                            crate::horizontal::write_values(&dir.join("velocity.bin"), velocity)?;
                        }
                        for (name, values) in req.derived_products.iter().zip(&derived) {
                            crate::horizontal::write_values(
                                &dir.join(format!("{name}.bin")),
                                values,
                            )?;
                        }
                        if let Some(values) = &shear {
                            crate::horizontal::write_values(&dir.join("shear_stress.bin"), values)?;
                        }
                    }
                    let mut unit = json!({"id":profile.id,"status":"complete","hydraulic_seconds":hydraulic_seconds,"render_seconds":render_seconds,"finalize_seconds":finalize_seconds,"velocity_seconds":velocity_seconds,"derived_seconds":derived_seconds,"storage_render_seconds":storage_render_seconds,"hydraulic_reused":hydraulic_reused});
                    if let Some(values) = &shear {
                        unit["shear_seconds"] = shear_seconds.into();
                        unit["shear_valid_pixels"] =
                            values.iter().filter(|v| !v.is_nan()).count().into();
                    }
                    sink::write_json(&dir.join("unit.json"), &unit)?;
                    report["profiles"].as_array_mut().unwrap().push(unit);
                    if deliver_files {
                        sink::write_json(&out.join("report.json"), &report)?;
                    }
                }
            }
            if req.reduce_max {
                let dir = out.join("max");
                fs::create_dir(&dir)?;
                for (name, values) in req.derived_products.iter().zip(&maxima) {
                    crate::horizontal::write_values(&dir.join(format!("{name}.bin")), values)?;
                }
                let valid: Vec<usize> = maxima
                    .iter()
                    .map(|m| m.iter().filter(|v| !v.is_nan()).count())
                    .collect();
                let unit = json!({"id":"max","status":"complete","profiles":req.profiles.len(),
                    "products":req.derived_products,"valid_pixels":valid});
                sink::write_json(&dir.join("unit.json"), &unit)?;
                report["max"] = unit;
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
            if deliver_files && let Some(shear) = &req.shear_stress {
                for m in 0..shear.meshes.len() {
                    let _ = fs::remove_file(out.join(format!("shear_face_max-{m}.bin")));
                    let _ = fs::remove_file(out.join(format!("shear_face_max-{m}.bin.partial")));
                }
                if let Some(record) = report.get_mut("shear_stress") {
                    record["face_files"] = Value::Null;
                }
            }
            if req.export_physical_ownership {
                let _ = fs::remove_file(out.join("owners.bin"));
                let _ = fs::remove_file(out.join("owners.bin.partial"));
                report.as_object_mut().unwrap().remove("physical_ownership");
            }
            report["error"] = match other {
                Ok(Err(e)) => e.to_string(),
                _ => "sloping internal panic".into(),
            }
            .into();
            for profile in req.profiles.iter().filter(|_| deliver_files) {
                let dir = out.join(&profile.id);
                for filename in [
                    "depth.bin",
                    "wse.bin",
                    "velocity.bin",
                    "shear_stress.bin",
                    "depth.bin.partial",
                    "wse.bin.partial",
                    "depth.tif",
                    "wse.tif",
                ]
                .into_iter()
                .map(String::from)
                .chain(req.derived_products.iter().map(|n| format!("{n}.bin")))
                {
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
    if deliver_files {
        commit_report(out, &report, req.export_physical_ownership)?;
    }
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
    fn final_report_failure_removes_exported_ownership() {
        let out = std::env::temp_dir().join(format!(
            "fast-map-owner-report-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&out).unwrap();
        // Force the atomic JSON writer to fail independently of permissions.
        fs::create_dir(out.join("report.json.partial")).unwrap();
        fs::write(out.join("owners.bin"), 1_u32.to_le_bytes()).unwrap();
        fs::write(out.join("owners.bin.partial"), [0]).unwrap();
        assert!(commit_report(&out, &json!({"status":"complete"}), true).is_err());
        assert!(!out.join("owners.bin").exists());
        assert!(!out.join("owners.bin.partial").exists());
        fs::remove_dir_all(&out).unwrap();
    }

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

/// Source-lattice aligned output stripes, expressed as (window row, row count).
fn source_row_stripes(row: usize, height: usize, block_rows: usize) -> Vec<(usize, usize)> {
    let mut stripes = Vec::new();
    let mut offset = 0;
    while offset < height {
        let rows = (block_rows - (row + offset) % block_rows).min(height - offset);
        stripes.push((offset, rows));
        offset += rows;
    }
    stripes
}

#[cfg(test)]
mod stripe_tests {
    use super::source_row_stripes;
    #[test]
    fn stripes_are_source_aligned_and_cover_partial_edges_once() {
        for row in [0, 1, 255, 256, 7787] {
            for height in [1, 17, 256, 513, 18228] {
                let stripes = source_row_stripes(row, height, 256);
                let mut next = 0;
                for (offset, rows) in stripes {
                    assert_eq!(offset, next);
                    assert!(rows > 0 && rows <= 256);
                    assert_eq!((row + offset) / 256, (row + offset + rows - 1) / 256);
                    next += rows;
                }
                assert_eq!(next, height);
            }
        }
    }

    #[test]
    fn row_stream_refusals_happen_before_input_reads() {
        use super::*;
        let baseline: Request = serde_json::from_value(json!({
            "schema":SCHEMA,"terrain":"not-present.tif","epsg":3433,
            "crs_identity":{"kind":"epsg"},"terrain_transform":[0,1,0,256,0,-1],
            "window":[0,0,256,256],"geometry_file":"not-present.wkb",
            "mesh_files":["not-present.mesh"],"owner_cells":[[0,0]],
            "profiles":[{"id":"max","values_files":["not-present.f32"]}],
            "threads":1,"max_working_bytes":268435456,"stream_outputs":true,
            "geotiff_outputs":true
        }))
        .unwrap();
        let output = crate::native_output::OutputOptions {
            row_stream_rows: 7,
            ..Default::default()
        };
        for case in 0..4 {
            let mut request = baseline.clone();
            match case {
                0 => request.options.render_mode = sloping::RenderMode::WithFaces,
                1 => request.storage_areas_file = Some("not-present-storage.wkb".into()),
                2 => request.velocity_normals_files = Some(vec!["not-present.f32".into()]),
                _ => request.stream_outputs = false,
            }
            let error = run_with_output_options(
                &request,
                Path::new("."),
                Path::new("not-created"),
                None,
                Some(&output),
            )
            .unwrap_err();
            assert_eq!(error.kind(), "unsupported");
            assert!(error.to_string().contains("row streaming"));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_native_rows(
    profile: &Profile,
    base: &Path,
    meshes: &[Mesh],
    stencils: &[Vec<Option<sloping::CellStencil>>],
    window: &sloping::RasterWindow<'_>,
    pool: &rayon::ThreadPool,
    options: sloping::Options,
    foreign: &[usize],
    output: &crate::native_output::OutputOptions,
    final_layout: Option<&crate::final_tiff::Layout>,
    dir: &Path,
    transform: [f64; 6],
    crs: &crate::crs::OutputCrs,
) -> Result<Value, MapError> {
    let tick = Instant::now();
    let mut prepared = Vec::new();
    for ((mesh, stencil), filename) in meshes.iter().zip(stencils).zip(&profile.values_files) {
        let values = values(&model::resolve(base, filename), mesh.cells.len())?;
        // Only a staged mesh without owner cells has no faces; nothing reads it.
        prepared.push(if mesh.faces.is_empty() {
            sloping::PreparedProfile::unread(options)
        } else {
            sloping::prepare_profile_with_options(mesh, stencil, &values, 0.001, pool, options)?
        });
    }
    crate::terrain_scope::deny_wet_foreign(foreign, window.owner, window.owner_cells, |m, c| {
        prepared[m].is_wet(c)
    })?;
    let hydraulic_seconds = tick.elapsed().as_secs_f64();
    let create = |name: &str| {
        output
            .products
            .iter()
            .any(|p| p == name)
            .then(|| {
                crate::geotiff::ProductWriter::create(
                    &dir.join(format!("{name}.tif")),
                    window.width,
                    window.height,
                    transform,
                    crs,
                    output.compression,
                    final_layout,
                )
            })
            .transpose()
    };
    let mut depth_writer = create("depth")?;
    let mut wse_writer = create("wse")?;
    let mut render_seconds = 0.0;
    let mut finalize_seconds = 0.0;
    let mut write_seconds = 0.0;
    let mut finite_pixels = 0;
    let mut flooded_pixels = 0;
    for row in (0..window.height).step_by(output.row_stream_rows) {
        let rows = output.row_stream_rows.min(window.height - row);
        let tick = Instant::now();
        let (mut depth, mut wse) =
            sloping::render_profile_rows(stencils, &prepared, window, 0.001, pool, row, rows)?;
        render_seconds += tick.elapsed().as_secs_f64();
        let tick = Instant::now();
        let (finite, flooded) = crate::finalize::apply(&mut depth, &mut wse, pool);
        finite_pixels += finite;
        flooded_pixels += flooded;
        finalize_seconds += tick.elapsed().as_secs_f64();
        let tick = Instant::now();
        if let Some(writer) = depth_writer.as_mut() {
            writer.write_rows(&depth, pool)?;
        }
        if let Some(writer) = wse_writer.as_mut() {
            writer.write_rows(&wse, pool)?;
        }
        write_seconds += tick.elapsed().as_secs_f64();
    }
    let tick = Instant::now();
    if let Some(writer) = depth_writer {
        writer.finish()?;
    }
    if let Some(writer) = wse_writer {
        writer.finish()?;
    }
    write_seconds += tick.elapsed().as_secs_f64();
    let mut header = json!({"id":profile.id,"status":"complete","pixels":window.owner.len(),
        "products":"native-geotiff/1","depth_finite_pixels":finite_pixels,"flooded_pixels":flooded_pixels,
        "selected_products":output.products,"compression":output.compression,
        "row_stream_rows":output.row_stream_rows,"write_seconds":write_seconds,"storage_render_seconds":0.0});
    if let Some(format) = &output.final_format {
        header["final_format"] = json!(format);
    }
    let tick = Instant::now();
    let mut stream = std::io::stdout().lock();
    writeln!(stream, "{header}")?;
    stream.flush()?;
    Ok(
        json!({"id":profile.id,"status":"complete","hydraulic_seconds":hydraulic_seconds,
        "render_seconds":render_seconds,"finalize_seconds":finalize_seconds,"write_seconds":write_seconds,
        "stream_seconds":tick.elapsed().as_secs_f64(),"row_stream_rows":output.row_stream_rows,
        "storage_render_seconds":0.0,"hydraulic_reused":false}),
    )
}
