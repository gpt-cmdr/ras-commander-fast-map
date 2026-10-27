//! Explicit sloping batch path with independently selected native rendering controls. Geometry, terrain and ownership are shared
//! across profiles; hydraulic connectivity is reconstructed for every profile.
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
use std::{collections::HashSet, fs, io::Write, path::Path, sync::Arc, time::Instant};

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

/// One explicitly owned, serialized native session. At most one geometry,
/// terrain and bounded output window are retained. Dropping it releases all.
#[derive(Default)]
pub struct PreparedSession {
    terrain: Option<(String, Arc<NativeTerrain>)>,
    geometry: Option<(String, Arc<CachedGeometry>)>,
    window: Option<(WindowKey, Arc<WindowData>)>,
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
}

fn one_profile() -> usize {
    1
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
    hydraulic_seconds: f64,
    render_seconds: f64,
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
) -> Result<RenderedProfile, MapError> {
    let tick = Instant::now();
    let mut prepared = Vec::new();
    for ((mesh, stencil), filename) in meshes.iter().zip(stencils).zip(&profile.values_files) {
        prepared.push(sloping::prepare_profile_with_options(
            mesh,
            stencil,
            &values(&model::resolve(base, filename), mesh.cells.len())?,
            0.001,
            pool,
            options,
        )?);
    }
    let hydraulic_seconds = tick.elapsed().as_secs_f64();
    let tick = Instant::now();
    let (mut depth, mut wse) =
        sloping::render_profile_cached(stencils, &prepared, window, 0.001, pool, weights)?;
    let render_seconds = tick.elapsed().as_secs_f64();
    pool.install(|| {
        depth
            .par_iter_mut()
            .zip(wse.par_iter_mut())
            .for_each(|(d, w)| {
                if *d == sloping::NODATA {
                    *d = f32::NAN;
                    *w = f32::NAN;
                }
            })
    });
    Ok(RenderedProfile {
        depth,
        wse,
        hydraulic_seconds,
        render_seconds,
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub id: String,
    pub values_files: Vec<String>,
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
        "native_version_qualified":"7.0.1", "stream_outputs":true, "native_ownership":true,
        "enhanced_options":true,"enhanced_method_id":"rasmapper-sloping-with-faces/1",
        "depth_weighted_faces":true,
        "native_donation_history":true,
        "bounded_profile_concurrency":true,
        "persistent_session":true,"bounded_pixel_weights":true,
        "options":{"CellRenderMode":"Sloping","CellStencilMode":"JustFacepoints",
            "FacepointAdjustmentMode":"None","ShallowBehaviorMode":"None",
            "UseDepthWeightedFaces":false,"FaceWSMode":"Adjusted"},
        "native_plot_tolerance":0.001_f32,"extra_depth_cutoff":null,
        "scope":"2D cells; plain cell-corner sloping; no 1D, slopingPretty or additional rendering options",
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
        record["scope"] = "physical 2D cells; cell-corner and face sloping; no 1D or standalone storage-area surfaces".into();
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
    let persistent_bytes = if session.is_some() { 64_u64 << 20 } else { 0 };
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
        s.window = None;
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
        s.window = None;
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
                s.window = None;
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
            let window_reused = session
                .as_ref()
                .is_some_and(|s| s.window.as_ref().is_some_and(|(old, _)| old == &window_key));
            if !window_reused && let Some(s) = session.as_mut() {
                s.window = None;
            }
            let mut data = if window_reused {
                session.as_ref().unwrap().window.as_ref().unwrap().1.clone()
            } else {
                let cells = crate::ownership::load(
                    &model::resolve(base, &req.geometry_file),
                    req.owner_cells.len(),
                    false,
                )?;
                let mut owner = pool.install(|| crate::ownership::burn(cells, grid.clone()))?;
                if let Some(filename) = &req.aoi_file {
                    let aoi = crate::ownership::load(&model::resolve(base, filename), 1, true)?;
                    let mask = pool.install(|| crate::ownership::burn(aoi, grid))?;
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
            let (bw, bh) = terrain.block;
            if session.is_some()
                && !window_reused
                && count as u64 * 8 < persistent_bytes / 2
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
                    (persistent_bytes / 2) as usize,
                    &pool,
                )?;
                Arc::get_mut(&mut data).unwrap().weights = Some(weights);
            }
            let weight_bytes = data.weights.as_ref().map_or(0, |w| w.bytes());
            report["session"]["window_reused"] = window_reused.into();
            report["session"]["pixel_weight_cache_bytes"] = weight_bytes.into();
            let window_bytes = count as u64 * 8
                + weight_bytes as u64
                + window_key.0.len() as u64
                + window_key.1.len() as u64
                + (window_key.3.len() * std::mem::size_of::<[usize; 2]>()) as u64
                + window_key.4.len() as u64
                + window_key.5.as_ref().map_or(0, |v| v.len()) as u64;
            if window_bytes <= persistent_bytes
                && let Some(s) = session.as_mut()
            {
                s.window = Some((window_key, data.clone()));
            }
            report["session"]["retained_window_bytes"] =
                if session.is_some() && window_bytes <= persistent_bytes {
                    window_bytes
                } else {
                    0
                }
                .into();
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
                    )
                })?;
                // Indexed collection and ordered emission preserve the pipe/report
                // contract even when later profiles finish first.
                for (profile, rendered) in batch.iter().zip(results) {
                    let RenderedProfile {
                        depth,
                        wse,
                        hydraulic_seconds,
                        render_seconds,
                    } = rendered;
                    let dir = out.join(&profile.id);
                    fs::create_dir(&dir)?;
                    if req.stream_outputs {
                        let mut stream =
                            std::io::BufWriter::with_capacity(65536, std::io::stdout().lock());
                        writeln!(
                            stream,
                            "{}",
                            json!({"id":profile.id,"status":"complete","pixels":count})
                        )?;
                        crate::horizontal::write_f32(&mut stream, &depth)?;
                        crate::horizontal::write_f32(&mut stream, &wse)?;
                        stream.flush()?;
                    } else {
                        crate::horizontal::write_values(&dir.join("depth.bin"), &depth)?;
                        crate::horizontal::write_values(&dir.join("wse.bin"), &wse)?;
                    }
                    let unit = json!({"id":profile.id,"status":"complete","hydraulic_seconds":hydraulic_seconds,"render_seconds":render_seconds});
                    sink::write_json(&dir.join("unit.json"), &unit)?;
                    report["profiles"].as_array_mut().unwrap().push(unit);
                    sink::write_json(&out.join("report.json"), &report)?;
                }
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
