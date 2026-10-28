//! Research kernel: RAS Mapper velocity reconstruction (`rasmapper-sloping-velocity/0`).
//!
//! **Not wired into any mapping request.** It reproduces the native structure
//! of RAS Mapper's stored-map velocity so that the remaining differences can be
//! closed before a product is exposed:
//!
//! 1. face classification: the sloping face water surfaces already used by the
//!    WSE renderer (`Mesh::reconstruct`); "connected" excludes `None`/`Levee`;
//! 2. face vectors, per cell side: the face's normal velocity plus a tangential
//!    component from an unweighted least-squares fit of the face and its two
//!    ring neighbours in that cell (only when a neighbour is connected); a
//!    connected face takes the mean of its two sides;
//! 3. corner vectors, per vertex and hydraulically connected arc: the
//!    inverse-length weighted mean of the arc's face vectors (zero when every
//!    incident face is dry on both sides);
//! 4. pixel: the cell-face sloping donated weights applied to the corner and
//!    face vectors in binary32, magnitude `sqrt(x*x + y*y)`.
//!
//! Max velocity uses `Maximum Face Velocity` and maximum WSE as one profile.
//! Velocity does not depend on the WSE render mode. [`interior`] is the
//! candidate qualification scope; measurements are in the PR.
use crate::{
    MapError,
    sloping::{CellStencil, Corner, FaceWater, Mesh, WeightScratch},
};

pub const METHOD_ID: &str = "rasmapper-sloping-velocity/0";

pub fn method_record() -> serde_json::Value {
    serde_json::json!({
        "method_id": METHOD_ID,
        "product": "velocity magnitude (ft/s or m/s), RAS Mapper stored-map rule, independent of the WSE render mode",
        "inputs": "per mesh: HDF face normals with polyline face lengths; per profile: face-normal velocities (Max: summary Maximum Face Velocity) and the cell WSE already supplied",
        "faces": "normal velocity plus a tangential component from an unweighted least-squares fit of the face and its two ring neighbours in each cell (when a neighbour is hydraulically connected); connected faces take the mean of both sides",
        "corners": "inverse polyline-length weighted mean of the face vectors over each hydraulically connected arc; zero where every incident face is dry",
        "pixels": "cell-face sloping donated weights in binary32, native donation history replayed in source-tile order; drawn where rendered depth > 0",
        "qualified": "native HEC-RAS 7.0.1: BaldEagleCrkMulti2D bridges p05 (Max, one output time), dam-break p07 2D cells (Max, one output time), Spring Creek eBFE AOI (one output time) bit for bit; Davis p02 Max all but 5 of 20,560 pixels (within 2 ulp)",
        "not_rendered": "storage areas, 1D; derived products (D x V, D x V^2, Froude, shear) are not rendered here"
    })
}

/// Face geometry the native velocity stencils need: HDF `Faces NormalUnitVector
/// and Length` (unit normal from cell A to cell B, and face length).
#[derive(Clone, Copy, Debug)]
pub struct FaceNormal {
    pub normal: [f32; 2],
    pub length: f32,
}

/// 2 x 2 normal-equation accumulator, solved by its scaled inverse (binary32).
#[derive(Clone, Copy, Default)]
struct Coef {
    a11: f32,
    a12: f32,
    a22: f32,
}

impl Coef {
    fn add(&mut self, n: [f32; 2], w: f32) {
        self.a11 += n[0] * n[0] * w;
        self.a12 += n[0] * n[1] * w;
        self.a22 += n[1] * n[1] * w;
    }
    /// Scaled inverse through one binary32 reciprocal of the determinant.
    fn complete(&mut self) {
        let inverse = 1.0 / (self.a11 * self.a22 - self.a12 * self.a12);
        self.a11 *= inverse;
        self.a12 *= inverse;
        self.a22 *= inverse;
    }
    fn solve(&self, bx: f32, by: f32) -> [f32; 2] {
        [
            self.a22 * bx - self.a12 * by,
            -self.a12 * bx + self.a11 * by,
        ]
    }
}

/// Reconstructed node vectors of one profile.
pub struct Nodes {
    /// Per face: vector on the cell-A side and on the cell-B side.
    pub faces: Vec<[[f32; 2]; 2]>,
    /// Per vertex and incident-face slot: the hydraulically connected arc vector.
    pub corners: Vec<Vec<[f32; 2]>>,
}

/// Hydraulically connected arc of `slot` around a corner: (start, end) slots.
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

/// Face vectors (C stencil) and corner vectors (inverse-length weighted least
/// squares over each hydraulically connected arc), from face-normal velocities.
/// `cell_faces` lists each cell's faces in ring order (as the stencils do).
pub fn reconstruct(
    mesh: &Mesh,
    cell_faces: &[Vec<usize>],
    water: &[FaceWater],
    normals: &[FaceNormal],
    velocity: &[f32],
) -> Result<Nodes, MapError> {
    let nf = mesh.faces.len();
    if water.len() != nf
        || normals.len() != nf
        || velocity.len() != nf
        || cell_faces.len() != mesh.cells.len()
    {
        return Err(MapError::Invalid("velocity input dimensions".into()));
    }
    let connected = |f: usize| water[f].connection.connected();
    // Neighbouring faces of `face` within `cell`: (clockwise, counter-clockwise).
    let neighbours = |cell: usize, face: usize| -> Option<(usize, usize)> {
        let ring = &cell_faces[cell];
        let i = ring.iter().position(|&f| f == face)?;
        let n = ring.len();
        // Rings are counter-clockwise: the next face is the counter-clockwise one.
        Some((ring[(i + n - 1) % n], ring[(i + 1) % n]))
    };
    let side = |cell: usize, f: usize| -> [f32; 2] {
        let n = normals[f].normal;
        let u = velocity[f];
        let Some((cw, ccw)) = neighbours(cell, f) else {
            return [u * n[0], u * n[1]];
        };
        let mut coef = Coef::default();
        coef.add(normals[cw].normal, 1.0);
        coef.add(normals[ccw].normal, 1.0);
        coef.add(n, 1.0);
        coef.complete();
        let t = [n[1], -n[0]];
        let mut along = 0.0_f32;
        if connected(cw) || connected(ccw) {
            let mut bx = normals[cw].normal[0] * velocity[cw];
            let mut by = normals[cw].normal[1] * velocity[cw];
            bx += normals[ccw].normal[0] * velocity[ccw];
            by += normals[ccw].normal[1] * velocity[ccw];
            bx += n[0] * u;
            by += n[1] * u;
            let v = coef.solve(bx, by);
            along = v[0] * t[0] + v[1] * t[1];
        }
        [u * n[0] + along * t[0], u * n[1] + along * t[1]]
    };
    let faces: Vec<[[f32; 2]; 2]> = mesh
        .faces
        .iter()
        .enumerate()
        .map(|(f, face)| {
            let a = side(face.cells[0], f);
            let b = side(face.cells[1], f);
            if connected(f) {
                let m = [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0];
                [m, m]
            } else {
                [a, b]
            }
        })
        .collect();
    let inverse_length = |f: usize| 1.0_f32 / normals[f].length;
    let corners = mesh
        .corners
        .iter()
        .enumerate()
        .map(|(vertex, corner)| {
            let k = corner.faces.len();
            if corner
                .faces
                .iter()
                .all(|&f| water[f].values == [crate::sloping::NODATA; 2])
            {
                return vec![[0.0, 0.0]; k];
            }
            // Side of face f that starts (true) or ends at this vertex.
            let at_a = |f: usize| mesh.faces[f].vertices[0] == vertex;
            (0..k)
                .map(|slot| {
                    let (start, end) = arc(corner, slot, connected);
                    let (mut x, mut y, mut total) = (0.0_f32, 0.0_f32, 0.0_f32);
                    let mut i = start;
                    loop {
                        let f = corner.faces[i];
                        let w = inverse_length(f);
                        let v = if i == start && !at_a(f) {
                            faces[f][1]
                        } else {
                            faces[f][0]
                        };
                        x += v[0] * w;
                        y += v[1] * w;
                        total += w;
                        i = corner.ccw[i];
                        if i == end {
                            break;
                        }
                    }
                    let f = corner.faces[end];
                    if !connected(f) {
                        let w = inverse_length(f);
                        let v = if at_a(f) { faces[f][1] } else { faces[f][0] };
                        x += v[0] * w;
                        y += v[1] * w;
                        total += w;
                    }
                    [x / total, y / total]
                })
                .collect()
        })
        .collect();
    Ok(Nodes { faces, corners })
}

/// Velocity magnitude at `point` inside `stencil`'s cell `cell`.
pub fn pixel(
    stencil: &CellStencil,
    cell: usize,
    mesh: &Mesh,
    nodes: &Nodes,
    point: [f64; 2],
    scratch: &mut WeightScratch,
) -> Result<f32, MapError> {
    let weights = stencil.donated_weights(point, scratch)?;
    Ok(paint(stencil, cell, mesh, nodes, &weights))
}

fn paint(stencil: &CellStencil, cell: usize, mesh: &Mesh, nodes: &Nodes, weights: &[f32]) -> f32 {
    let n = weights.len() / 2;
    let (mut x, mut y) = (0.0_f32, 0.0_f32);
    for (w, [vertex, slot]) in weights[..n].iter().zip(stencil.corner_slots()) {
        let v = nodes.corners[vertex][slot];
        x += v[0] * w;
        y += v[1] * w;
    }
    for (w, f) in weights[n..].iter().zip(stencil.face_ids()) {
        let v = nodes.faces[f][usize::from(mesh.faces[f].cells[0] != cell)];
        x += v[0] * w;
        y += v[1] * w;
    }
    (f64::from(x * x + y * y)).sqrt() as f32
}

/// Native donation history: RAS Mapper computes a cell's donated weights in
/// source-tile raster order, keeping the clockwise/counter-clockwise donation
/// scratch between points and clearing it every 256 points. Replays one cell
/// whose clipped weights expose that state; returns (window index, velocity).
#[allow(clippy::too_many_arguments)]
pub(crate) fn replay_cell(
    stencil: &CellStencil,
    cell: usize,
    mesh: &Mesh,
    nodes: &Nodes,
    source_transform: [f64; 6],
    tile: [usize; 2],
    window: [usize; 4],
) -> Result<Vec<(usize, f32)>, MapError> {
    let ring = &stencil.physical_ring;
    if ring.is_empty() {
        return Err(MapError::Unsupported(
            "velocity donation replay needs the cell perimeter".into(),
        ));
    }
    let gt = source_transform;
    let [tw, th] = tile;
    let [col, row, width, height] = window;
    let fold = |axis: usize, f: fn(f64, f64) -> f64, start: f64| {
        ring.iter().map(|p| p[axis]).fold(start, f)
    };
    let (minx, maxx) = (
        fold(0, f64::min, f64::INFINITY),
        fold(0, f64::max, f64::NEG_INFINITY),
    );
    let (miny, maxy) = (
        fold(1, f64::min, f64::INFINITY),
        fold(1, f64::max, f64::NEG_INFINITY),
    );
    let c0 = (((minx - gt[0]) / gt[1]).floor() as i64)
        .max(col as i64)
        .max(0) as usize;
    let c1 = (((maxx - gt[0]) / gt[1]).ceil() as i64)
        .min((col + width) as i64)
        .max(0) as usize;
    let r0 = (((maxy - gt[3]) / gt[5]).floor() as i64)
        .max(row as i64)
        .max(0) as usize;
    let r1 = (((miny - gt[3]) / gt[5]).ceil() as i64)
        .min((row + height) as i64)
        .max(0) as usize;
    let mut output = Vec::new();
    if c0 >= c1 || r0 >= r1 {
        return Ok(output);
    }
    for ty in r0 / th..=(r1 - 1) / th {
        for tx in c0 / tw..=(c1 - 1) / tw {
            let tile_gt = [
                gt[0] + (tx * tw) as f64 * gt[1],
                gt[1],
                0.,
                gt[3] + (ty * th) as f64 * gt[5],
                0.,
                gt[5],
            ];
            let owned = crate::ownership::burn_ring(
                ring,
                terrio::ops::rasterize::Grid {
                    geo_transform: tile_gt,
                    cols: tw,
                    rows: th,
                    crs: Default::default(),
                },
            )?;
            let mut scratch = WeightScratch::new(stencil.corner_slots().count());
            let mut native_index = 0;
            for (i, &inside) in owned.iter().enumerate() {
                if inside == 0 {
                    continue;
                }
                if native_index % 256 == 0 {
                    scratch.clear_donation_history();
                }
                native_index += 1;
                let point = [
                    tile_gt[0] + (i % tw) as f64 * gt[1] + 0.5 * gt[1],
                    tile_gt[3] + (i / tw) as f64 * gt[5] + 0.5 * gt[5],
                ];
                let weights = stencil.donated_weights_with_history(point, &mut scratch, true)?;
                let (c, r) = (tx * tw + i % tw, ty * th + i / tw);
                if (col..col + width).contains(&c) && (row..row + height).contains(&r) {
                    output.push((
                        (r - row) * width + c - col,
                        paint(stencil, cell, mesh, nodes, &weights),
                    ));
                }
            }
        }
    }
    Ok(output)
}

/// Every face incident to the cell's corners, and to those faces' far
/// vertices, is hydraulically connected (so no perimeter or dry face).
pub(crate) fn interior(
    mesh: &Mesh,
    water: &[FaceWater],
    stencil: &CellStencil,
    structure_vertices: &[bool],
) -> bool {
    let mut vertices: Vec<usize> = stencil.corner_slots().map(|[v, _]| v).collect();
    let first: Vec<usize> = vertices
        .iter()
        .flat_map(|&v| mesh.corners[v].faces.iter().copied())
        .collect();
    for &f in &first {
        vertices.extend(mesh.faces[f].vertices);
    }
    if vertices
        .iter()
        .any(|&v| structure_vertices.get(v).copied().unwrap_or(false))
    {
        return false;
    }
    vertices
        .iter()
        .flat_map(|&v| mesh.corners[v].faces.iter())
        .all(|&f| water[f].connection.connected())
}

/// Reads HDF-ordered float32 (x, y, length) face normals; `length` must be the
/// face polyline length (RAS Mapper's inverse-length weights use it).
pub fn read_normals(path: &std::path::Path, faces: usize) -> Result<Vec<FaceNormal>, MapError> {
    let raw = read_f32(path, faces * 3)?;
    let normals: Vec<FaceNormal> = raw
        .chunks(3)
        .map(|p| FaceNormal {
            normal: [p[0], p[1]],
            length: p[2],
        })
        .collect();
    if normals.iter().any(|n| {
        !n.normal.iter().all(|v| v.is_finite())
            || n.length.is_nan()
            || n.length <= 0.0
            || !n.length.is_finite()
    }) {
        return Err(MapError::Invalid(
            "face normals must be finite with positive lengths".into(),
        ));
    }
    Ok(normals)
}

/// Reads one profile's face-normal velocities (float32, one per face, finite).
pub fn read_face_velocity(path: &std::path::Path, faces: usize) -> Result<Vec<f32>, MapError> {
    let values = read_f32(path, faces)?;
    if values.iter().any(|v| !v.is_finite()) {
        return Err(MapError::Invalid("face velocities must be finite".into()));
    }
    Ok(values)
}

/// Velocity magnitude for a rendered window: drawn where `depth > 0` (NaN
/// elsewhere), from each mesh's cell WSE and face-normal velocities. Cells whose
/// clipped weights expose native donation history are replayed in source-tile
/// order. `meshes[m]` = (mesh, stencils, cell WSE, face normals, face velocity).
#[allow(clippy::type_complexity)]
pub fn render(
    meshes: &[(&Mesh, &[Option<CellStencil>], &[f32], &[FaceNormal], &[f32])],
    window: &crate::sloping::RasterWindow<'_>,
    depth: &[f32],
    pool: &rayon::ThreadPool,
) -> Result<Vec<f32>, MapError> {
    use rayon::prelude::*;
    let mut nodes = Vec::with_capacity(meshes.len());
    for &(mesh, stencils, wse, normals, velocity) in meshes {
        let water = mesh.reconstruct(wse, 0.001, pool)?.faces;
        let rings: Vec<Vec<usize>> = stencils
            .iter()
            .map(|s| s.as_ref().map_or_else(Vec::new, |s| s.face_ids().collect()))
            .collect();
        nodes.push(reconstruct(mesh, &rings, &water, normals, velocity)?);
    }
    if depth.len() != window.width * window.height {
        return Err(MapError::Internal("velocity window size mismatch".into()));
    }
    let width = window.width;
    let flagged: Vec<std::sync::atomic::AtomicBool> = (0..window.owner_cells.len())
        .map(|_| std::sync::atomic::AtomicBool::new(false))
        .collect();
    let mut output = vec![f32::NAN; depth.len()];
    pool.install(|| {
        output.par_chunks_mut(4096).enumerate().try_for_each_init(
            || WeightScratch::new(0),
            |scratch, (chunk, out)| -> Result<(), MapError> {
                for (j, v) in out.iter_mut().enumerate() {
                    let i = chunk * 4096 + j;
                    let label = window.owner[i] as usize;
                    if label == 0 || depth[i].is_nan() || depth[i] <= 0.0 {
                        continue;
                    }
                    let [m, cell] = window.owner_cells[label - 1];
                    let Some(stencil) = &meshes[m].1[cell] else {
                        continue;
                    };
                    let point = [
                        window.transform[0] + ((i % width) as f64 + 0.5) * window.transform[1],
                        window.transform[3] + ((i / width) as f64 + 0.5) * window.transform[5],
                    ];
                    *v = pixel(stencil, cell, meshes[m].0, &nodes[m], point, scratch)?;
                    if stencil.history_sensitive(scratch) {
                        flagged[label - 1].store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                }
                Ok(())
            },
        )
    })?;
    let gt = window.source_transform;
    let col = ((window.transform[0] - gt[0]) / gt[1]).round() as usize;
    let row = ((window.transform[3] - gt[3]) / gt[5]).round() as usize;
    for (label, flag) in flagged.iter().enumerate() {
        if !flag.load(std::sync::atomic::Ordering::Relaxed) {
            continue;
        }
        let [m, cell] = window.owner_cells[label];
        let stencil = meshes[m].1[cell].as_ref().unwrap();
        for (i, value) in replay_cell(
            stencil,
            cell,
            meshes[m].0,
            &nodes[m],
            gt,
            window.source_tile,
            [col, row, window.width, window.height],
        )? {
            if window.owner[i] as usize == label + 1 && depth[i] > 0.0 {
                output[i] = value;
            }
        }
    }
    Ok(output)
}

/// Derived products from a pixel's binary32 depth and velocity at one output
/// time. D x V and D x V^2 reproduce native bits given native depth and
/// velocity; Froude is within one binary32 ulp (native uses unrounded inputs).
pub fn depth_x_velocity(depth: f32, velocity: f32) -> f32 {
    depth * velocity
}
pub fn depth_x_velocity_squared(depth: f32, velocity: f32) -> f32 {
    depth * velocity * velocity
}
pub fn froude(depth: f32, velocity: f32) -> f32 {
    (f64::from(velocity) / (f64::from(32.174_f32) * f64::from(depth)).sqrt()) as f32
}

fn read_f32(path: &std::path::Path, count: usize) -> Result<Vec<f32>, MapError> {
    let raw = std::fs::read(path)?;
    if raw.len() != count * 4 {
        return Err(MapError::Invalid(format!("{}: length", path.display())));
    }
    Ok(raw
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect())
}

/// Research probe: renders velocity magnitude for every owned pixel of wet
/// cells of a staged single-mesh sloping request (NaN elsewhere) and writes it
/// as little-endian float32 in window order. `inputs` holds HDF-ordered float32
/// per mesh `m`: `normals-m.f32` (x, y, length), `fv-m.f32` and `ws-m.f32`.
#[doc(hidden)]
pub fn probe(
    request: &std::path::Path,
    inputs: &std::path::Path,
    out: &std::path::Path,
) -> Result<(), MapError> {
    use crate::model;
    let req: crate::sloping_session::Request = model::read_json(request)?;
    let base = request
        .parent()
        .ok_or_else(|| MapError::Invalid("request parent".into()))?;
    let pool = rayon::ThreadPoolBuilder::new()
        .build()
        .map_err(|e| MapError::Internal(e.to_string()))?;
    let mut meshes = Vec::new();
    for (m, file) in req.mesh_files.iter().enumerate() {
        let geometry = crate::sloping_binary::load(&model::resolve(base, file))?;
        let cell_faces: Vec<Vec<usize>> = geometry.cells.iter().map(|c| c.faces.clone()).collect();
        let (mesh, stencils) = geometry.prepare()?;
        let (nc, nf) = (mesh.cells.len(), mesh.faces.len());
        let raw = read_f32(&inputs.join(format!("normals-{m}.f32")), nf * 3)?;
        let normals: Vec<FaceNormal> = raw
            .chunks(3)
            .map(|p| FaceNormal {
                normal: [p[0], p[1]],
                length: p[2],
            })
            .collect();
        let velocity = read_f32(&inputs.join(format!("fv-{m}.f32")), nf)?;
        let wse = read_f32(&inputs.join(format!("ws-{m}.f32")), nc)?;
        let wet: Vec<bool> = mesh
            .cells
            .iter()
            .zip(&wse)
            .map(|(c, &w)| !c.virtual_cell && w > c.minimum + 0.001_f32)
            .collect();
        let water = mesh.reconstruct(&wse, 0.001, &pool)?.faces;
        let nodes = reconstruct(&mesh, &cell_faces, &water, &normals, &velocity)?;
        let mut structure = vec![false; mesh.corners.len()];
        let path = inputs.join(format!("structure-vertices-{m}.u32"));
        if path.exists() {
            for v in std::fs::read(&path)?.chunks(4) {
                let v = u32::from_le_bytes(v.try_into().unwrap()) as usize;
                if let Some(x) = structure.get_mut(v) {
                    *x = true;
                }
            }
        }
        meshes.push((mesh, stencils, nodes, wet, water, structure));
    }
    let [col, row, width, height] = req.window;
    let gt = req.terrain_transform;
    let transform = [
        gt[0] + col as f64 * gt[1],
        gt[1],
        0.0,
        gt[3] + row as f64 * gt[5],
        0.0,
        gt[5],
    ];
    let grid = terrio::ops::rasterize::Grid {
        geo_transform: transform,
        cols: width,
        rows: height,
        crs: Default::default(),
    };
    let layer = crate::ownership::load(
        &model::resolve(base, &req.geometry_file),
        req.owner_cells.len(),
        false,
    )?;
    let owner = crate::ownership::burn(layer, grid)?;
    let mut output = vec![f32::NAN; width * height];
    let mut sensitive = vec![0.0_f32; width * height];
    let mut replay = std::collections::BTreeSet::new();
    let mut scratch = WeightScratch::new(0);
    for (i, &label) in owner.iter().enumerate() {
        if label == 0 {
            continue;
        }
        let [m, cell] = req.owner_cells[label as usize - 1];
        let (mesh, stencils, nodes, wet, water, structure) = &meshes[m];
        if !wet[cell] {
            continue;
        }
        let Some(stencil) = &stencils[cell] else {
            continue;
        };
        let point = [
            transform[0] + ((i % width) as f64 + 0.5) * transform[1],
            transform[3] + ((i / width) as f64 + 0.5) * transform[5],
        ];
        output[i] = pixel(stencil, cell, mesh, nodes, point, &mut scratch)?;
        if stencil.history_sensitive(&scratch) {
            replay.insert((m, cell));
        }
        sensitive[i] = f32::from(u8::from(stencil.history_sensitive(&scratch)))
            + 2.0 * f32::from(u8::from(interior(mesh, water, stencil, structure)));
    }
    for &(m, cell) in &replay {
        let (mesh, stencils, nodes, _, _, _) = &meshes[m];
        let stencil = stencils[cell].as_ref().unwrap();
        for (i, value) in replay_cell(stencil, cell, mesh, nodes, gt, [256, 256], req.window)? {
            if owner[i] != 0 && req.owner_cells[owner[i] as usize - 1] == [m, cell] {
                output[i] = value;
            }
        }
    }
    crate::horizontal::write_values(&out.with_extension("flag"), &sensitive)?;
    let labels: Vec<f32> = owner.iter().map(|&l| l as f32).collect();
    crate::horizontal::write_values(&out.with_extension("owner"), &labels)?;
    crate::horizontal::write_values(out, &output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coefficient_inverse_solves_two_normals() {
        let mut c = Coef::default();
        c.add([1.0, 0.0], 1.0);
        c.add([0.0, 1.0], 1.0);
        c.complete();
        assert_eq!(c.solve(3.0, 4.0), [3.0, 4.0]);
        let mut c = Coef::default();
        c.add([1.0, 0.0], 0.5);
        c.add([0.0, 1.0], 2.0);
        c.complete();
        assert_eq!(c.solve(1.5, 8.0), [3.0, 4.0]);
    }

    #[test]
    fn arc_walks_connected_faces() {
        // Four faces around a corner; only face 1 is connected.
        let corner = Corner {
            point: [0.0, 0.0],
            faces: vec![10, 11, 12, 13],
            ccw: vec![1, 2, 3, 0],
            cw: vec![3, 0, 1, 2],
        };
        let connected = |f: usize| f == 11;
        assert_eq!(arc(&corner, 0, connected), (0, 2));
        assert_eq!(arc(&corner, 1, connected), (0, 2));
        assert_eq!(arc(&corner, 2, connected), (2, 3));
        let all = |_f: usize| true;
        assert_eq!(arc(&corner, 0, all), (0, 0));
    }

    #[test]
    fn derived_arithmetic_order() {
        let (d, v) = (2.5_f32, 1.3_f32);
        assert_eq!(depth_x_velocity(d, v).to_bits(), (d * v).to_bits());
        assert_eq!(
            depth_x_velocity_squared(d, v).to_bits(),
            ((d * v) * v).to_bits()
        );
        assert_eq!(froude(1.0, 0.0), 0.0);
    }
}
