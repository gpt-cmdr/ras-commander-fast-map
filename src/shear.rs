//! RAS Mapper Max shear stress for 2D meshes (`rasmapper-face-shear-max/1`).
//!
//! RAS Mapper 7.0.1 does not draw Max shear stress from a per-pixel time
//! series. Its post-processor computes a shear vector on every face at every
//! stored output time, keeps each face's largest magnitude (PostProcessing
//! `Maximum Shear`), and the stored map interpolates those face scalars:
//!
//! 1. face water surfaces: the sloping face classification already used by
//!    the WSE renderer (`Mesh::reconstruct`, RAS Mapper `ComputeFaceWSsNew`);
//! 2. face hydraulic radius, connected faces only, at the face water surface
//!    `ValueA` from the HDF `Faces Area Elevation` table: area and top width by
//!    linear interpolation of area over elevation (above the table, the last
//!    area plus the face polyline length times the excess), wetted perimeter
//!    by the native interpolation `P[i-1] * t + P[i] * (1 - t)` (constant above
//!    the table), `R = area / perimeter` (binary32). Other faces: 0;
//! 3. facepoint velocity vectors, per facepoint and hydraulically connected
//!    arc: weighted least squares of the arc's face-normal velocities with
//!    inverse polyline-length weights (`ComputeSlopingFacepointVelocities_LeastSquares`);
//!    zero where every incident face is dry on both sides;
//! 4. face true velocity of a connected face: the mean of its two facepoint
//!    arc vectors (`ComputeFaceTrueVelocities`); other faces: zero;
//! 5. face shear (`ComputeFaceTrueShear`): `k * |V| * V * n^2 / R^(1/3)` in
//!    binary32 with `R^(1/3)` from binary64 `pow`, `k` = 28.258362 (US
//!    Customary, 62.4 / 1.486^2) or 9810 (SI), `n` the face's first `Faces Area
//!    Elevation` Manning's n; zero when `V = 0` or `R = 0`;
//! 6. Max: the per-face maximum of `|tau|` over every stored output time
//!    (initial 0, strict `>`);
//! 7. pixels: each facepoint takes the binary32 mean of its incident faces'
//!    Max values, painted with the plain sloping facepoint weights
//!    (`FacepointWeightedPaint`) in cells whose Max WSE passes the native wet
//!    gate, drawn where the clipping depth is > 0. The values do not depend on
//!    the WSE render mode; the active mode's Max depth is the mask.
//!
//! Qualification is recorded by [`method_record`].
use crate::{
    MapError,
    sloping::{CellStencil, Corner, FaceWater, Mesh, NODATA, WeightScratch, paint_corner_values},
    velocity::{FaceNormal, SourceLattice, VelocityDepth},
};
use rayon::prelude::*;
use serde::Deserialize;
use std::{
    fs,
    io::{BufReader, Read},
    path::Path,
};

pub const METHOD_ID: &str = "rasmapper-face-shear-max/1";
/// RAS Mapper `MeshFV2D.ComputeFaceTrueShear` unit factors (binary32).
pub const FACTOR_US: f32 = 28.258362;
pub const FACTOR_SI: f32 = 9810.0;
/// RAS Mapper's `MinWSPlotTolerance` (cell wet gate and face classification).
const PLOT_TOLERANCE: f32 = 0.001;

pub fn method_record() -> serde_json::Value {
    serde_json::json!({
        "method_id": METHOD_ID,
        "product": "shear stress (lb/ft2 or N/m2), RAS Mapper stored-map Max rule, independent of the WSE render mode",
        "profiles": ["Max"],
        "inputs": "per mesh: HDF face normals with polyline face lengths, Faces Area Elevation Info/Values (elevation, area, wetted perimeter, Manning's n), and every stored output time's cell Water Surface and Face Velocity; the request profile supplies Maximum Water Surface",
        "face_water_surface": "sloping face classification (ComputeFaceWSsNew); connected = not None and not Levee",
        "hydraulic_radius": "connected faces at ValueA: area/top width by linear area interpolation (beyond the table: last area + polyline length * excess), perimeter P[i-1]*t + P[i]*(1-t) (constant beyond), R = area / perimeter in binary32; 0 elsewhere",
        "facepoint_velocity": "inverse polyline-length weighted least squares of the face-normal velocities over each hydraulically connected arc; zero where every incident face is dry",
        "face_velocity": "connected faces: mean of the two facepoint arc vectors; others zero",
        "face_shear": "k*|V|*V*n*n / float32(pow(R, 0.33333333333333337)) in binary32, k = 28.258362 (US) or 9810 (SI), n = first Faces Area Elevation Manning's n; 0 when V = 0 or R = 0",
        "max": "per face maximum of |tau| over every stored output time (PostProcessing Maximum Shear)",
        "pixels": "facepoint = binary32 mean of incident face maxima; plain sloping facepoint weights (square cells on the native source-tile lattice), cell Max WSE > minimum + 0.001, drawn where the clipping depth > 0",
        "clipping_depth": "shear_stress.depth: rendered (this request's render mode, default) or horizontal (cell-constant Max WSE)",
        "qualified": "native HEC-RAS 7.0.1 Shear Stress (Max), complete masks and Float32 bits exact: BaldEagleCrkMulti2D bridges p05 sloping (973,902 pixels) and horizontal (973,532), Davis p02 sloping (22,041) and horizontal (20,560); face Max equal to PostProcessing Maximum Shear (docs/shear-stress.md)",
        "not_rendered": "stored output times, storage areas, 1D, slopingPretty face options"
    })
}

/// Shear-stress inputs of one sloping request.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// Plan `Units System`: `US Customary` or `SI Units`.
    pub units: String,
    /// Depth that clips the product (default: the rendered depth).
    #[serde(default)]
    pub depth: VelocityDepth,
    pub meshes: Vec<MeshRequest>,
}

/// Per-mesh shear-stress inputs (little-endian, HDF order).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeshRequest {
    /// float32 (x, y, polyline length) per face, as `velocity_normals_files`.
    pub normals_file: String,
    /// int32 (start row, row count) per face: `Faces Area Elevation Info`.
    pub tables_info_file: String,
    /// float32 (elevation, area, wetted perimeter, Manning's n) rows:
    /// `Faces Area Elevation Values`.
    pub tables_values_file: String,
    pub table_rows: usize,
    /// Number of stored output times.
    pub times: usize,
    /// float32 time x cells `Water Surface` (cells as the mesh file, ghosts included).
    pub water_surface_file: String,
    /// float32 time x faces `Face Velocity`.
    pub face_velocity_file: String,
}

pub fn factor(units: &str) -> Result<f32, MapError> {
    match units {
        "US Customary" => Ok(FACTOR_US),
        "SI Units" => Ok(FACTOR_SI),
        other => Err(MapError::Unsupported(format!(
            "shear stress needs US Customary or SI Units, not {other:?}"
        ))),
    }
}

/// `Faces Area Elevation` per face: Manning's n (first row) and the
/// (elevation, area, wetted perimeter) rows.
pub struct FaceTables {
    start: Vec<usize>,
    count: Vec<usize>,
    rows: Vec<[f32; 3]>,
    pub mann: Vec<f32>,
}

impl FaceTables {
    pub fn new(info: &[[i32; 2]], values: &[[f32; 4]]) -> Result<Self, MapError> {
        let mut start = Vec::with_capacity(info.len());
        let mut count = Vec::with_capacity(info.len());
        let mut mann = Vec::with_capacity(info.len());
        for &[s, c] in info {
            // RAS Mapper reads every face's first Manning's n; an empty table
            // has none (its reader fails), so the mesh is refused.
            if s < 0
                || c <= 0
                || (s as usize)
                    .checked_add(c as usize)
                    .is_none_or(|e| e > values.len())
            {
                return Err(MapError::Unsupported(
                    "shear stress needs a non-empty Faces Area Elevation table on every face"
                        .into(),
                ));
            }
            start.push(s as usize);
            count.push(c as usize);
            mann.push(values[s as usize][3]);
        }
        if values.iter().flatten().any(|v| !v.is_finite()) {
            return Err(MapError::Invalid(
                "Faces Area Elevation Values must be finite".into(),
            ));
        }
        Ok(Self {
            start,
            count,
            rows: values.iter().map(|r| [r[0], r[1], r[2]]).collect(),
            mann,
        })
    }

    fn table(&self, face: usize) -> &[[f32; 3]] {
        &self.rows[self.start[face]..self.start[face] + self.count[face]]
    }

    /// `ComputeFaceAreaAndTopWidth` then `ComputeFacePerimeter`; `area / perimeter`.
    pub fn hydraulic_radius(&self, face: usize, ws: f32, face_length: f32) -> f32 {
        let t = self.table(face);
        let last = t[t.len() - 1];
        let mut area = last[1] + face_length * (ws - last[0]);
        for i in 1..t.len() {
            if t[i][0] >= ws {
                let top_width = (t[i][1] - t[i - 1][1]) / (t[i][0] - t[i - 1][0]);
                area = t[i - 1][1] + top_width * (ws - t[i - 1][0]);
                break;
            }
        }
        let mut perimeter = last[2];
        for i in 1..t.len() {
            if t[i][0] >= ws {
                let fraction = (ws - t[i - 1][0]) / (t[i][0] - t[i - 1][0]);
                perimeter = t[i - 1][2] * fraction + t[i][2] * (1.0 - fraction);
                break;
            }
        }
        area / perimeter
    }
}

/// 2 x 2 normal-equation accumulator (`FaceVelocityCoef`), binary32.
#[derive(Clone, Copy, Default)]
struct Coef {
    a11: f32,
    a22: f32,
    a12: f32,
    count: usize,
}

impl Coef {
    fn add(&mut self, n: [f32; 2], w: f32) {
        self.a11 += n[0] * n[0] * w;
        self.a22 += n[1] * n[1] * w;
        self.a12 += n[0] * n[1] * w;
        self.count += 1;
    }
    fn complete(&mut self) {
        let determinant = self.a11 * self.a22 - self.a12 * self.a12;
        if determinant == 0.0 {
            // Unreachable for count 0: every arc holds at least one face.
            self.a11 = 1.0 / self.count as f32;
            self.a12 = 0.0;
            self.a22 = 1.0 / self.count as f32;
        } else {
            let inverse = 1.0 / determinant;
            self.a11 *= inverse;
            self.a22 *= inverse;
            self.a12 *= inverse;
        }
    }
    fn solve(&self, b1: f32, b2: f32) -> [f32; 2] {
        [
            self.a22 * b1 - self.a12 * b2,
            (0.0 - self.a12) * b1 + self.a11 * b2,
        ]
    }
}

/// Hydraulically connected arc of `slot` (`GetHydraulicallyConnectedArc`).
fn arc(corner: &Corner, slot: usize, connected: impl Fn(usize) -> bool) -> (usize, usize) {
    let mut end = slot;
    loop {
        end = corner.ccw[end];
        if !(connected(corner.faces[end]) && end != slot) {
            break;
        }
    }
    let mut start = slot;
    if start != end {
        while connected(corner.faces[start]) {
            start = corner.cw[start];
        }
    }
    (start, end)
}

/// Facepoint arc vectors (`ComputeFacePointVelocity_FacePerpLeastSquares_Weighted`).
fn facepoint_vectors(
    corner: &Corner,
    water: &[FaceWater],
    normals: &[FaceNormal],
    inverse_lengths: &[f32],
    velocity: &[f32],
) -> Vec<[f32; 2]> {
    let k = corner.faces.len();
    if corner.faces.iter().all(|&f| water[f].values == [NODATA; 2]) {
        return vec![[0.0, 0.0]; k];
    }
    let connected = |f: usize| water[f].connection.connected();
    (0..k)
        .map(|slot| {
            let (start, end) = arc(corner, slot, connected);
            let mut coef = Coef::default();
            let (mut bx, mut by) = (0.0_f32, 0.0_f32);
            let mut add = |f: usize| {
                let n = normals[f].normal;
                let u = velocity[f];
                let w = inverse_lengths[f];
                bx += n[0] * u * w;
                by += n[1] * u * w;
                coef.add(n, w);
            };
            let mut i = start;
            loop {
                add(corner.faces[i]);
                i = corner.ccw[i];
                if i == end {
                    break;
                }
            }
            let f = corner.faces[end];
            if !connected(f) {
                add(f);
            }
            coef.complete();
            coef.solve(bx, by)
        })
        .collect()
}

/// Per-mesh geometry the face pass needs, prepared once.
pub struct FaceGeometry<'a> {
    pub mesh: &'a Mesh,
    pub normals: &'a [FaceNormal],
    pub tables: &'a FaceTables,
    inverse_lengths: Vec<f32>,
    /// Per face: slot of the face in its (start, end) facepoint rings.
    slots: Vec<[usize; 2]>,
}

impl<'a> FaceGeometry<'a> {
    pub fn new(
        mesh: &'a Mesh,
        normals: &'a [FaceNormal],
        tables: &'a FaceTables,
    ) -> Result<Self, MapError> {
        let faces = mesh.faces.len();
        if normals.len() != faces || tables.mann.len() != faces {
            return Err(MapError::Invalid(
                "shear stress face inputs do not match the mesh".into(),
            ));
        }
        let mut slots = Vec::with_capacity(faces);
        for (f, face) in mesh.faces.iter().enumerate() {
            let slot = |v: usize| {
                mesh.corners[v]
                    .faces
                    .iter()
                    .position(|&g| g == f)
                    .ok_or_else(|| MapError::Invalid("face not incident to its facepoint".into()))
            };
            slots.push([slot(face.vertices[0])?, slot(face.vertices[1])?]);
        }
        Ok(Self {
            mesh,
            normals,
            tables,
            inverse_lengths: normals.iter().map(|n| 1.0_f32 / n.length).collect(),
            slots,
        })
    }

    /// `|tau|` of every face at one output time.
    pub fn face_shear(
        &self,
        wse: &[f32],
        velocity: &[f32],
        factor: f32,
        pool: &rayon::ThreadPool,
    ) -> Result<Vec<f32>, MapError> {
        let mesh = self.mesh;
        if wse.len() != mesh.cells.len() || velocity.len() != mesh.faces.len() {
            return Err(MapError::Invalid("shear stress profile dimensions".into()));
        }
        let water = mesh.face_water(wse, PLOT_TOLERANCE, pool)?;
        pool.install(|| {
            let corners: Vec<Vec<[f32; 2]>> = mesh
                .corners
                .par_iter()
                .map(|c| {
                    facepoint_vectors(c, &water, self.normals, &self.inverse_lengths, velocity)
                })
                .collect();
            Ok(mesh
                .faces
                .par_iter()
                .enumerate()
                .map(|(f, face)| {
                    if !water[f].connection.connected() {
                        return 0.0;
                    }
                    let a = corners[face.vertices[0]][self.slots[f][0]];
                    let b = corners[face.vertices[1]][self.slots[f][1]];
                    let v = [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0];
                    let r =
                        self.tables
                            .hydraulic_radius(f, water[f].values[0], self.normals[f].length);
                    shear_magnitude(v, self.tables.mann[f], r, factor)
                })
                .collect())
        })
    }
}

/// `ComputeFaceTrueShear` then `Vector2D.Magnitude`.
pub fn shear_magnitude(v: [f32; 2], n: f32, r: f32, factor: f32) -> f32 {
    let [x, y] = v;
    if (x == 0.0 && y == 0.0) || r == 0.0 {
        return 0.0;
    }
    let magnitude = f64::from(x * x + y * y).sqrt() as f32;
    let root = f64::from(r).powf(0.333_333_333_333_333_37) as f32;
    let sx = factor * magnitude * x * n * n / root;
    let sy = factor * magnitude * y * n * n / root;
    f64::from(sx * sx + sy * sy).sqrt() as f32
}

/// Native max accumulation: initial 0, replaced only by a strictly larger value.
pub fn accumulate(max: &mut [f32], values: &[f32]) {
    for (m, &v) in max.iter_mut().zip(values) {
        if v > *m {
            *m = v;
        }
    }
}

/// Facepoint scalar: binary32 mean of the incident faces' values in ring order
/// (`AverageFaceScalarsToFacepointValuesSA`).
pub fn facepoint_values(mesh: &Mesh, face_values: &[f32]) -> Vec<f32> {
    mesh.corners
        .iter()
        .map(|c| {
            let mut total = 0.0_f32;
            for &f in &c.faces {
                total += face_values[f];
            }
            total / c.faces.len() as f32
        })
        .collect()
}

fn read_exact_f32(
    reader: &mut impl Read,
    out: &mut [f32],
    bytes: &mut Vec<u8>,
) -> Result<(), MapError> {
    bytes.resize(out.len() * 4, 0);
    reader.read_exact(bytes)?;
    for (v, b) in out.iter_mut().zip(bytes.as_chunks::<4>().0) {
        *v = f32::from_le_bytes(*b);
    }
    if out.iter().any(|v| !v.is_finite()) {
        return Err(MapError::Invalid(
            "shear stress time series must be finite".into(),
        ));
    }
    Ok(())
}

fn expect_len(path: &Path, bytes: u64) -> Result<(), MapError> {
    if fs::metadata(path)?.len() != bytes {
        return Err(MapError::Invalid(format!("{}: length", path.display())));
    }
    Ok(())
}

/// Loads the staged `Faces Area Elevation` table of one mesh.
pub fn read_tables(
    info: &Path,
    values: &Path,
    faces: usize,
    rows: usize,
) -> Result<FaceTables, MapError> {
    expect_len(info, faces as u64 * 8)?;
    expect_len(values, rows as u64 * 16)?;
    let raw = fs::read(info)?;
    let info: Vec<[i32; 2]> = raw
        .as_chunks::<8>()
        .0
        .iter()
        .map(|b| {
            [
                i32::from_le_bytes([b[0], b[1], b[2], b[3]]),
                i32::from_le_bytes([b[4], b[5], b[6], b[7]]),
            ]
        })
        .collect();
    let raw = fs::read(values)?;
    let values: Vec<[f32; 4]> = raw
        .as_chunks::<16>()
        .0
        .iter()
        .map(|b| {
            std::array::from_fn(|i| {
                f32::from_le_bytes([b[4 * i], b[4 * i + 1], b[4 * i + 2], b[4 * i + 3]])
            })
        })
        .collect();
    FaceTables::new(&info, &values)
}

/// Max face shear of one mesh, streaming the staged time series one output
/// time at a time (memory: one cell row and one face row).
pub fn face_max_from_files(
    geometry: &FaceGeometry<'_>,
    water_surface: &Path,
    face_velocity: &Path,
    times: usize,
    factor: f32,
    pool: &rayon::ThreadPool,
) -> Result<Vec<f32>, MapError> {
    let (cells, faces) = (geometry.mesh.cells.len(), geometry.mesh.faces.len());
    if times == 0 {
        return Err(MapError::Invalid(
            "shear stress needs stored output times".into(),
        ));
    }
    expect_len(water_surface, times as u64 * cells as u64 * 4)?;
    expect_len(face_velocity, times as u64 * faces as u64 * 4)?;
    let mut ws_reader = BufReader::with_capacity(1 << 20, fs::File::open(water_surface)?);
    let mut fv_reader = BufReader::with_capacity(1 << 20, fs::File::open(face_velocity)?);
    let (mut wse, mut velocity) = (vec![0.0_f32; cells], vec![0.0_f32; faces]);
    let mut bytes = Vec::new();
    let mut max = vec![0.0_f32; faces];
    for _ in 0..times {
        read_exact_f32(&mut ws_reader, &mut wse, &mut bytes)?;
        read_exact_f32(&mut fv_reader, &mut velocity, &mut bytes)?;
        accumulate(
            &mut max,
            &geometry.face_shear(&wse, &velocity, factor, pool)?,
        );
    }
    Ok(max)
}

/// Shear stress for a rendered window: drawn where `depth > 0` (NaN elsewhere)
/// in cells whose Max WSE passes the wet gate, from each mesh's facepoint
/// values. `meshes[m]` = (mesh, stencils, cell Max WSE, facepoint values).
#[allow(clippy::type_complexity)]
pub fn render(
    meshes: &[(&Mesh, &[Option<CellStencil>], &[f32], &[f32])],
    window: &crate::sloping::RasterWindow<'_>,
    depth: &[f32],
    pool: &rayon::ThreadPool,
    source_shape: [usize; 2],
) -> Result<Vec<f32>, MapError> {
    if depth.len() != window.width * window.height || depth.len() != window.owner.len() {
        return Err(MapError::Internal(
            "shear stress window size mismatch".into(),
        ));
    }
    let lattice =
        match SourceLattice::new(window.source_transform, window.source_tile, source_shape) {
            Ok(lattice) => Some(lattice),
            Err(MapError::Unsupported(_)) => None,
            Err(error) => return Err(error),
        };
    let width = window.width;
    let gt = window.source_transform;
    let col0 = ((window.transform[0] - gt[0]) / gt[1]).round() as usize;
    let row0 = ((window.transform[3] - gt[3]) / gt[5]).round() as usize;
    let mut output = vec![f32::NAN; depth.len()];
    pool.install(|| {
        output.par_chunks_mut(4096).enumerate().try_for_each_init(
            || WeightScratch::new(0),
            |scratch, (chunk, out)| -> Result<(), MapError> {
                let mut local = Vec::new();
                for (j, v) in out.iter_mut().enumerate() {
                    let i = chunk * 4096 + j;
                    let label = window.owner[i] as usize;
                    if label == 0 || depth[i].is_nan() || depth[i] <= 0.0 {
                        continue;
                    }
                    let [m, cell] = window.owner_cells[label - 1];
                    let (mesh, stencils, wse, fp) = meshes[m];
                    if wse[cell] <= mesh.cells[cell].minimum + PLOT_TOLERANCE {
                        continue;
                    }
                    let Some(stencil) = &stencils[cell] else {
                        continue;
                    };
                    local.clear();
                    local.extend(stencil.corner_slots().map(|[vertex, _]| fp[vertex]));
                    let (col, row) = (col0 + i % width, row0 + i / width);
                    let value = if let Some(weights) =
                        lattice.and_then(|l| square_weights(stencil, mesh, l, col, row))
                    {
                        paint_corner_values(&weights[..4], &local)?
                    } else {
                        let point = match lattice {
                            Some(l) => l.point(col, row),
                            None => [
                                window.transform[0]
                                    + ((i % width) as f64 + 0.5) * window.transform[1],
                                window.transform[3]
                                    + ((i / width) as f64 + 0.5) * window.transform[5],
                            ],
                        };
                        paint_corner_values(stencil.plain_weights(point, scratch)?, &local)?
                    };
                    if value != NODATA {
                        *v = value;
                    }
                }
                Ok(())
            },
        )
    })?;
    Ok(output)
}

/// Square cells on the native source-tile lattice: the normalized raster-row
/// stepping of `velocity::square_pixel`, without face donation.
fn square_weights(
    stencil: &CellStencil,
    mesh: &Mesh,
    lattice: SourceLattice,
    col: usize,
    row: usize,
) -> Option<[f32; 4]> {
    let [left, bottom, right, top] = stencil.square_bounds()?;
    let [x, y] = lattice.square_unit(left, bottom, right, top, col, row);
    let mut weights = [0.0_f32; 4];
    for (i, [vertex, _]) in stencil.corner_slots().enumerate() {
        let point = mesh.corners[vertex].point;
        let wx = if point[0] <= (left + right) * 0.5 {
            1.0 - x
        } else {
            x
        };
        let wy = if point[1] <= (bottom + top) * 0.5 {
            1.0 - y
        } else {
            y
        };
        weights[i] = (wx * wy) as f32;
    }
    Some(weights)
}

/// Max WSE horizontal clipping depth, as velocity's horizontal depth.
pub fn clip_depth<'a>(
    depth: VelocityDepth,
    rendered: &'a [f32],
    window: &crate::sloping::RasterWindow<'_>,
    meshes: &[Mesh],
    cell_values: &[Vec<f32>],
    pool: &rayon::ThreadPool,
) -> Result<std::borrow::Cow<'a, [f32]>, MapError> {
    Ok(match depth {
        VelocityDepth::Rendered => std::borrow::Cow::Borrowed(rendered),
        VelocityDepth::Horizontal => std::borrow::Cow::Owned(
            crate::velocity::horizontal_depth_in_pool(window, meshes, cell_values, pool)?,
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hydraulic_radius_interpolates_like_native() {
        let tables = FaceTables::new(
            &[[0, 3]],
            &[
                [10.0, 0.0, 0.0, 0.05],
                [11.0, 4.0, 5.0, 0.06],
                [13.0, 12.0, 9.0, 0.07],
            ],
        )
        .unwrap();
        assert_eq!(tables.mann, vec![0.05]);
        // ws 12: area 4 + 4 * 1 = 8; perimeter 5 * 0.5 + 9 * 0.5 = 7.
        assert_eq!(tables.hydraulic_radius(0, 12.0, 20.0), 8.0 / 7.0);
        // Native perimeter weights are reversed: ws 11.5 -> t = 0.25.
        let p = 5.0_f32 * 0.25 + 9.0 * 0.75;
        let a = 4.0_f32 + 4.0 * 0.5;
        assert_eq!(tables.hydraulic_radius(0, 11.5, 20.0), a / p);
        // Above the table: last area + length * excess, last perimeter.
        assert_eq!(tables.hydraulic_radius(0, 14.0, 20.0), (12.0 + 20.0) / 9.0);
    }

    #[test]
    fn empty_tables_are_refused() {
        assert!(FaceTables::new(&[[0, 0]], &[[1.0, 0.0, 0.0, 0.05]]).is_err());
        assert!(FaceTables::new(&[[0, 2]], &[[1.0, 0.0, 0.0, 0.05]]).is_err());
        assert!(FaceTables::new(&[[0, 1]], &[[f32::NAN, 0.0, 0.0, 0.05]]).is_err());
    }

    #[test]
    fn shear_formula_and_zero_cases() {
        assert_eq!(shear_magnitude([0.0, 0.0], 0.05, 1.0, FACTOR_US), 0.0);
        assert_eq!(shear_magnitude([1.0, 0.0], 0.05, 0.0, FACTOR_US), 0.0);
        // R = 1: tau = k * v^2 * n^2.
        let t = shear_magnitude([2.0, 0.0], 0.05, 1.0, FACTOR_US);
        assert_eq!(t, FACTOR_US * 2.0 * 2.0 * 0.05 * 0.05);
        let t = shear_magnitude([0.0, -3.0], 0.04, 8.0, FACTOR_SI);
        let root = 8.0_f64.powf(0.333_333_333_333_333_37) as f32;
        assert_eq!(t, (FACTOR_SI * 3.0 * -3.0 * 0.04 * 0.04 / root).abs());
        assert_eq!(factor("US Customary").unwrap(), FACTOR_US);
        assert_eq!(factor("SI Units").unwrap(), FACTOR_SI);
        assert!(factor("Imperial").is_err());
    }

    #[test]
    fn max_is_strict_and_starts_at_zero() {
        let mut max = vec![0.0; 3];
        accumulate(&mut max, &[1.0, f32::NAN, 0.0]);
        accumulate(&mut max, &[0.5, 2.0, -1.0]);
        assert_eq!(max, vec![1.0, 2.0, 0.0]);
    }

    fn wet(values: [f32; 2], connection: crate::sloping::Connection) -> FaceWater {
        FaceWater { values, connection }
    }

    #[test]
    fn facepoint_least_squares_recovers_uniform_flow_and_zeroes_dry_points() {
        use crate::sloping::Connection;
        // Three faces around one facepoint, all connected: a uniform field is exact.
        let corner = Corner {
            point: [0.0, 0.0],
            faces: vec![0, 1, 2],
            ccw: vec![1, 2, 0],
            cw: vec![2, 0, 1],
        };
        let normals: Vec<FaceNormal> = [[1.0, 0.0], [0.0, 1.0], [-0.6, 0.8]]
            .into_iter()
            .map(|normal| FaceNormal {
                normal,
                length: 2.0,
            })
            .collect();
        let field = [2.0_f32, -1.0];
        let velocity: Vec<f32> = normals
            .iter()
            .map(|n| n.normal[0] * field[0] + n.normal[1] * field[1])
            .collect();
        let inverse = vec![0.5_f32; 3];
        let water = vec![wet([5.0, 5.0], Connection::DownhillDeep); 3];
        let vectors = facepoint_vectors(&corner, &water, &normals, &inverse, &velocity);
        assert_eq!(vectors.len(), 3);
        for v in &vectors {
            assert!(
                (v[0] - field[0]).abs() < 1e-5 && (v[1] - field[1]).abs() < 1e-5,
                "{v:?}"
            );
        }
        // Every incident face dry on both sides: zero vectors.
        let dry = vec![wet([NODATA; 2], Connection::None); 3];
        assert_eq!(
            facepoint_vectors(&corner, &dry, &normals, &inverse, &velocity),
            vec![[0.0, 0.0]; 3]
        );
        // A levee face splits the ring: each arc fits its own faces plus the
        // unconnected end face.
        let mut split = water.clone();
        split[1] = wet([5.0, 4.0], Connection::Levee);
        let vectors = facepoint_vectors(&corner, &split, &normals, &inverse, &velocity);
        assert!(vectors.iter().all(|v| v.iter().all(|c| c.is_finite())));
    }

    #[test]
    fn facepoint_values_average_incident_faces_in_ring_order() {
        let mesh = Mesh {
            cells: vec![],
            faces: vec![],
            corners: vec![
                Corner {
                    point: [0.0, 0.0],
                    faces: vec![0, 1, 2],
                    ccw: vec![1, 2, 0],
                    cw: vec![2, 0, 1],
                },
                Corner {
                    point: [1.0, 0.0],
                    faces: vec![2],
                    ccw: vec![0],
                    cw: vec![0],
                },
            ],
        };
        let values = facepoint_values(&mesh, &[0.1, 0.2, 0.7]);
        assert_eq!(values, vec![(0.1_f32 + 0.2 + 0.7) / 3.0, 0.7]);
    }

    #[test]
    fn coefficient_solve_recovers_vector() {
        // Two orthogonal unit normals carrying the components of (3, -2).
        let mut coef = Coef::default();
        coef.add([1.0, 0.0], 0.5);
        coef.add([0.0, 1.0], 0.5);
        coef.complete();
        assert_eq!(coef.solve(3.0 * 0.5, -2.0 * 0.5), [3.0, -2.0]);
        // Collinear normals fall back to 1 / count.
        let mut coef = Coef::default();
        coef.add([1.0, 0.0], 1.0);
        coef.add([-1.0, 0.0], 1.0);
        coef.complete();
        assert_eq!(coef.a11, 0.5);
    }
}
