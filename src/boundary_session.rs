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
pub const NATIVE_SCHEMA: &str = "ras-commander-boundary-request/2";
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub schema: String,
    pub interpolation: String,
    pub model: sloping_session::Request,
    pub terrain_metadata: TerrainMetadata,
    #[serde(default)]
    pub contour_method: Option<String>,
    #[serde(default)]
    pub one_d: Option<crate::boundary_xs::XsInput>,
    #[serde(default)]
    pub diagnostic_units: bool,
    #[serde(default)]
    pub additional_terrains: Vec<AdditionalTerrain>,
}
/// Original native source order; each source retains its own sample lattice.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdditionalTerrain {
    pub terrain: String,
    pub crs_identity: model::CrsIdentity,
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
struct TerrainSource<'a> {
    metadata: &'a TerrainMetadata,
    terrain: NativeTerrain,
    data: TerrainMetadataData,
    sequence_offset: usize,
}
enum TerrainMetadataData {
    Resident {
        mask: Vec<u8>,
        perimeter: Vec<f32>,
    },
    Rows {
        mask: crate::boundary_metadata::RowFile,
        perimeter: crate::boundary_metadata::RowFile,
    },
}
enum MetadataFrame<'a> {
    Borrowed(&'a [u8], &'a [f32]),
    Owned(Vec<u8>, Vec<f32>),
}
impl MetadataFrame<'_> {
    fn mask(&self) -> &[u8] {
        match self {
            Self::Borrowed(mask, _) => mask,
            Self::Owned(mask, _) => mask,
        }
    }
    fn perimeter(&self) -> &[f32] {
        match self {
            Self::Borrowed(_, perimeter) => perimeter,
            Self::Owned(_, perimeter) => perimeter,
        }
    }
}
impl TerrainMetadataData {
    fn frame(&self, row: usize) -> Result<MetadataFrame<'_>, MapError> {
        match self {
            Self::Resident { mask, perimeter } => Ok(MetadataFrame::Borrowed(
                &mask[row * TILE * TILE..(row + 1) * TILE * TILE],
                &perimeter[row * 513..(row + 1) * 513],
            )),
            Self::Rows { mask, perimeter } => {
                let mask = mask.read(row)?;
                let raw = perimeter.read(row)?;
                let perimeter = raw
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|v| f32::from_le_bytes(*v))
                    .collect();
                Ok(MetadataFrame::Owned(mask, perimeter))
            }
        }
    }
    fn stats(&self) -> Value {
        match self {
            Self::Resident { .. } => json!({"residency":"whole-source"}),
            Self::Rows { mask, perimeter } => {
                json!({"residency":"active-rows-only", "mask":mask.stats(), "perimeter":perimeter.stats()})
            }
        }
    }
}
fn terrain_inputs(
    t: &TerrainMetadata,
    native: bool,
    base: &Path,
) -> Result<(usize, u64, u64), MapError> {
    if t.tile_shape != [TILE, TILE]
        || t.shape.contains(&0)
        || t.tile_grid != [t.shape[0].div_ceil(TILE), t.shape[1].div_ceil(TILE)]
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
    let total = t.tile_grid[0]
        .checked_mul(t.tile_grid[1])
        .ok_or_else(|| invalid("boundary tile count overflow"))?;
    if native {
        // Full row-major enumeration proves range and uniqueness without an
        // auxiliary allocation before working-memory admission.
        if t.selected_tiles.len() != total
            || t.selected_tiles
                .iter()
                .enumerate()
                .any(|(index, &id)| index != id)
        {
            return Err(MapError::Unsupported(
                "native boundary ordering requires every original source tile before final AOI clipping".into(),
            ));
        }
    } else {
        let mut unique = HashSet::new();
        if t.selected_tiles.is_empty()
            || t.selected_tiles
                .iter()
                .any(|i| *i >= total || !unique.insert(*i))
        {
            return Err(invalid("boundary selected tiles invalid"));
        }
    }
    let n = t.selected_tiles.len();
    Ok((
        total,
        bytes(&t.mask, "|u1", &[n, TILE * TILE], 1, base)?,
        bytes(&t.perimeter, "<f4", &[n, 513], 4, base)?,
    ))
}
fn invalid(s: &str) -> MapError {
    MapError::Invalid(s.into())
}
fn native_ground(z: f64) -> Option<f32> {
    let z = z as f32;
    (z.is_finite() && z != -9999.0).then_some(z)
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
    first_claim: bool,
}
impl Owners {
    fn grid_bounds(transform: [f64; 6]) -> AABB<[f64; 2]> {
        AABB::from_corners(
            [transform[0], transform[3] + SIDE as f64 * transform[5]],
            [transform[0] + SIDE as f64 * transform[1], transform[3]],
        )
    }
    fn intersects_grid(&self, transform: [f64; 6]) -> bool {
        self.index
            .locate_in_envelope_intersecting(Self::grid_bounds(transform))
            .next()
            .is_some()
    }
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
            first_claim: false,
        })
    }
    fn empty() -> Self {
        Self {
            layer: Layer::new("empty ownership"),
            index: RTree::new(),
            first_claim: false,
        }
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
        if self.first_claim {
            layer.features.reverse();
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
        let mut candidates = self.candidates(AABB::from_point(p));
        if !self.first_claim {
            candidates.reverse();
        }
        candidates
            .into_iter()
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
    fn native_point(&self, p: [f64; 2]) -> Result<u32, MapError> {
        let tolerance = if self.first_claim {
            0.0
        } else {
            crate::boundary_point::FUZZY_TOLERANCE
        };
        let candidates = self.candidates(AABB::from_corners(
            [p[0] - tolerance, p[1] - tolerance],
            [p[0] + tolerance, p[1] + tolerance],
        ));
        let mut owner = 0;
        for id in candidates {
            let Some(Geometry::Polygon {
                exterior,
                interiors,
            }) = self.layer.features[id].geometry.as_ref()
            else {
                return Err(invalid(
                    "native boundary ownership requires polygon features",
                ));
            };
            let contains = if self.first_claim {
                crate::boundary_point::contains_polygon_plain(exterior, interiors, p)
            } else {
                crate::boundary_point::contains_polygon(exterior, interiors, p)?
            };
            if contains {
                if self.first_claim {
                    // Native storage uses stored feature order and plain X-ray membership.
                    return Ok(id as u32 + 1);
                }
                if owner != 0 {
                    return Err(MapError::Unsupported(
                        "native boundary stitch point has multiple physical 2D claims; native parallel index order is unqualified".into(),
                    ));
                }
                owner = id as u32 + 1;
            }
        }
        Ok(owner)
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
    transform: [f64; 6],
    bounds: [f64; 4],
    owner: Vec<u32>,
    storage_owner: Vec<u32>,
    ground: Vec<Option<f32>>,
}
fn sample_geometry(
    metadata: &TerrainMetadata,
    id: usize,
    native: bool,
) -> Result<([f64; 6], [f64; 4]), MapError> {
    let g = metadata.transform;
    let row = id / metadata.tile_grid[1];
    let col = id % metadata.tile_grid[1];
    if !native {
        let transform = [
            g[0] + (col * TILE) as f64 * g[1],
            g[1],
            0.,
            g[3] + (row * TILE) as f64 * g[5],
            0.,
            g[5],
        ];
        return Ok((
            transform,
            [
                transform[0],
                transform[3] + SIDE as f64 * transform[5],
                transform[0] + SIDE as f64 * transform[1],
                transform[3],
            ],
        ));
    }
    // Reproduce native RFI extent -> padded tile raster -> extended sample
    // raster assignments. Every subtraction/division is significant in f64.
    let width = metadata.shape[1] as f64;
    let height = metadata.shape[0] as f64;
    let right = g[0] + width * g[1];
    let bottom = g[3] + height * g[5];
    let cell = ((right - g[0]) / width).max((g[3] - bottom) / height);
    let initial_pitch = TILE as f64 * cell;
    let columns = metadata.tile_grid[1] as f64;
    let rows = metadata.tile_grid[0] as f64;
    let padded_right = g[0] + columns * initial_pitch;
    let padded_bottom = g[3] - rows * initial_pitch;
    let pitch = ((padded_right - g[0]) / columns).max((g[3] - padded_bottom) / rows);
    let left = g[0] + pitch * col as f64;
    let right = g[0] + pitch * (col + 1) as f64;
    let top = g[3] - pitch * row as f64;
    let bottom = g[3] - pitch * (row + 1) as f64;
    let tile_cell = ((right - left) / TILE as f64).max((top - bottom) / TILE as f64);
    let right = right + tile_cell;
    let bottom = bottom - tile_cell;
    let sample_cell = ((right - left) / SIDE as f64).max((top - bottom) / SIDE as f64);
    let bounds = [left, bottom, right, top];
    if bounds.iter().any(|v| !v.is_finite()) || !sample_cell.is_finite() || sample_cell <= 0. {
        return Err(invalid("native boundary sample geometry overflow"));
    }
    Ok(([left, sample_cell, 0., top, 0., -sample_cell], bounds))
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
#[allow(clippy::too_many_arguments)] // Explicit immutable source metadata and coverage layers.
fn prepare_tile(
    id: usize,
    position: usize,
    metadata: &TerrainMetadata,
    mask: &[u8],
    perimeter: &[f32],
    terrain: &NativeTerrain,
    owners: &Owners,
    storage_owners: &Owners,
    has_xs: bool,
    native: bool,
) -> Result<TileData, MapError> {
    let row = (id / metadata.tile_grid[1]) * TILE;
    let col = (id % metadata.tile_grid[1]) * TILE;
    let (transform, bounds) = sample_geometry(metadata, id, native)?;
    let owner = owners.grid(transform)?;
    let storage_owner = storage_owners.grid(transform)?;
    let start = position * TILE * TILE;
    let valid = activated_samples(&mask[start..start + TILE * TILE], TILE, TILE);
    let mut ground = vec![None; SAMPLES];
    let mut blocks = HashMap::new();
    for r in 0..SIDE {
        for c in 0..SIDE {
            let k = r * SIDE + c;
            if !valid[k] || (owner[k] == 0 && storage_owner[k] == 0 && !has_xs) {
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
            if v.is_infinite() {
                return Err(invalid("infinite boundary terrain sample"));
            }
            if native {
                // Native tile/perimeter buffers use the fixed Float32 sentinel,
                // without interpreting the original TIFF's nodata metadata.
                ground[k] = native_ground(f64::from(v));
            } else if let Some(v) = terrain.classify(v) {
                ground[k] = Some(v as f32);
            }
        }
    }
    Ok(TileData {
        transform,
        bounds,
        owner,
        storage_owner,
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
fn write_diagnostic_units(path: &Path, units: &[contour::Unit]) -> Result<(), MapError> {
    // Stream ordered integer operands without copying the retained contours.
    let partial = path.with_extension("partial");
    let mut file = std::io::BufWriter::new(fs::File::create(&partial)?);
    file.write_all(b"{\"schema\":\"fast-map-boundary-ordered-units/1\",\"units\":[")?;
    for (index, unit) in units.iter().enumerate() {
        if index != 0 {
            file.write_all(b",")?;
        }
        write!(file, "{{\"sequence\":{},\"rings\":[", unit.sequence)?;
        for (ring_index, ring) in unit.paths.iter().enumerate() {
            if ring_index != 0 {
                file.write_all(b",")?;
            }
            file.write_all(b"[")?;
            for (point_index, point) in ring.iter().enumerate() {
                if point_index != 0 {
                    file.write_all(b",")?;
                }
                write!(file, "[{},{}]", point.x, point.y)?;
            }
            file.write_all(b"]")?;
        }
        file.write_all(b"]}")?;
    }
    file.write_all(b"]}")?;
    file.flush()?;
    drop(file);
    fs::rename(partial, path)?;
    Ok(())
}
#[allow(clippy::too_many_arguments)]
fn composed_depth(
    owner: u32,
    water: Option<f32>,
    storage_owner: u32,
    levels: &[f32],
    xs_claim: u8,
    xs_depth: f32,
    xs_water: f32,
    ground: Option<f32>,
) -> Result<Option<f32>, MapError> {
    use crate::boundary_surface::{SampleInputs, SignedSample, compose_sample};
    let two_d = match (water, ground) {
        (Some(wse), Some(ground)) => SignedSample {
            depth: wse - ground,
            wse,
        },
        _ => SignedSample::missing(),
    };
    let storage_level = if storage_owner == 0 {
        f32::NAN
    } else {
        *levels
            .get(storage_owner as usize - 1)
            .ok_or_else(|| invalid("boundary storage label outside profile"))?
    };
    let result = compose_sample(SampleInputs {
        two_d_owner: owner,
        two_d,
        storage_owner,
        storage_level,
        xs_claim: xs_claim != 0,
        xs: SignedSample {
            depth: xs_depth,
            wse: xs_water,
        },
        terrain: ground.unwrap_or(f32::NAN),
    })?;
    Ok(result
        .values
        .depth
        .is_finite()
        .then_some(result.values.depth))
}
pub fn method_record() -> Value {
    json!({"method_id":contour::METHOD_ID,"coordinate_precision":0.001,"native_vector_parity":false,"scope":"physical 2D cells; horizontal or plain corner sloping; deterministic zero-depth contours","differences":"stable ordered all-subject integer union; RASMapper polygons can differ in holes and quantized coordinates","depth_cutoff":0,"cell_wet_tolerance":0.001,"cell_wet_rule":"float32 cell WSE > float32(minimum elevation + 0.001); no per-pixel depth filtering"})
}
pub fn native_method_record() -> Value {
    json!({"method_id":crate::boundary_native_contour::METHOD_ID,
        "coordinate_precision":0.001,"native_vector_parity":false,
        "native_parity_qualified":false,
        "scope":"ordered original terrain sources; plain 2D, stored XS and storage signed samples; explicit native serial ring order",
        "native_reference_order":"serial source/tile/stitch order",
        "depth_cutoff":0,"unsupported":"terrain modifications, face options, encroachment, blocked polygons and ambiguous/fuzzy physical 2D stitch-point ownership"})
}
/// One bounded batch. The complete physical geometry and hydraulics remain global.
pub fn run(req: &Request, base: &Path, out: &Path) -> Result<Value, MapError> {
    if out.exists() && fs::read_dir(out)?.next().is_some() {
        return Err(invalid("boundary output directory must be empty"));
    }
    fs::create_dir_all(out)?;
    let method = if req.schema == NATIVE_SCHEMA {
        native_method_record()
    } else {
        method_record()
    };
    let mut report = json!({"schema":"ras-commander-boundary-report/1","status":"in_progress","method":method,"units":[],"profiles":[]});
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
    let native = req.schema == NATIVE_SCHEMA;
    if (!native && req.schema != SCHEMA)
        || (native
            && req.contour_method.as_deref() != Some(crate::boundary_native_contour::METHOD_ID))
        || (!native
            && (req.one_d.is_some()
                || req.contour_method.is_some()
                || req.diagnostic_units
                || !req.additional_terrains.is_empty()))
        || m.schema != sloping_session::SCHEMA
        || !["horizontal", "sloping"].contains(&req.interpolation.as_str())
        || m.options != sloping::Options::default()
    {
        return Err(invalid("unsupported boundary schema/interpolation/options"));
    }
    if m.velocity_normals_files.is_some()
        || m.profiles.iter().any(|p| p.face_velocity_files.is_some())
    {
        return Err(MapError::Unsupported(
            "velocity products are not part of boundary requests".into(),
        ));
    }
    if m.terrain_scope.is_some() {
        return Err(MapError::Unsupported(
            "top-priority-source terrains are not qualified for boundaries".into(),
        ));
    }
    if !native
        && (m.storage_areas_file.is_some()
            || m.storage_area_count != 0
            || m.profiles
                .iter()
                .any(|p| p.storage_area_values_file.is_some()))
    {
        return Err(MapError::Unsupported(
            "storage-area surfaces are not qualified for boundaries".into(),
        ));
    }
    if m.epsg == 0
        || m.threads == 0
        || m.threads > 64
        || m.max_inflight_profiles == 0
        || m.profiles.is_empty()
        || (m.mesh_files.is_empty() != m.owner_cells.is_empty())
        || (m.mesh_files.is_empty() && req.one_d.is_none() && m.storage_area_count == 0)
        || m.owner_cells.len() >= u32::MAX as usize
        || m.storage_area_count >= u32::MAX as usize
    {
        return Err(invalid("invalid boundary threads/profiles/geometry"));
    }
    if (m.storage_area_count == 0) != m.storage_areas_file.is_none()
        || m.profiles
            .iter()
            .any(|p| p.storage_area_values_file.is_some() != (m.storage_area_count > 0))
    {
        return Err(invalid(
            "boundary storage descriptors must be complete for every profile",
        ));
    }
    if let Some(xs) = &req.one_d {
        if xs
            .profiles
            .iter()
            .map(|p| &p.id)
            .ne(m.profiles.iter().map(|p| &p.id))
        {
            return Err(invalid(
                "boundary XS and 2D profile order/identities differ",
            ));
        }
        if req.interpolation != "sloping" {
            return Err(MapError::Unsupported(
                "XS boundary surfaces require sloping interpolation".into(),
            ));
        }
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
    if t.transform != m.terrain_transform {
        return Err(invalid("boundary model/source transform mismatch"));
    }
    let descriptors: Vec<_> = std::iter::once((&m.terrain, &m.crs_identity, t))
        .chain(req.additional_terrains.iter().map(|source| {
            (
                &source.terrain,
                &source.crs_identity,
                &source.terrain_metadata,
            )
        }))
        .collect();
    if descriptors.len() > 256 {
        return Err(MapError::Unsupported(
            "boundary supports at most 256 original terrain sources".into(),
        ));
    }
    let mut plans = Vec::with_capacity(descriptors.len());
    let mut total_tiles = 0usize;
    let mut n = 0usize;
    let mut input = 0u64;
    let mut terrain_metadata_bytes = 0u64;
    for (index, (_, _, metadata)) in descriptors.iter().enumerate() {
        if index > 0 && (metadata.stitch_points.is_some() || metadata.stitch_triangles.is_some()) {
            return Err(invalid(
                "boundary mosaic global stitch TIN belongs only to the primary descriptor",
            ));
        }
        let plan = terrain_inputs(metadata, native, base)?;
        total_tiles = total_tiles
            .checked_add(plan.0)
            .ok_or_else(|| invalid("boundary source tile count overflow"))?;
        n = n
            .checked_add(metadata.selected_tiles.len())
            .ok_or_else(|| invalid("boundary selected tile count overflow"))?;
        input = input
            .checked_add(plan.1)
            .and_then(|v| v.checked_add(plan.2))
            .ok_or_else(|| invalid("boundary input size overflow"))?;
        terrain_metadata_bytes = terrain_metadata_bytes
            .checked_add(plan.1)
            .and_then(|v| v.checked_add(plan.2))
            .ok_or_else(|| invalid("boundary terrain metadata size overflow"))?;
        plans.push(plan);
    }
    if !m.owner_cells.is_empty() {
        input = input
            .checked_add(fs::metadata(model::resolve(base, &m.geometry_file))?.len())
            .ok_or_else(|| invalid("boundary geometry budget overflow"))?;
    }
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
    if let Some(name) = &m.storage_areas_file {
        input = input
            .checked_add(fs::metadata(model::resolve(base, name))?.len())
            .ok_or_else(|| invalid("boundary storage budget overflow"))?;
    }
    let xs_bytes = match &req.one_d {
        Some(xs) => crate::boundary_xs::planned_bytes(xs)? as u64,
        None => 0,
    };
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
    let mut opened = Vec::with_capacity(descriptors.len());
    let mut source_paths = HashSet::new();
    for (path, identity, metadata) in &descriptors {
        let path = model::resolve(base, path);
        if !source_paths.insert(fs::canonicalize(&path)?) {
            return Err(invalid("duplicate original boundary terrain source"));
        }
        let terrain = NativeTerrain::open(
            path.to_str()
                .ok_or_else(|| invalid("non-UTF8 terrain path"))?,
            m.epsg,
            identity,
            QueryConvention::PixelCenter,
            NodataPolicy::Dry,
            64 << 20,
        )?;
        if [terrain.height, terrain.width] != metadata.shape || terrain.gt != metadata.transform {
            return Err(invalid(
                "boundary original terrain metadata disagrees with TIFF",
            ));
        }
        opened.push(terrain);
    }
    let cache_bytes = (opened.len() as u64)
        .checked_mul(64 << 20)
        .ok_or_else(|| invalid("boundary terrain cache budget overflow"))?;
    let decoder_bytes = opened
        .iter()
        .map(NativeTerrain::decoder_transient_bytes_per_load)
        .max()
        .unwrap_or(0);
    let resident_input = if native {
        input - terrain_metadata_bytes
    } else {
        input
    };
    let metadata_address_bytes = if native {
        (n as u64)
            .checked_mul(32)
            .and_then(|v| v.checked_add((m.threads as u64) * (TILE * TILE + 513 * 8) as u64))
            .ok_or_else(|| invalid("boundary metadata row budget overflow"))?
    } else {
        0
    };
    let unit_container_bytes = if native {
        n.checked_add(1)
            .and_then(|v| v.checked_mul(std::mem::size_of::<contour::Unit>()))
            .ok_or_else(|| invalid("boundary unit address budget overflow"))? as u64
    } else {
        0
    };
    // Conservative geometry decode/ownership inflation plus shared tile payloads;
    // remaining memory is explicitly divided among concurrent contour builders.
    let shared = resident_input
        .checked_mul(16)
        .and_then(|v| v.checked_add(metadata_address_bytes))
        .and_then(|x| {
            (m.threads as u64)
                .checked_mul((SAMPLES * if native { 64 } else { 32 }) as u64)
                .and_then(|y| x.checked_add(y))
        })
        .and_then(|x| x.checked_add(cache_bytes))
        .and_then(|x| x.checked_add(xs_bytes))
        .and_then(|x| {
            decoder_bytes
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
    let hydraulic_allowance = resident_input
        .checked_mul(8)
        .ok_or_else(|| invalid("boundary hydraulic budget overflow"))?;
    let slot = hydraulic_allowance
        .checked_add(16 << 20)
        .and_then(|v| {
            unit_container_bytes
                .checked_mul(2)
                .and_then(|u| v.checked_add(u))
        })
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
    let owners = if m.owner_cells.is_empty() {
        Owners::empty()
    } else {
        Owners::load(&model::resolve(base, &m.geometry_file), m.owner_cells.len())?
    };
    let storage_owners = if let Some(path) = &m.storage_areas_file {
        let mut owners = Owners::load(&model::resolve(base, path), m.storage_area_count)?;
        owners.first_claim = true;
        owners
    } else {
        Owners::empty()
    };
    let xs = match &req.one_d {
        Some(input) => Some(crate::boundary_xs::PreparedXs::load(
            input,
            base,
            usize::try_from(m.max_working_bytes - (shared - xs_bytes))
                .map_err(|_| invalid("boundary XS budget exceeds platform"))?,
        )?),
        None => None,
    };
    let mut sources = Vec::with_capacity(descriptors.len());
    let mut sequence_offset = 0usize;
    for (((_, _, metadata), terrain), (count, mask_bytes, perimeter_bytes)) in
        descriptors.iter().zip(opened).zip(plans)
    {
        let data = if native {
            TerrainMetadataData::Rows {
                mask: crate::boundary_metadata::RowFile::open(
                    &model::resolve(base, &metadata.mask.path),
                    metadata.selected_tiles.len(),
                    TILE * TILE,
                )?,
                perimeter: crate::boundary_metadata::RowFile::open(
                    &model::resolve(base, &metadata.perimeter.path),
                    metadata.selected_tiles.len(),
                    513 * 4,
                )?,
            }
        } else {
            let mask = read(&metadata.mask, base, mask_bytes)?;
            let raw = read(&metadata.perimeter, base, perimeter_bytes)?;
            let perimeter = raw
                .as_chunks::<4>()
                .0
                .iter()
                .map(|x| f32::from_le_bytes(*x))
                .collect();
            TerrainMetadataData::Resident { mask, perimeter }
        };
        sources.push(TerrainSource {
            metadata,
            terrain,
            data,
            sequence_offset,
        });
        sequence_offset += count; // Checked total above, and count is nonnegative.
    }
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
            if native && !(v[2] as f32).is_finite() {
                return Err(invalid("native stitch elevation exceeds Float32 range"));
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
    let stitch_owners: Vec<_> = points
        .iter()
        .map(|p| {
            if native {
                owners.native_point([p[0], p[1]])
            } else {
                Ok(owners.point([p[0], p[1]]))
            }
        })
        .collect::<Result<_, _>>()?;
    let stitch_storage: Vec<_> = points
        .iter()
        .map(|p| {
            if native {
                storage_owners.native_point([p[0], p[1]])
            } else {
                Ok(storage_owners.point([p[0], p[1]]))
            }
        })
        .collect::<Result<_, _>>()?;
    let xy: Vec<_> = points.iter().map(|p| [p[0], p[1]]).collect();
    let clip = aoi(base, &m.aoi_file)?;
    report["interpolation"] = req.interpolation.clone().into();
    report["threads"] = m.threads.into();
    report["effective_inflight_profiles"] = inflight.into();
    report["planned_shared_bytes"] = shared.into();
    if native {
        report["terrain_metadata_staged_bytes"] = terrain_metadata_bytes.into();
        report["terrain_metadata_residency"] = "active-rows-only".into();
        report["planned_unit_container_bytes_per_profile"] = unit_container_bytes.into();
    }
    report["per_profile_contour_budget_bytes"] = allowance.into();
    report["selected_tiles"] = n.into();
    if native {
        report["native_reference_order"] = "serial source/tile/stitch order".into();
    }
    report["terrain_tile_residency"] = "bounded by active tile/profile workers".into();
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
            let storage_levels = match &profile.storage_area_values_file {
                Some(path) => {
                    crate::storage_area::values(&model::resolve(base, path), m.storage_area_count)?
                }
                None => vec![],
            };
            let xs_levels = xs.as_ref().map(|xs| xs.levels(&profile.id)).transpose()?;
            let hydraulic_seconds = profile_tick.elapsed().as_secs_f64();
            let contour_tick = Instant::now();
            let sample = |label: u32,
                          point: [f64; 2],
                          ground: Option<f32>,
                          scratch: &mut sloping::WeightScratch|
             -> Result<Option<f32>, MapError> {
                let Some(_) = ground else {
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
                Ok(water)
            };
            let mut units = vec![];
            if native {
                units
                    .try_reserve_exact(n + 1)
                    .map_err(|e| MapError::Budget(format!("boundary unit addresses: {e}")))?;
            }
            let unit_limits = contour::Limits {
                max_working_bytes: allowance / (2 * tile_workers),
            };
            let mut retained_estimate = 0usize;
            let mut surface_worker_seconds = 0.;
            let mut contour_worker_seconds = 0.;
            for source in &sources {
                for (batch, tile_batch) in source
                    .metadata
                    .selected_tiles
                    .chunks(tile_workers)
                    .enumerate()
                {
                    let compute_tile = |(offset, &id): (usize, &usize)| -> Result<_, MapError> {
                        let tick = Instant::now();
                        let sequence = (source.sequence_offset + id) as u64;
                        if native {
                            let (transform, bounds) = sample_geometry(source.metadata, id, true)?;
                            let grid = contour::Grid {
                                rows: SIDE,
                                cols: SIDE,
                                transform,
                            };
                            grid.validate(SAMPLES)?;
                            contour::quantize(grid.point(0, 0))?;
                            contour::quantize(grid.point(SIDE - 1, SIDE - 1))?;
                            unit_limits.admit(0, 0)?;
                            let xs_intersects = match &xs {
                                Some(xs) => xs.surface().intersects_signed_raster(
                                    crate::one_d_native::SampleRaster { bounds, size: SIDE },
                                )?,
                                None => false,
                            };
                            // Match the actual ownership/XS candidate queries, independent
                            // of profile wetness and final AOI. No possible claim means
                            // every sample is missing and the ordered tile unit is empty.
                            if !owners.intersects_grid(transform)
                                && !storage_owners.intersects_grid(transform)
                                && !xs_intersects
                            {
                                return Ok((
                                    contour::Unit {
                                        sequence,
                                        paths: vec![],
                                    },
                                    tick.elapsed().as_secs_f64(),
                                    0.0,
                                ));
                            }
                        }
                        let frame = source.data.frame(batch * tile_workers + offset)?;
                        if native && frame.mask().iter().all(|&value| value == 0) {
                            return Ok((
                                contour::Unit {
                                    sequence,
                                    paths: vec![],
                                },
                                tick.elapsed().as_secs_f64(),
                                0.0,
                            ));
                        }
                        let tile = prepare_tile(
                            id,
                            0,
                            source.metadata,
                            frame.mask(),
                            frame.perimeter(),
                            &source.terrain,
                            &owners,
                            &storage_owners,
                            xs.is_some(),
                            native,
                        )?;
                        let mut xs_depth = if xs.is_some() {
                            vec![f32::NAN; SAMPLES]
                        } else {
                            vec![]
                        };
                        let mut xs_water = if xs.is_some() {
                            vec![f32::NAN; SAMPLES]
                        } else {
                            vec![]
                        };
                        let mut xs_claims = if xs.is_some() {
                            vec![0_u8; SAMPLES]
                        } else {
                            vec![]
                        };
                        if let (Some(xs), Some(levels)) = (&xs, xs_levels) {
                            let ground: Vec<_> =
                                tile.ground.iter().map(|v| v.unwrap_or(f32::NAN)).collect();
                            for (i, claim) in xs_claims.iter_mut().enumerate() {
                                *claim = u8::from(tile.owner[i] != 0 || tile.storage_owner[i] != 0);
                            }
                            xs.surface().render_signed_raster(
                                levels,
                                &ground,
                                crate::one_d_native::SampleRaster {
                                    bounds: tile.bounds,
                                    size: SIDE,
                                },
                                &mut xs_depth,
                                &mut xs_water,
                                &mut xs_claims,
                            )?;
                        }
                        let mut scratch = sloping::WeightScratch::new(32);
                        let depth = (0..SAMPLES)
                            .map(|index| {
                                let row = index / SIDE;
                                let col = index % SIDE;
                                let point = [
                                    tile.transform[0] + (col as f64 + 0.5) * tile.transform[1],
                                    tile.transform[3] + (row as f64 + 0.5) * tile.transform[5],
                                ];
                                let water = sample(
                                    tile.owner[index],
                                    point,
                                    tile.ground[index],
                                    &mut scratch,
                                )?;
                                composed_depth(
                                    tile.owner[index],
                                    water,
                                    tile.storage_owner[index],
                                    &storage_levels,
                                    xs_claims.get(index).copied().unwrap_or(0),
                                    xs_depth.get(index).copied().unwrap_or(f32::NAN),
                                    xs_water.get(index).copied().unwrap_or(f32::NAN),
                                    tile.ground[index],
                                )
                            })
                            .collect::<Result<Vec<_>, _>>()?;
                        let surface_seconds = tick.elapsed().as_secs_f64();
                        let tick = Instant::now();
                        let grid_contours = if native {
                            crate::boundary_native_contour::grid
                        } else {
                            contour::grid
                        };
                        let unit = grid_contours(
                            sequence,
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
                        .enumerate()
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
                            .and_then(|current| {
                                current.checked_add(if native {
                                    std::mem::size_of::<contour::Unit>()
                                } else {
                                    0
                                })
                            })
                            .ok_or_else(|| invalid("boundary retained units size overflow"))?;
                        if retained_estimate > allowance / 2 {
                            return Err(MapError::Budget(
                                "boundary retained contour units exceed profile allowance".into(),
                            ));
                        }
                        units.push(unit);
                    }
                }
            }
            if !triangles.is_empty() {
                let tick = Instant::now();
                let mut scratch = sloping::WeightScratch::new(32);
                let ground: Vec<_> = points
                    .iter()
                    .map(|point| {
                        if native {
                            // Global TIN Z is independent of each TIFF's nodata tag.
                            native_ground(point[2])
                        } else {
                            sources[0]
                                .terrain
                                .classify(point[2] as f32)
                                .map(|v| v as f32)
                        }
                    })
                    .collect();
                let mut xs_depth = if xs.is_some() {
                    vec![f32::NAN; points.len()]
                } else {
                    vec![]
                };
                let mut xs_water = if xs.is_some() {
                    vec![f32::NAN; points.len()]
                } else {
                    vec![]
                };
                let mut xs_claims = if xs.is_some() {
                    vec![0_u8; points.len()]
                } else {
                    vec![]
                };
                if let (Some(xs), Some(levels)) = (&xs, xs_levels) {
                    let terrain: Vec<_> = ground.iter().map(|v| v.unwrap_or(f32::NAN)).collect();
                    for (i, claim) in xs_claims.iter_mut().enumerate() {
                        *claim = u8::from(stitch_owners[i] != 0 || stitch_storage[i] != 0);
                    }
                    xs.surface().sample_signed_points(
                        levels,
                        &xy,
                        &terrain,
                        &mut xs_depth,
                        &mut xs_water,
                        &mut xs_claims,
                    )?;
                }
                let depth = points
                    .iter()
                    .enumerate()
                    .map(|(i, point)| {
                        let water = sample(
                            stitch_owners[i],
                            [point[0], point[1]],
                            ground[i],
                            &mut scratch,
                        )?;
                        composed_depth(
                            stitch_owners[i],
                            water,
                            stitch_storage[i],
                            &storage_levels,
                            xs_claims.get(i).copied().unwrap_or(0),
                            xs_depth.get(i).copied().unwrap_or(f32::NAN),
                            xs_water.get(i).copied().unwrap_or(f32::NAN),
                            ground[i],
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                surface_worker_seconds += tick.elapsed().as_secs_f64();
                let tick = Instant::now();
                let tin_contours = if native {
                    crate::boundary_native_contour::tin
                } else {
                    contour::tin
                };
                units.push(tin_contours(
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
            if req.diagnostic_units {
                write_diagnostic_units(&out.join(format!("{}.units.json", profile.id)), &units)?;
            }
            let result = if native {
                crate::boundary_native_contour::finish(&units, clip.as_ref(), limits)?
            } else {
                contour::finish(&units, clip.as_ref(), limits)?
            };
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
            let product = json!({"type":"FeatureCollection","crs":{"type":"name","properties":{"name":format!("EPSG:{}",m.epsg)}},"features":[{"type":"Feature","properties":{"profile_id":profile.id,"interpolation":req.interpolation,"method_id":result.method_id,"native_vector_parity":false},"geometry":{"type":"MultiPolygon","coordinates":result.coordinates}}]});
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
        serde_json::to_value(sources[0].terrain.stats()).map_err(|e| invalid(&e.to_string()))?;
    if native {
        report["terrain_source_count"] = sources.len().into();
        report["terrain_sources"] = json!(
            sources
                .iter()
                .map(|source| json!({
                        "path":source.terrain.path,"sequence_offset":source.sequence_offset,
                        "selected_tiles":source.metadata.selected_tiles.len(),
                "terrain_cache":source.terrain.stats(), "terrain_metadata":source.data.stats()
                    }))
                .collect::<Vec<_>>()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_ground_uses_global_float32_sentinel_not_source_nodata() {
        assert_eq!(native_ground(-32768.0), Some(-32768.0));
        assert_eq!(native_ground(-9999.0), None);
        // Cast precedes the native sentinel test.
        assert_eq!(native_ground(-9999.00001), None);
        assert_eq!(native_ground(f64::NAN), None);
        assert_eq!(native_ground(f64::MAX), None);
        assert_eq!(native_ground(-0.0).unwrap().to_bits(), (-0.0f32).to_bits());
    }
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
