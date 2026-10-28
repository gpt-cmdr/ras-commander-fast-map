//! Deterministic signed-depth boundary pipeline. Raster delivery is untouched.
use crate::{
    MapError, boundary_contour as contour,
    model::{self, NodataPolicy, QueryConvention},
    sloping, sloping_session,
    terrain::NativeTerrain,
};
use rayon::prelude::*;
use rstar::{AABB, RTree, RTreeObject};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Write,
    path::Path,
    time::Instant,
};
use wbvector::{Geometry, Layer};
const TILE: usize = 256;
const SIDE: usize = 257;
const SAMPLES: usize = SIDE * SIDE;
pub const SCHEMA: &str = "ras-commander-boundary-request/1";
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub schema: String,
    pub interpolation: String,
    pub model: sloping_session::Request,
    pub terrain_metadata: TerrainMetadata,
}
#[derive(Deserialize)]
pub struct Array {
    pub path: String,
    pub dtype: String,
    pub shape: Vec<usize>,
}
#[derive(Deserialize)]
pub struct TerrainMetadata {
    pub shape: [usize; 2],
    pub transform: [f64; 6],
    pub tile_shape: [usize; 2],
    pub tile_grid: [usize; 2],
    pub selected_tiles: Vec<usize>,
    pub mask: Array,
    pub perimeter: Array,
    pub stitch_points: Option<Array>,
    pub stitch_triangles: Option<Array>,
}
fn invalid(s: &str) -> MapError {
    MapError::Invalid(s.into())
}
fn bytes(
    a: &Array,
    dtype: &str,
    shape: &[usize],
    width: usize,
    base: &Path,
) -> Result<u64, MapError> {
    if a.dtype != dtype || a.shape != shape {
        return Err(invalid("boundary numeric descriptor mismatch"));
    }
    let n = shape
        .iter()
        .try_fold(width, |n, d| n.checked_mul(*d))
        .ok_or_else(|| invalid("boundary array size overflow"))?;
    if fs::metadata(model::resolve(base, &a.path))?.len() != n as u64 {
        return Err(invalid("boundary numeric file length mismatch"));
    }
    Ok(n as u64)
}
fn read(a: &Array, base: &Path, expected: u64) -> Result<Vec<u8>, MapError> {
    let data = fs::read(model::resolve(base, &a.path))?;
    if data.len() as u64 != expected {
        return Err(invalid("boundary numeric file changed size"));
    }
    Ok(data)
}
#[derive(Clone)]
struct Indexed {
    id: usize,
    bbox: AABB<[f64; 2]>,
}
impl RTreeObject for Indexed {
    type Envelope = AABB<[f64; 2]>;
    fn envelope(&self) -> Self::Envelope {
        self.bbox
    }
}
struct Owners {
    layer: Layer,
    index: RTree<Indexed>,
}
impl Owners {
    fn load(path: &Path, count: usize) -> Result<Self, MapError> {
        let layer = crate::ownership::load(path, count, false)?;
        let indexed = layer
            .features
            .iter()
            .enumerate()
            .map(|(id, f)| {
                let b = f
                    .geometry
                    .as_ref()
                    .and_then(Geometry::bbox)
                    .ok_or_else(|| invalid("empty boundary physical cell"))?;
                Ok(Indexed {
                    id,
                    bbox: AABB::from_corners([b.min_x, b.min_y], [b.max_x, b.max_y]),
                })
            })
            .collect::<Result<Vec<_>, MapError>>()?;
        Ok(Self {
            layer,
            index: RTree::bulk_load(indexed),
        })
    }
    fn candidates(&self, bounds: AABB<[f64; 2]>) -> Vec<usize> {
        let mut ids: Vec<_> = self
            .index
            .locate_in_envelope_intersecting(bounds)
            .map(|x| x.id)
            .collect();
        ids.sort_unstable();
        ids
    }
    fn grid(&self, transform: [f64; 6]) -> Result<Vec<u32>, MapError> {
        let a = [transform[0], transform[3] + SIDE as f64 * transform[5]];
        let b = [transform[0] + SIDE as f64 * transform[1], transform[3]];
        let mut layer = Layer::new("boundary ownership");
        layer.schema = self.layer.schema.clone();
        layer.features = self
            .candidates(AABB::from_corners(a, b))
            .into_iter()
            .map(|i| self.layer.features[i].clone())
            .collect();
        if layer.features.is_empty() {
            return Ok(vec![0; SAMPLES]);
        }
        crate::ownership::burn(
            layer,
            terrio::ops::rasterize::Grid {
                geo_transform: transform,
                cols: SIDE,
                rows: SIDE,
                crs: Default::default(),
            },
        )
    }
    fn point(&self, p: [f64; 2]) -> u32 {
        self.candidates(AABB::from_point(p))
            .into_iter()
            .rev()
            .find(
                |&i| match self.layer.features[i].geometry.as_ref().unwrap() {
                    Geometry::Polygon {
                        exterior,
                        interiors,
                    } => inside(&exterior.0, p) && !interiors.iter().any(|r| inside(&r.0, p)),
                    _ => false,
                },
            )
            .map_or(0, |i| i as u32 + 1)
    }
}
fn inside(r: &[wbvector::Coord], p: [f64; 2]) -> bool {
    let mut hit = false;
    for (a, b) in r.iter().zip(r.iter().cycle().skip(1)).take(r.len()) {
        if (a.y > p[1]) != (b.y > p[1]) && p[0] < (b.x - a.x) * (p[1] - a.y) / (b.y - a.y) + a.x {
            hit = !hit;
        }
    }
    hit
}
struct TileData {
    id: usize,
    transform: [f64; 6],
    owner: Vec<u32>,
    ground: Vec<Option<f32>>,
}
// Render-block coverage activates its four sample locations. Ground validity is
// checked independently. This does not declare the original block a quad clip mask.
fn activated_samples(mask: &[u8], rows: usize, cols: usize) -> Vec<bool> {
    let stride = cols + 1;
    let mut active = vec![false; (rows + 1) * stride];
    for row in 0..rows {
        for col in 0..cols {
            if mask[row * cols + col] & 1 != 0 {
                let index = row * stride + col;
                for offset in [0, 1, stride, stride + 1] {
                    active[index + offset] = true;
                }
            }
        }
    }
    active
}
fn prepare_tile(
    id: usize,
    position: usize,
    metadata: &TerrainMetadata,
    mask: &[u8],
    perimeter: &[f32],
    terrain: &NativeTerrain,
    owners: &Owners,
) -> Result<TileData, MapError> {
    let row = (id / metadata.tile_grid[1]) * TILE;
    let col = (id % metadata.tile_grid[1]) * TILE;
    let mut transform = metadata.transform;
    transform[0] += col as f64 * transform[1];
    transform[3] += row as f64 * transform[5];
    let owner = owners.grid(transform)?;
    let start = position * TILE * TILE;
    let valid = activated_samples(&mask[start..start + TILE * TILE], TILE, TILE);
    let mut ground = vec![None; SAMPLES];
    let mut blocks = HashMap::new();
    for r in 0..SIDE {
        for c in 0..SIDE {
            let k = r * SIDE + c;
            if !valid[k] || owner[k] == 0 {
                continue;
            }
            let v = if r == TILE {
                perimeter[position * 513 + TILE + c]
            } else if c == TILE {
                perimeter[position * 513 + r]
            } else if row + r < terrain.height && col + c < terrain.width {
                let key = ((col + c) / terrain.block.0, (row + r) / terrain.block.1);
                if let std::collections::hash_map::Entry::Vacant(entry) = blocks.entry(key) {
                    entry.insert(terrain.get_block(key.0, key.1)?);
                }
                let block = &blocks[&key];
                let w =
                    terrain.block_window((col + c) / terrain.block.0, (row + r) / terrain.block.1);
                block[((row + r) % terrain.block.1) * w.width + (col + c) % terrain.block.0]
            } else {
                continue;
            };
            if let Some(v) = terrain.classify(v) {
                if !v.is_finite() {
                    return Err(invalid("infinite boundary terrain sample"));
                }
                ground[k] = Some(v as f32);
            }
        }
    }
    Ok(TileData {
        id,
        transform,
        owner,
        ground,
    })
}
fn aoi(base: &Path, filename: &Option<String>) -> Result<Option<contour::MultiPolygon>, MapError> {
    let Some(name) = filename else {
        return Ok(None);
    };
    let l = crate::ownership::load(&model::resolve(base, name), 1, true)?;
    let ring = |r: &wbvector::Ring| r.0.iter().map(|c| [c.x, c.y]).collect::<Vec<_>>();
    let polygon = |e: &wbvector::Ring, h: &[wbvector::Ring]| {
        std::iter::once(ring(e))
            .chain(h.iter().map(ring))
            .collect::<Vec<_>>()
    };
    Ok(Some(match l.features[0].geometry.as_ref().unwrap() {
        Geometry::Polygon {
            exterior,
            interiors,
        } => vec![polygon(exterior, interiors)],
        Geometry::MultiPolygon(p) => p.iter().map(|(e, h)| polygon(e, h)).collect(),
        _ => return Err(invalid("boundary AOI must be polygon")),
    }))
}
fn values(path: &Path, n: usize) -> Result<Vec<f32>, MapError> {
    if fs::metadata(path)?.len() != n as u64 * 4 {
        return Err(invalid("boundary profile length mismatch"));
    }
    let raw = fs::read(path)?;
    if raw.len() != n * 4 {
        return Err(invalid("boundary profile size changed"));
    }
    let result: Vec<_> = raw
        .as_chunks::<4>()
        .0
        .iter()
        .map(|x| f32::from_le_bytes(*x))
        .collect();
    if result.iter().any(|x| !x.is_finite()) {
        return Err(invalid("boundary profile requires finite cell WSE"));
    }
    Ok(result)
}
fn atomic_json(path: &Path, value: &Value) -> Result<(), MapError> {
    let partial = path.with_extension("partial");
    let mut file = fs::File::create(&partial)?;
    serde_json::to_writer(&mut file, value).map_err(|e| invalid(&e.to_string()))?;
    file.flush()?;
    drop(file);
    fs::rename(partial, path)?;
    Ok(())
}
fn horizontal_surface(water: f32, minimum: f32) -> Option<f32> {
    (water != sloping::NODATA && water > minimum + 0.001_f32).then_some(water)
}
pub fn method_record() -> Value {
    json!({"method_id":contour::METHOD_ID,"coordinate_precision":0.001,"native_vector_parity":false,"scope":"physical 2D cells; horizontal or plain corner sloping; deterministic zero-depth contours","differences":"stable ordered all-subject integer union; RASMapper polygons can differ in holes and quantized coordinates","depth_cutoff":0,"cell_wet_tolerance":0.001,"cell_wet_rule":"float32 cell WSE > float32(minimum elevation + 0.001); no per-pixel depth filtering"})
}
/// One bounded batch. The complete physical geometry and hydraulics remain global.
pub fn run(req: &Request, base: &Path, out: &Path) -> Result<Value, MapError> {
    if out.exists() && fs::read_dir(out)?.next().is_some() {
        return Err(invalid("boundary output directory must be empty"));
    }
    fs::create_dir_all(out)?;
    let mut report = json!({"schema":"ras-commander-boundary-report/1","status":"in_progress","method":method_record(),"units":[],"profiles":[]});
    atomic_json(&out.join("report.json"), &report)?;
    match execute(req, base, out, &mut report) {
        Ok(()) => {
            report["status"] = "complete".into();
            atomic_json(&out.join("report.json"), &report)?;
            Ok(report)
        }
        Err(error) => {
            for item in fs::read_dir(out)? {
                let path = item?.path();
                if path
                    .extension()
                    .is_some_and(|e| e == "geojson" || e == "partial")
                {
                    fs::remove_file(path)?;
                }
            }
            report["status"] = "aborted".into();
            report["profiles"] = req
                .model
                .profiles
                .iter()
                .map(|p| json!({"id":p.id,"status":"aborted","error":error.to_string()}))
                .collect::<Vec<_>>()
                .into();
            for unit in report["units"].as_array_mut().unwrap() {
                unit["status"] = "aborted".into();
                unit.as_object_mut().unwrap().remove("path");
            }
            report["error"] = error.to_string().into();
            atomic_json(&out.join("report.json"), &report)?;
            Err(error)
        }
    }
}
fn execute(req: &Request, base: &Path, out: &Path, report: &mut Value) -> Result<(), MapError> {
    let tick = Instant::now();
    let m = &req.model;
    let t = &req.terrain_metadata;
    if req.schema != SCHEMA
        || m.schema != sloping_session::SCHEMA
        || !["horizontal", "sloping"].contains(&req.interpolation.as_str())
        || m.options != sloping::Options::default()
    {
        return Err(invalid("unsupported boundary schema/interpolation/options"));
    }
    if m.terrain_scope.is_some() {
        return Err(MapError::Unsupported(
            "top-priority-source terrains are not qualified for boundaries".into(),
        ));
    }
    if m.epsg == 0
        || m.threads == 0
        || m.threads > 64
        || m.max_inflight_profiles == 0
        || m.profiles.is_empty()
        || m.mesh_files.is_empty()
        || m.owner_cells.is_empty()
        || m.owner_cells.len() >= u32::MAX as usize
    {
        return Err(invalid("invalid boundary threads/profiles/geometry"));
    }
    let mut ids = HashSet::new();
    for profile in &m.profiles {
        model::validate_id(&profile.id)
            .map_err(|error| invalid(&format!("invalid boundary profile id: {error}")))?;
        if !ids.insert(model::id_key(&profile.id)) {
            return Err(invalid("duplicate boundary profile ids (case-insensitive)"));
        }
        if profile.values_files.len() != m.mesh_files.len() {
            return Err(invalid("boundary profile files must match mesh count"));
        }
    }
    if t.tile_shape != [TILE, TILE]
        || t.shape.contains(&0)
        || t.tile_grid != [t.shape[0].div_ceil(TILE), t.shape[1].div_ceil(TILE)]
        || t.transform != m.terrain_transform
        || t.transform.iter().any(|x| !x.is_finite())
        || t.transform[1] <= 0.
        || t.transform[5] != -t.transform[1]
        || t.transform[2] != 0.
        || t.transform[4] != 0.
    {
        return Err(invalid(
            "boundary requires original square north-up 256 terrain tiles",
        ));
    }
    let mut unique = HashSet::new();
    let total_tiles = t.tile_grid[0]
        .checked_mul(t.tile_grid[1])
        .ok_or_else(|| invalid("boundary tile count overflow"))?;
    if t.selected_tiles.is_empty()
        || t.selected_tiles
            .iter()
            .any(|i| *i >= total_tiles || !unique.insert(*i))
    {
        return Err(invalid("boundary selected tiles invalid"));
    }
    let n = t.selected_tiles.len();
    let mask_bytes = bytes(&t.mask, "|u1", &[n, TILE * TILE], 1, base)?;
    let perimeter_bytes = bytes(&t.perimeter, "<f4", &[n, 513], 4, base)?;
    let geometry_bytes = fs::metadata(model::resolve(base, &m.geometry_file))?.len();
    let mut input = mask_bytes
        .checked_add(perimeter_bytes)
        .and_then(|v| v.checked_add(geometry_bytes))
        .ok_or_else(|| invalid("boundary input size overflow"))?;
    if let Some(name) = &m.aoi_file {
        input = input
            .checked_add(fs::metadata(model::resolve(base, name))?.len())
            .ok_or_else(|| invalid("boundary AOI budget overflow"))?;
    }
    for name in &m.mesh_files {
        input = input
            .checked_add(crate::sloping_binary::input_bytes(&model::resolve(
                base, name,
            ))?)
            .ok_or_else(|| invalid("boundary input budget overflow"))?;
    }
    let (point_bytes, tri_bytes) = match (&t.stitch_points, &t.stitch_triangles) {
        (Some(p), Some(q))
            if p.shape.len() == 2 && p.shape[1] == 4 && q.shape.len() == 2 && q.shape[1] == 3 =>
        {
            (
                bytes(p, "<f8", &p.shape, 8, base)?,
                bytes(q, "<i4", &q.shape, 4, base)?,
            )
        }
        (None, None) => (0, 0),
        _ => {
            return Err(invalid(
                "boundary stitch descriptors must be paired Nx4/Mx3",
            ));
        }
    };
    input = input
        .checked_add(point_bytes)
        .and_then(|x| x.checked_add(tri_bytes))
        .ok_or_else(|| invalid("boundary input budget overflow"))?;
    let terrain = NativeTerrain::open(
        model::resolve(base, &m.terrain)
            .to_str()
            .ok_or_else(|| invalid("non-UTF8 terrain path"))?,
        m.epsg,
        &m.crs_identity,
        QueryConvention::PixelCenter,
        NodataPolicy::Dry,
        64 << 20,
    )?;
    if [terrain.height, terrain.width] != t.shape || terrain.gt != t.transform {
        return Err(invalid(
            "boundary original terrain metadata disagrees with TIFF",
        ));
    }
    // Conservative geometry decode/ownership inflation plus shared tile payloads;
    // remaining memory is explicitly divided among concurrent contour builders.
    let shared = input
        .checked_mul(16)
        .and_then(|x| {
            (n as u64)
                .checked_mul((SAMPLES * 16) as u64)
                .and_then(|y| x.checked_add(y))
        })
        .and_then(|x| x.checked_add(64 << 20))
        .and_then(|x| {
            terrain
                .decoder_transient_bytes_per_load()
                .checked_mul(m.threads as u64)
                .and_then(|y| x.checked_add(y))
        })
        .ok_or_else(|| invalid("boundary memory plan overflow"))?;
    if shared >= m.max_working_bytes {
        return Err(MapError::Budget(
            "boundary shared preparation exceeds budget".into(),
        ));
    }
    let remaining = m.max_working_bytes - shared;
    let hydraulic_allowance = input
        .checked_mul(8)
        .ok_or_else(|| invalid("boundary hydraulic budget overflow"))?;
    let slot = hydraulic_allowance
        .checked_add(16 << 20)
        .ok_or_else(|| invalid("boundary slot budget overflow"))?;
    let inflight = m
        .max_inflight_profiles
        .min(m.threads)
        .min(m.profiles.len())
        .min((remaining / slot) as usize);
    if inflight == 0 {
        return Err(MapError::Budget(
            "boundary contour allowance below 16 MiB".into(),
        ));
    }
    let allowance = ((remaining / inflight as u64) - hydraulic_allowance) as usize;
    let limits = contour::Limits {
        max_working_bytes: allowance,
    };
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(m.threads)
        .build()
        .map_err(|e| invalid(&e.to_string()))?;
    let mut meshes = vec![];
    let mut stencils = vec![];
    for name in &m.mesh_files {
        let (mesh, s) = crate::sloping_binary::load(&model::resolve(base, name))?.prepare()?;
        meshes.push(mesh);
        stencils.push(s);
    }
    for &[mi, ci] in &m.owner_cells {
        if meshes
            .get(mi)
            .and_then(|x| x.cells.get(ci))
            .is_none_or(|x| x.virtual_cell)
        {
            return Err(invalid("boundary ownership must address physical 2D cells"));
        }
    }
    let owners = Owners::load(&model::resolve(base, &m.geometry_file), m.owner_cells.len())?;
    let mask = read(&t.mask, base, mask_bytes)?;
    let raw = read(&t.perimeter, base, perimeter_bytes)?;
    let perimeter: Vec<_> = raw
        .as_chunks::<4>()
        .0
        .iter()
        .map(|x| f32::from_le_bytes(*x))
        .collect();
    drop(raw);
    let tiles = pool.install(|| {
        t.selected_tiles
            .par_iter()
            .enumerate()
            .map(|(p, &id)| prepare_tile(id, p, t, &mask, &perimeter, &terrain, &owners))
            .collect::<Result<Vec<_>, MapError>>()
    })?;
    let mut points = vec![];
    let mut triangles = vec![];
    if let (Some(p), Some(q)) = (&t.stitch_points, &t.stitch_triangles) {
        for row in read(p, base, point_bytes)?.as_chunks::<32>().0.iter() {
            let mut v = [0.; 4];
            for (x, b) in v.iter_mut().zip(row.as_chunks::<8>().0.iter()) {
                *x = f64::from_le_bytes(*b);
            }
            if v.iter().any(|x| !x.is_finite()) {
                return Err(invalid("nonfinite stitch point"));
            }
            points.push(v);
        }
        for row in read(q, base, tri_bytes)?.as_chunks::<12>().0.iter() {
            let mut v = [0u32; 3];
            for (x, b) in v.iter_mut().zip(row.as_chunks::<4>().0.iter()) {
                let index = i32::from_le_bytes(*b);
                if index < 0 || index as usize >= points.len() {
                    return Err(invalid("invalid stitch triangle index"));
                }
                *x = index as u32;
            }
            triangles.push(v);
        }
    }
    let stitch_owners: Vec<_> = points.iter().map(|p| owners.point([p[0], p[1]])).collect();
    let xy: Vec<_> = points.iter().map(|p| [p[0], p[1]]).collect();
    let clip = aoi(base, &m.aoi_file)?;
    report["interpolation"] = req.interpolation.clone().into();
    report["threads"] = m.threads.into();
    report["effective_inflight_profiles"] = inflight.into();
    report["planned_shared_bytes"] = shared.into();
    report["per_profile_contour_budget_bytes"] = allowance.into();
    report["selected_tiles"] = n.into();
    // A large CPU budget must not split contour memory into unusable units.
    let tile_workers = (m.threads / inflight)
        .max(1)
        .min(n)
        .min((allowance / 2 / (16 << 20)).max(1));
    report["tile_workers_per_profile"] = tile_workers.into();
    report["prepare_seconds"] = tick.elapsed().as_secs_f64().into();
    let render_tick = Instant::now();
    for batch in m.profiles.chunks(inflight) {
        let compute = |profile: &sloping_session::Profile| -> Result<(Value, Value), MapError> {
            let profile_tick = Instant::now();
            let mut cell_values = vec![];
            let mut prepared = vec![];
            for ((mesh, stencil), name) in meshes.iter().zip(&stencils).zip(&profile.values_files) {
                let values = values(&model::resolve(base, name), mesh.cells.len())?;
                if req.interpolation == "sloping" {
                    prepared.push(sloping::prepare_profile(
                        mesh, stencil, &values, 0.001, &pool,
                    )?);
                }
                cell_values.push(values);
            }
            let hydraulic_seconds = profile_tick.elapsed().as_secs_f64();
            let contour_tick = Instant::now();
            let sample = |label: u32,
                          point: [f64; 2],
                          ground: Option<f32>,
                          scratch: &mut sloping::WeightScratch|
             -> Result<Option<f32>, MapError> {
                let Some(ground) = ground else {
                    return Ok(None);
                };
                if label == 0 {
                    return Ok(None);
                }
                let [mesh, cell] = m.owner_cells[label as usize - 1];
                let water = if req.interpolation == "horizontal" {
                    let water = cell_values[mesh][cell];
                    horizontal_surface(water, meshes[mesh].cells[cell].minimum)
                } else {
                    sloping::boundary_surface_value(
                        &stencils, &prepared, mesh, cell, point, scratch,
                    )?
                };
                Ok(water.map(|water| water - ground))
            };
            let mut units = vec![];
            let unit_limits = contour::Limits {
                max_working_bytes: allowance / (2 * tile_workers),
            };
            let mut retained_estimate = 0usize;
            let mut surface_worker_seconds = 0.;
            let mut contour_worker_seconds = 0.;
            for tile_batch in tiles.chunks(tile_workers) {
                let compute_tile = |tile: &TileData| -> Result<_, MapError> {
                    let tick = Instant::now();
                    let mut scratch = sloping::WeightScratch::new(32);
                    let depth = (0..SAMPLES)
                        .map(|index| {
                            let row = index / SIDE;
                            let col = index % SIDE;
                            let point = [
                                tile.transform[0] + (col as f64 + 0.5) * tile.transform[1],
                                tile.transform[3] + (row as f64 + 0.5) * tile.transform[5],
                            ];
                            sample(tile.owner[index], point, tile.ground[index], &mut scratch)
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    let surface_seconds = tick.elapsed().as_secs_f64();
                    let tick = Instant::now();
                    let unit = contour::grid(
                        tile.id as u64,
                        contour::Grid {
                            rows: SIDE,
                            cols: SIDE,
                            transform: tile.transform,
                        },
                        &depth,
                        unit_limits,
                    )?;
                    Ok((unit, surface_seconds, tick.elapsed().as_secs_f64()))
                };
                let computed = tile_batch
                    .par_iter()
                    .map(compute_tile)
                    .collect::<Result<Vec<_>, MapError>>()?;
                for (unit, surface_seconds, contour_seconds) in computed {
                    surface_worker_seconds += surface_seconds;
                    contour_worker_seconds += contour_seconds;
                    retained_estimate = unit
                        .vertex_count()
                        .checked_mul(256)
                        .and_then(|vertices| {
                            unit.ring_count()
                                .checked_mul(128)
                                .and_then(|rings| vertices.checked_add(rings))
                        })
                        .and_then(|current| current.checked_add(retained_estimate))
                        .ok_or_else(|| invalid("boundary retained units size overflow"))?;
                    if retained_estimate > allowance / 2 {
                        return Err(MapError::Budget(
                            "boundary retained contour units exceed profile allowance".into(),
                        ));
                    }
                    units.push(unit);
                }
            }
            if !triangles.is_empty() {
                let tick = Instant::now();
                let mut scratch = sloping::WeightScratch::new(32);
                let depth = points
                    .iter()
                    .zip(&stitch_owners)
                    .map(|(point, &owner)| {
                        sample(
                            owner,
                            [point[0], point[1]],
                            terrain.classify(point[2] as f32).map(|v| v as f32),
                            &mut scratch,
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                surface_worker_seconds += tick.elapsed().as_secs_f64();
                let tick = Instant::now();
                units.push(contour::tin(
                    total_tiles as u64,
                    &xy,
                    &depth,
                    &triangles,
                    contour::Limits {
                        max_working_bytes: allowance / 2,
                    },
                )?);
                contour_worker_seconds += tick.elapsed().as_secs_f64();
            }
            let tick = Instant::now();
            let result = contour::finish(&units, clip.as_ref(), limits)?;
            contour_worker_seconds += tick.elapsed().as_secs_f64();
            let polygon_count = result.coordinates.len();
            let hole_count: usize = result
                .coordinates
                .iter()
                .map(|polygon| polygon.len().saturating_sub(1))
                .sum();
            let vertex_count: usize = result.coordinates.iter().flatten().map(Vec::len).sum();
            let area: f64 = result
                .coordinates
                .iter()
                .map(|polygon| {
                    polygon
                        .iter()
                        .enumerate()
                        .map(|(index, ring)| {
                            let origin = ring.first().copied().unwrap_or([0., 0.]);
                            let area = ring
                                .windows(2)
                                .map(|pair| {
                                    (pair[0][0] - origin[0]) * (pair[1][1] - origin[1])
                                        - (pair[1][0] - origin[0]) * (pair[0][1] - origin[1])
                                })
                                .sum::<f64>()
                                .abs()
                                * 0.5;
                            if index == 0 { area } else { -area }
                        })
                        .sum::<f64>()
                })
                .sum();
            let stats = json!({"polygon_count":polygon_count,"hole_count":hole_count,"vertex_count":vertex_count,"area":area,"hydraulic_seconds":hydraulic_seconds,"contour_seconds":contour_tick.elapsed().as_secs_f64(),"surface_worker_seconds":surface_worker_seconds,"contour_worker_seconds":contour_worker_seconds});
            let product = json!({"type":"FeatureCollection","crs":{"type":"name","properties":{"name":format!("EPSG:{}",m.epsg)}},"features":[{"type":"Feature","properties":{"profile_id":profile.id,"interpolation":req.interpolation,"method_id":contour::METHOD_ID,"native_vector_parity":false},"geometry":{"type":"MultiPolygon","coordinates":result.coordinates}}]});
            Ok((product, stats))
        };
        let products = pool.install(|| {
            batch
                .par_iter()
                .map(compute)
                .collect::<Result<Vec<_>, MapError>>()
        })?;
        for (profile, (product, stats)) in batch.iter().zip(products) {
            let name = format!("{}.geojson", profile.id);
            atomic_json(&out.join(&name), &product)?;
            report["units"]
                .as_array_mut()
                .unwrap()
                .push(json!({"profile_id":profile.id,"status":"complete","path":name}));
            let mut row = stats;
            row["id"] = profile.id.clone().into();
            row["status"] = if row["polygon_count"] == 0 {
                "empty"
            } else {
                "complete"
            }
            .into();
            row["file"] = name.into();
            report["profiles"].as_array_mut().unwrap().push(row);
        }
    }
    report["render_seconds"] = render_tick.elapsed().as_secs_f64().into();
    report["terrain_cache"] =
        serde_json::to_value(terrain.stats()).map_err(|e| invalid(&e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn coverage_gap_and_missing_ground_have_distinct_contour_semantics() {
        // An inactive render block is not a missing ground sample. Surrounding
        // blocks can activate every corner of the central block. This regression
        // records the prototype's vertex-activation behavior explicitly; native
        // mask meaning must be established independently from this fixture.
        let mut mask = vec![1u8; 9];
        mask[4] = 0;
        let active = activated_samples(&mask, 3, 3);
        assert!(active.iter().all(|&value| value));
        let depth: Vec<_> = active.iter().map(|&value| value.then_some(1f32)).collect();
        let grid = contour::Grid {
            rows: 4,
            cols: 4,
            transform: [0., 1., 0., 4., 0., -1.],
        };
        let limits = contour::Limits {
            max_working_bytes: 16 << 20,
        };
        let unit = contour::grid(0, grid, &depth, limits).unwrap();
        let result = contour::finish(&[unit], None, limits).unwrap();
        assert_eq!(result.coordinates.len(), 1);
        assert_eq!(result.coordinates[0].len(), 1);
        // TIFF nodata at a vertex remains None even when the coverage block
        // activates it: no synthetic ground value or flood depth is substituted.
        let mut missing_ground = depth;
        missing_ground[5] = None;
        missing_ground[6] = None;
        missing_ground[9] = None;
        missing_ground[10] = None;
        let unit = contour::grid(0, grid, &missing_ground, limits).unwrap();
        let result = contour::finish(&[unit], None, limits).unwrap();
        assert_ne!(result.coordinates[0][0].len(), 5);
    }
    #[test]
    fn intrinsic_wet_gate_matches_plain_sloping_float32_tolerance() {
        let minimum = 100_f32;
        let threshold = minimum + 0.001_f32;
        assert_eq!(horizontal_surface(minimum, minimum), None);
        assert_eq!(horizontal_surface(threshold, minimum), None);
        let above = f32::from_bits(threshold.to_bits() + 1);
        assert_eq!(horizontal_surface(above, minimum), Some(above));
        assert_eq!(horizontal_surface(sloping::NODATA, -10000.), None);
        assert_eq!(horizontal_surface(1., 0.).map(|w| w - 2.), Some(-1.));
    }
}
