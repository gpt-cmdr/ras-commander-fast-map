//! Opt-in bounded corner fits (`rasmapper-plain-sloping-bounded-corners/1`) on
//! the staged-cells synthetic quad mesh: the default path keeps its pre-change
//! bits, the bound changes only flagged corners, requests are refused before
//! any read, and native and bounded requests never share a reconstruction.
use ras_commander_fast_map::{
    depth_groups::{self, Request as GroupRequest},
    sloping_session::{self, Request},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

const NX: usize = 10;
const NY: usize = 8;
const SIZE: f64 = 4.0;

/// A quad mesh in HEC-RAS binary layout: physical cells first, one ghost cell
/// per perimeter face after them; faces are directed with cells[0] on the left.
struct Mesh {
    vertices: Vec<[f64; 2]>,
    centers: Vec<[f64; 2]>,
    cell_faces: Vec<Vec<usize>>,
    face_cells: Vec<[usize; 2]>,
    face_vertices: Vec<[usize; 2]>,
    face_points: Vec<Vec<[f64; 2]>>,
    vertex_faces: Vec<Vec<usize>>,
}

/// The vertex whose ring is bowed (see `mesh`), interior and unjittered.
const SPIKE_I: usize = 3;
const SPIKE_J: usize = 3;

fn v(i: usize, j: usize) -> usize {
    j * (NX + 1) + i
}
fn c(i: usize, j: usize) -> usize {
    j * NX + i
}

#[allow(clippy::needless_range_loop)] // Grid indices are the clearest fixture.
fn mesh() -> Mesh {
    let mut vertices = Vec::new();
    for j in 0..=NY {
        for i in 0..=NX {
            let interior = i > 0 && i < NX && j > 0 && j < NY;
            // Jitter the eastern half so its cells use general polygon weights.
            let (dx, dy) = if interior && i >= 5 {
                (
                    0.3 * (i as f64 * 1.7 + j as f64 * 0.9).sin(),
                    0.25 * (i as f64 * 0.6 - j as f64 * 1.3).cos(),
                )
            } else {
                (0.0, 0.0)
            };
            vertices.push([i as f64 * SIZE + dx, j as f64 * SIZE + dy]);
        }
    }
    let mut centers: Vec<[f64; 2]> = (0..NY)
        .flat_map(|j| (0..NX).map(move |i| (i, j)))
        .map(|(i, j)| {
            let p = [v(i, j), v(i + 1, j), v(i + 1, j + 1), v(i, j + 1)].map(|k| vertices[k]);
            [
                p.iter().map(|q| q[0]).sum::<f64>() / 4.0,
                p.iter().map(|q| q[1]).sum::<f64>() / 4.0,
            ]
        })
        .collect();
    let mut cell_faces = vec![Vec::new(); NX * NY];
    let mut face_cells = Vec::new();
    let mut face_vertices = Vec::new();
    let mut face_points = Vec::new();
    let mut horizontal = vec![vec![0; NX]; NY + 1];
    let mut vertical = vec![vec![0; NY]; NX + 1];
    let mut add = |cells: [Option<usize>; 2], ends: [usize; 2], interior: Vec<[f64; 2]>| {
        let id = face_cells.len();
        let left = cells[0].unwrap();
        let right = cells[1].unwrap_or_else(|| {
            // Ghost centre mirrored across the face midpoint.
            let m = [
                (vertices[ends[0]][0] + vertices[ends[1]][0]) * 0.5,
                (vertices[ends[0]][1] + vertices[ends[1]][1]) * 0.5,
            ];
            centers.push([2.0 * m[0] - centers[left][0], 2.0 * m[1] - centers[left][1]]);
            cell_faces.push(vec![id]);
            centers.len() - 1
        });
        face_cells.push([left, right]);
        face_vertices.push(ends);
        face_points.push(interior);
        id
    };
    for j in 0..=NY {
        for i in 0..NX {
            let (p, q) = (v(i, j), v(i + 1, j));
            // Around vertex SPIKE_VERTEX, the east and west faces bow north so
            // that their application points and the north face's lie nearly on
            // one line the vertex is off: a corner fit there extrapolates.
            let bow = if j == SPIKE_J && (i == SPIKE_I || i + 1 == SPIKE_I) {
                let (a, b) = (vertices[p], vertices[q]);
                vec![[(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5 + 0.4 * SIZE]]
            } else {
                vec![]
            };
            horizontal[j][i] = if j == 0 {
                add([Some(c(i, 0)), None], [p, q], vec![])
            } else if j == NY {
                add([Some(c(i, NY - 1)), None], [q, p], vec![])
            } else {
                add([Some(c(i, j)), Some(c(i, j - 1))], [p, q], bow)
            };
        }
    }
    for i in 0..=NX {
        for j in 0..NY {
            let (p, q) = (v(i, j), v(i, j + 1));
            let bend = if i % 3 == 1 && i < NX {
                let (a, b) = (vertices[p], vertices[q]);
                vec![[(a[0] + b[0]) * 0.5 + 0.2, (a[1] + b[1]) * 0.5]]
            } else {
                vec![]
            };
            vertical[i][j] = if i == 0 {
                add([Some(c(0, j)), None], [q, p], vec![])
            } else if i == NX {
                add([Some(c(NX - 1, j)), None], [p, q], vec![])
            } else {
                add([Some(c(i - 1, j)), Some(c(i, j))], [p, q], bend)
            };
        }
    }
    for j in 0..NY {
        for i in 0..NX {
            cell_faces[c(i, j)] = vec![
                horizontal[j][i],
                vertical[i + 1][j],
                horizontal[j + 1][i],
                vertical[i][j],
            ];
        }
    }
    let mut vertex_faces = Vec::new();
    for j in 0..=NY {
        for i in 0..=NX {
            let mut ring = Vec::new();
            if i < NX {
                ring.push(horizontal[j][i]);
            }
            if j < NY {
                ring.push(vertical[i][j]);
            }
            if i > 0 {
                ring.push(horizontal[j][i - 1]);
            }
            if j > 0 {
                ring.push(vertical[i][j - 1]);
            }
            vertex_faces.push(ring);
        }
    }
    Mesh {
        vertices,
        centers,
        cell_faces,
        face_cells,
        face_vertices,
        face_points,
        vertex_faces,
    }
}

fn terrain_at(x: f64, y: f64) -> f32 {
    (0.1 * x + 0.05 * y + 0.4 * (x * 0.31).sin() * (y * 0.23).cos()) as f32
}

fn write(dir: &Path, name: &str, bytes: Vec<u8>) -> String {
    fs::write(dir.join(name), bytes).unwrap();
    name.to_string()
}
fn le_f32(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}
fn le_pairs_f64(values: &[[f64; 2]]) -> Vec<u8> {
    values
        .iter()
        .flatten()
        .flat_map(|v| v.to_le_bytes())
        .collect()
}
fn le_pairs_i32(values: &[[i32; 2]]) -> Vec<u8> {
    values
        .iter()
        .flatten()
        .flat_map(|v| v.to_le_bytes())
        .collect()
}
fn csr(rows: &[Vec<usize>]) -> (Vec<[i32; 2]>, Vec<[i32; 2]>) {
    let mut info = Vec::new();
    let mut values = Vec::new();
    for row in rows {
        info.push([values.len() as i32, row.len() as i32]);
        // Orientation column is ignored by incidence; keep it nonzero.
        values.extend(row.iter().map(|&f| [f as i32, -1]));
    }
    (info, values)
}
fn framed_polygon(ring: &[[f64; 2]]) -> Vec<u8> {
    let mut wkb = vec![1];
    for i in [3u32, 1, ring.len() as u32 + 1] {
        wkb.extend(i.to_le_bytes());
    }
    for point in ring.iter().chain(std::iter::once(&ring[0])) {
        for v in point {
            wkb.extend(v.to_le_bytes());
        }
    }
    let mut bytes = (wkb.len() as u32).to_le_bytes().to_vec();
    bytes.extend(wkb);
    bytes
}

struct Model {
    fixture: Fixture,
    mesh: Mesh,
    minima: Vec<f32>,
    profiles: Vec<Vec<f32>>,
    owners: Vec<usize>,
}

/// `elevations`: stored corner elevations (depth weighting); `full_minima`: a
/// cell-minimum row for every cell including ghosts (the other admitted layout).
fn model_with(owner_box: [usize; 4], elevations: bool, full_minima: bool) -> Model {
    let dir = std::env::temp_dir().join(format!(
        "fast-map-staged-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&dir).unwrap();
    use ras_commander_fast_map::effective_terrain::{self, Effective, Grid};
    let (width, height) = (NX * SIZE as usize, NY * SIZE as usize);
    effective_terrain::write_geotiff(
        &dir.join("terrain.tif"),
        Effective {
            grid: Grid {
                origin_x: 0.0,
                origin_y: height as f64,
                cell_size: 1.0,
                width,
                height,
            },
            values: (0..height)
                .flat_map(|r| (0..width).map(move |col| (r, col)))
                .map(|(r, col)| terrain_at(col as f64 + 0.5, height as f64 - r as f64 - 0.5))
                .collect(),
            report: Value::Null,
        },
        3857,
    )
    .unwrap();
    let m = mesh();
    let physical = NX * NY;
    let minima: Vec<f32> = (0..physical)
        .map(|k| terrain_at(m.centers[k][0], m.centers[k][1]) - 0.6)
        .collect();
    let face_minima: Vec<f32> = m
        .face_cells
        .iter()
        .map(|&[a, _]| minima[a] + 0.15 * (a as f32 * 0.37).sin())
        .collect();
    let (cell_info, cell_faces) = csr(&m.cell_faces);
    let (vertex_info, vertex_faces) = csr(&m.vertex_faces);
    let mut face_info = Vec::new();
    let mut interior = Vec::new();
    for points in &m.face_points {
        face_info.push([interior.len() as i32, points.len() as i32]);
        interior.extend(points.iter().copied());
    }
    let mut stored_minima = minima.clone();
    if full_minima {
        stored_minima.extend((physical..m.centers.len()).map(|g| 1000.0 + g as f32));
    }
    let mut arrays = json!({
        "centers":{"path":write(&dir,"centers.bin",le_pairs_f64(&m.centers)),"rows":m.centers.len()},
        "cell_info":{"path":write(&dir,"cell_info.bin",le_pairs_i32(&cell_info)),"rows":cell_info.len()},
        "cell_faces":{"path":write(&dir,"cell_faces.bin",le_pairs_i32(&cell_faces)),"rows":cell_faces.len()},
        "cell_minimum":{"path":write(&dir,"cell_minimum.bin",le_f32(&stored_minima)),"rows":stored_minima.len()},
        "face_vertices":{"path":write(&dir,"face_vertices.bin",le_pairs_i32(&m.face_vertices.iter().map(|e| [e[0] as i32,e[1] as i32]).collect::<Vec<_>>())),"rows":m.face_vertices.len()},
        "face_cells":{"path":write(&dir,"face_cells.bin",le_pairs_i32(&m.face_cells.iter().map(|e| [e[0] as i32,e[1] as i32]).collect::<Vec<_>>())),"rows":m.face_cells.len()},
        "face_info":{"path":write(&dir,"face_info.bin",le_pairs_i32(&face_info)),"rows":face_info.len()},
        "face_points":{"path":write(&dir,"face_points.bin",le_pairs_f64(&interior)),"rows":interior.len()},
        "face_minimum":{"path":write(&dir,"face_minimum.bin",le_f32(&face_minima)),"rows":face_minima.len()},
        "vertices":{"path":write(&dir,"vertices.bin",le_pairs_f64(&m.vertices)),"rows":m.vertices.len()},
        "vertex_info":{"path":write(&dir,"vertex_info.bin",le_pairs_i32(&vertex_info)),"rows":vertex_info.len()},
        "vertex_faces":{"path":write(&dir,"vertex_faces.bin",le_pairs_i32(&vertex_faces)),"rows":vertex_faces.len()},
    });
    if elevations {
        let z: Vec<f32> = m
            .vertices
            .iter()
            .map(|p| terrain_at(p[0], p[1]) - 0.3)
            .collect();
        arrays["vertex_elevations"] =
            json!({"path":write(&dir,"vertex_elevations.bin",le_f32(&z)),"rows":z.len()});
    }
    fs::write(
        dir.join("mesh.json"),
        json!({"schema":"ras-commander-sloping-mesh/1","arrays":arrays}).to_string(),
    )
    .unwrap();
    // Two profiles with wet, dry, levee and backfill faces; ghosts copy neighbours.
    let profiles: Vec<Vec<f32>> = [(1.0_f32, 0.7_f32), (0.4, 1.3)]
        .iter()
        .map(|&(base, phase)| {
            let mut wse: Vec<f32> = (0..physical)
                .map(|k| {
                    let (i, j) = ((k % NX) as f32, (k / NX) as f32);
                    minima[k] + base + 1.1 * (phase * i).sin() + 0.6 * (0.5 * j + phase).cos()
                })
                .collect();
            for g in physical..m.centers.len() {
                let face = m.cell_faces[g][0];
                wse.push(wse[m.face_cells[face][0]]);
            }
            wse
        })
        .collect();
    for (p, values) in profiles.iter().enumerate() {
        write(&dir, &format!("full-{p}.bin"), le_f32(values));
    }
    let [i0, i1, j0, j1] = owner_box;
    let owners: Vec<usize> = (j0..j1)
        .flat_map(|j| (i0..i1).map(move |i| c(i, j)))
        .collect();
    let mut geometry = Vec::new();
    for &k in &owners {
        let (i, j) = (k % NX, k / NX);
        let ring = [v(i, j), v(i + 1, j), v(i + 1, j + 1), v(i, j + 1)].map(|q| m.vertices[q]);
        geometry.extend(framed_polygon(&ring));
    }
    write(&dir, "owners.wkb", geometry);
    write(
        &dir,
        "aoi.wkb",
        framed_polygon(&[[5.0, 5.0], [33.0, 6.0], [31.0, 27.0], [7.0, 25.0]]),
    );
    Model {
        fixture: Fixture(dir),
        mesh: m,
        minima,
        profiles,
        owners,
    }
}

/// Independent closure: owner vertices, their complete face rings, both cells.
fn closure(m: &Mesh, owners: &[usize]) -> Vec<usize> {
    let mut cells = BTreeSet::new();
    for &owner in owners {
        for &face in &m.cell_faces[owner] {
            for vertex in m.face_vertices[face] {
                for &f in &m.vertex_faces[vertex] {
                    cells.extend(m.face_cells[f]);
                }
            }
        }
    }
    cells.into_iter().collect()
}

impl Model {
    fn dir(&self) -> &Path {
        &self.fixture.0
    }
    fn request(&self, staged: Option<&[usize]>, options: Value) -> Value {
        let (files, values): (Option<Vec<String>>, Vec<String>) = match staged {
            None => (None, (0..2).map(|p| format!("full-{p}.bin")).collect()),
            Some(cells) => {
                let tag = NEXT.fetch_add(1, Ordering::Relaxed);
                let list = write(
                    self.dir(),
                    &format!("staged-{tag}.bin"),
                    cells
                        .iter()
                        .flat_map(|&c| (c as u32).to_le_bytes())
                        .collect(),
                );
                let values = (0..2)
                    .map(|p| {
                        let subset: Vec<f32> = cells.iter().map(|&c| self.profiles[p][c]).collect();
                        write(
                            self.dir(),
                            &format!("staged-{tag}-{p}.bin"),
                            le_f32(&subset),
                        )
                    })
                    .collect();
                (Some(vec![list]), values)
            }
        };
        let mut request = json!({"schema":sloping_session::SCHEMA,"options":options,
            "terrain":"terrain.tif","epsg":3857,"crs_identity":{"kind":"epsg"},
            "terrain_transform":[0.,1.,0.,(NY as f64)*SIZE,0.,-1.],
            "window":[0,0,NX*SIZE as usize,NY*SIZE as usize],"geometry_file":"owners.wkb",
            "aoi_file":"aoi.wkb","mesh_files":["mesh.json"],
            "owner_cells":self.owners.iter().map(|&c| [0,c]).collect::<Vec<_>>(),
            "profiles":values.iter().enumerate().map(|(p,f)| json!({"id":format!("p{p}"),"values_files":[f]})).collect::<Vec<_>>(),
            "threads":3,"max_working_bytes":512u64<<20,"window_cache_bytes":0});
        if let Some(files) = files {
            request["staged_cells_files"] = json!(files);
        }
        request
    }
    fn run(&self, request: &Value, name: &str) -> Result<(Value, Vec<Vec<u8>>), String> {
        self.run_in(request, name, None)
    }
    fn run_in(
        &self,
        request: &Value,
        name: &str,
        session: Option<&mut sloping_session::PreparedSession>,
    ) -> Result<(Value, Vec<Vec<u8>>), String> {
        let req: Request = serde_json::from_value(request.clone()).unwrap();
        let out = self.dir().join(name);
        let report = sloping_session::run_with_session(&req, self.dir(), &out, session)
            .map_err(|e| e.to_string())?;
        let mut bytes = Vec::new();
        for p in 0..2 {
            for product in ["depth.bin", "wse.bin"] {
                bytes.push(fs::read(out.join(format!("p{p}")).join(product)).unwrap());
            }
        }
        Ok((report, bytes))
    }
}

fn no_aoi(mut r: Value) -> Value {
    r.as_object_mut().unwrap().remove("aoi_file");
    r
}

// ---------------------------------------------------------------------------
// Bounded corner fits (`rasmapper-plain-sloping-bounded-corners/1`).
//
// Only JSON requests are used here (no new Rust symbols), so this file also
// compiles at the pre-change commit, where the golden hashes were recorded.

const BOUNDED: &str = "rasmapper-plain-sloping-bounded-corners/1";

fn bound(tolerance: f64) -> Value {
    json!({"corner_fit_bound":{"mode":"input_range","tolerance":tolerance}})
}

/// Profile 1 with a first-wetting stencil at the bowed vertex: the two cells
/// south of it dry, the north-west cell deep and the north-east cell shallow.
fn spiking(m: &Model) -> Vec<f32> {
    let mut wse = m.profiles[1].clone();
    let (i, j) = (SPIKE_I, SPIKE_J);
    for (cell, depth) in [
        (c(i - 1, j - 1), -0.2_f32),
        (c(i, j - 1), -0.2),
        (c(i - 1, j), 1.5),
        (c(i, j), 0.05),
    ] {
        wse[cell] = m.minima[cell] + depth;
    }
    wse
}

/// The fixture used throughout: profile 1 carries one out-of-range corner fit.
fn spiked(owner_box: [usize; 4], elevations: bool) -> Model {
    let mut m = model_with(owner_box, elevations, false);
    let wse = spiking(&m);
    write(m.dir(), "full-1.bin", le_f32(&wse));
    m.profiles[1] = wse;
    m
}

/// The four cells whose stencils contain the bowed vertex.
fn spike_cells() -> [usize; 4] {
    let (i, j) = (SPIKE_I, SPIKE_J);
    [c(i - 1, j - 1), c(i, j - 1), c(i - 1, j), c(i, j)]
}

fn units(report: &Value) -> Vec<Value> {
    report["profiles"].as_array().unwrap().clone()
}

fn fnv(bytes: &[u8]) -> String {
    format!(
        "{:016x}:{}",
        ras_commander_fast_map::sink::fnv1a64(bytes),
        bytes.len()
    )
}

fn read_tiff(path: &Path) -> Vec<u32> {
    let mut raster = terrio::LazyRaster::open_tiff(path.to_str().unwrap())
        .unwrap()
        .unwrap();
    let win = terrio::Window {
        x: 0,
        y: 0,
        width: raster.width(),
        height: raster.height(),
    };
    let ds = raster
        .read_window(
            win,
            &terrio::WindowOptions {
                bands: vec![1],
                overview: None,
            },
        )
        .unwrap();
    let terrio::raster::RasterData::F32(values) = ds.raster.data else {
        panic!("not Float32")
    };
    values.iter().map(|v| v.to_bits()).collect()
}

/// Grouped maxima of both profiles (source indices 4 and 9) in two jobs.
fn groups(m: &Model, model: Value, name: &str) -> (Value, Vec<Vec<u32>>) {
    let wire = json!({"schema":depth_groups::SCHEMA,"model":model,"source_indices":[4,9],
        "depth_units":"feet","depth_cutoff_ft":0.0,"max_accumulator_bytes":1<<20,"compression":"LZW","jobs":[
            {"job_id":"west","reach_id":"r1","window":[0,0,20,32],"aoi_file":"aoi.wkb","groups":[
                {"group_id":"both","timesteps":[9,4]},{"group_id":"spike","timesteps":[9]}]},
            {"job_id":"east","reach_id":"r2","window":[16,4,24,24],"groups":[
                {"group_id":"both","timesteps":[4,9]}]}]});
    let req: GroupRequest = serde_json::from_value(wire).unwrap();
    let report = depth_groups::run(&req, m.dir(), &m.dir().join(name)).unwrap();
    assert_eq!(report["status"], "complete", "{report}");
    let rasters = ["west/both.tif", "west/spike.tif", "east/both.tif"]
        .iter()
        .map(|p| read_tiff(&m.dir().join(name).join(p)))
        .collect();
    (report, rasters)
}

/// Every default rendering of the golden test: (name, depth/WSE file hashes).
fn default_renders() -> Vec<(String, Vec<String>)> {
    let mut renders = Vec::new();
    let m = spiked([0, NX, 0, NY], true);
    let staged = closure(&m.mesh, &m.owners);
    for (name, request) in [
        ("plain", no_aoi(m.request(None, json!({})))),
        ("plain-aoi", m.request(None, json!({}))),
        ("plain-staged", m.request(Some(&staged), json!({}))),
        (
            "pretty",
            no_aoi(m.request(None, json!({"render_mode":"slopingPretty"}))),
        ),
        (
            "pretty-shallow-weighted",
            no_aoi(m.request(
                None,
                json!({"render_mode":"slopingPretty","reduce_shallow_to_horizontal":true,
                    "use_depth_weighted_faces":true}),
            )),
        ),
    ] {
        let (_, bytes) = m.run(&request, name).unwrap();
        renders.push((name.to_string(), bytes.iter().map(|b| fnv(b)).collect()));
    }
    let (_, rasters) = groups(&m, m.request(None, json!({})), "groups");
    renders.push((
        "depth-groups".into(),
        rasters
            .iter()
            .map(|r| fnv(&r.iter().flat_map(|b| b.to_le_bytes()).collect::<Vec<_>>()))
            .collect(),
    ));
    renders
}

/// Recorded with this file's fixture at the pre-change commit 85ad9be
/// (`FAST_MAP_PRINT_GOLDEN=1 cargo test --test bounded_corners golden`).
const GOLDEN: &[(&str, &[&str])] = &[
    (
        "plain",
        &[
            "b11e67045e9dcc6e:5120",
            "1d11b07dbe15eded:5120",
            "d69886a100ac10d7:5120",
            "2a078e6cbb1e3925:5120",
        ],
    ),
    (
        "plain-aoi",
        &[
            "586a45075f7a082c:5120",
            "0003b3ace1be345f:5120",
            "88f95d4f9e7cbb45:5120",
            "c0fdf78f5f3ebf68:5120",
        ],
    ),
    (
        "plain-staged",
        &[
            "586a45075f7a082c:5120",
            "0003b3ace1be345f:5120",
            "88f95d4f9e7cbb45:5120",
            "c0fdf78f5f3ebf68:5120",
        ],
    ),
    (
        "pretty",
        &[
            "a78ba50d8fbb1221:5120",
            "d41c3764dd87327e:5120",
            "2d9084380f0293ea:5120",
            "eba817a415110196:5120",
        ],
    ),
    (
        "pretty-shallow-weighted",
        &[
            "35421fc2743c72ea:5120",
            "cd1bbc76e460aae3:5120",
            "7fdf67cedc7c9094:5120",
            "a0838780c488aedd:5120",
        ],
    ),
    (
        "depth-groups",
        &[
            "275f950d49fadf60:2560",
            "e940d9923a48b76d:2560",
            "1bdb457c7e937f86:2304",
        ],
    ),
];

#[test]
fn golden_default_bits_match_the_pre_change_engine() {
    let renders = default_renders();
    if std::env::var_os("FAST_MAP_PRINT_GOLDEN").is_some() {
        for (name, hashes) in &renders {
            println!("GOLDEN (\"{name}\", &{hashes:?}),");
        }
        return;
    }
    assert_eq!(renders.len(), GOLDEN.len());
    for ((name, hashes), (golden_name, golden)) in renders.iter().zip(GOLDEN) {
        assert_eq!(name, golden_name);
        assert_eq!(hashes, golden, "{name}: default output changed");
    }
}

#[test]
fn untriggered_bound_renders_native_bits() {
    // f32::MAX flags no finite fit: the bounded plumbing adds no arithmetic.
    let m = spiked([0, NX, 0, NY], false);
    let staged = closure(&m.mesh, &m.owners);
    for (name, native, wide) in [
        (
            "plain",
            no_aoi(m.request(None, json!({}))),
            no_aoi(m.request(None, bound(f32::MAX as f64))),
        ),
        (
            "aoi",
            m.request(None, json!({})),
            m.request(None, bound(f32::MAX as f64)),
        ),
        (
            "staged",
            m.request(Some(&staged), json!({})),
            m.request(Some(&staged), bound(f32::MAX as f64)),
        ),
    ] {
        let (native_report, native_bytes) = m.run(&native, &format!("{name}-native")).unwrap();
        let (report, bytes) = m.run(&wide, &format!("{name}-wide")).unwrap();
        assert_eq!(bytes, native_bytes, "{name}");
        assert_eq!(report["method"]["method_id"], BOUNDED);
        assert_eq!(
            native_report["method"]["method_id"],
            "rasmapper-plain-sloping/1"
        );
        for unit in units(&native_report) {
            assert!(
                unit.get("corner_fit_bound").is_none(),
                "native report unchanged"
            );
        }
        for unit in units(&report) {
            let record = &unit["corner_fit_bound"];
            assert_eq!(record["method_id"], BOUNDED);
            assert_eq!(record["bounded_corner_groups"], 0, "{record}");
            assert!(record["corner_groups_evaluated"].as_u64().unwrap() > 0);
        }
    }
    // Tolerance 0 on the profile without an out-of-range fit is native too.
    let (_, native) = m
        .run(&no_aoi(m.request(None, json!({}))), "p0-native")
        .unwrap();
    let (report, bounded) = m
        .run(&no_aoi(m.request(None, bound(0.0))), "p0-bounded")
        .unwrap();
    assert_eq!(
        units(&report)[0]["corner_fit_bound"]["bounded_corner_groups"],
        0
    );
    assert_eq!(bounded[0], native[0]);
    assert_eq!(bounded[1], native[1]);
}

fn as_f32(bytes: &[u8]) -> Vec<f32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect()
}

#[test]
fn bound_changes_only_cells_with_a_bounded_corner() {
    let m = spiked([0, NX, 0, NY], false);
    let request = no_aoi(m.request(None, json!({})));
    let (_, native) = m.run(&request, "native").unwrap();
    let owners = {
        // Physical ownership of every pixel (label - 1 indexes owner_cells).
        let mut r = request.clone();
        r["export_physical_ownership"] = true.into();
        let (_, _) = m.run(&r, "owners").unwrap();
        let raw = fs::read(m.dir().join("owners").join("owners.bin")).unwrap();
        raw.as_chunks::<4>()
            .0
            .iter()
            .map(|b| u32::from_le_bytes(*b))
            .collect::<Vec<_>>()
    };
    let ring = spike_cells();
    for (tolerance, expected_change) in [(0.0, true), (0.5, true)] {
        let name = format!("bounded-{tolerance}");
        let (report, bounded) = m
            .run(&no_aoi(m.request(None, bound(tolerance))), &name)
            .unwrap();
        let profiles = units(&report);
        assert_eq!(profiles[0]["corner_fit_bound"]["bounded_corner_groups"], 0);
        let record = &profiles[1]["corner_fit_bound"];
        assert_eq!(record["bounded_corner_groups"], 1, "{record}");
        assert_eq!(record["bounded_down"], 1, "{record}");
        assert_eq!(record["bounded_groups_min_wet_input_depth_lt_0p1"], 1);
        assert_eq!(record["bounded_groups_L_ge_3"], 1);
        assert_eq!(record["tolerance"], tolerance);
        // Under the bound no painted value exceeds its same-profile inputs.
        assert_eq!(
            record["same_profile_out_of_range"]["out_of_range_pixels"],
            0
        );
        assert!(
            record["same_profile_out_of_range"]["wet_pixels"]
                .as_u64()
                .unwrap()
                > 0
        );
        // Profile 0 is untouched; profile 1 changes only in the spike's cells.
        assert_eq!(bounded[0], native[0]);
        assert_eq!(bounded[1], native[1]);
        let mut changed = 0;
        for product in [2, 3] {
            let (a, b) = (as_f32(&native[product]), as_f32(&bounded[product]));
            for (pixel, (x, y)) in a.iter().zip(&b).enumerate() {
                if x.to_bits() != y.to_bits() {
                    changed += 1;
                    let label = owners[pixel] as usize;
                    assert!(label > 0, "product {product} pixel {pixel} unowned");
                    let cell = m.owners[label - 1];
                    assert!(ring.contains(&cell), "pixel {pixel} in cell {cell} changed");
                }
            }
        }
        assert_eq!(changed > 0, expected_change);
    }
}

#[test]
fn native_and_bounded_requests_never_share_a_reconstruction() {
    let m = spiked([0, NX, 0, NY], false);
    let mut session = sloping_session::PreparedSession::default();
    let mut runs = Vec::new();
    for (k, options) in [json!({}), bound(0.0), json!({}), bound(0.5), bound(0.0)]
        .into_iter()
        .enumerate()
    {
        let mut request = no_aoi(m.request(None, options));
        request["hydraulic_cache_bytes"] = (64u64 << 20).into();
        let (report, bytes) = m
            .run_in(&request, &format!("session-{k}"), Some(&mut session))
            .unwrap();
        let reused: Vec<_> = units(&report)
            .iter()
            .map(|u| u["hydraulic_reused"].as_bool().unwrap())
            .collect();
        runs.push((reused, bytes));
    }
    // A reconstruction is reused only by a request with the same options.
    assert_eq!(runs[0].0, [false, false]);
    assert_eq!(runs[1].0, [false, false], "bounded must not reuse native");
    assert_eq!(runs[2].0, [true, true]);
    assert_eq!(
        runs[3].0,
        [false, false],
        "another tolerance is another key"
    );
    assert_eq!(runs[4].0, [true, true]);
    assert_eq!(runs[0].1, runs[2].1);
    assert_eq!(runs[1].1, runs[4].1);
    assert_ne!(runs[0].1[2], runs[1].1[2], "the spike profile differs");
    assert_ne!(runs[1].1[2], runs[3].1[2], "tolerance 0.5 differs from 0");
}

#[test]
fn bounded_requests_are_refused_before_any_read() {
    let m = spiked([0, NX, 0, NY], false);
    let missing = |mut r: Value| {
        // Any read would fail on these; a refusal must come first.
        r["terrain"] = "missing-terrain.tif".into();
        r["mesh_files"] = json!(["missing-mesh.json"]);
        r
    };
    let refused = |request: Value, expect: &str| {
        let parsed: Result<Request, _> = serde_json::from_value(request.clone());
        let message = match parsed {
            Err(error) => error.to_string(),
            Ok(req) => {
                let out = m
                    .dir()
                    .join(format!("refused-{}", NEXT.fetch_add(1, Ordering::Relaxed)));
                let error =
                    sloping_session::run(&req, m.dir(), &out).expect_err("request must be refused");
                assert!(!out.exists(), "no output before the refusal");
                error.to_string()
            }
        };
        assert!(message.contains(expect), "{message} lacks {expect}");
        assert!(
            !message.contains("missing-"),
            "read before refusing: {message}"
        );
    };
    let base = || missing(no_aoi(m.request(None, json!({}))));
    let with = |options: Value| {
        let mut r = base();
        r["options"] = options;
        r
    };
    refused(with(bound(-0.5)), "tolerance must be finite and >= 0");
    refused(with(bound(1e39)), "tolerance must be finite and >= 0");
    refused(
        with(json!({"corner_fit_bound":{"mode":"input_range"}})),
        "missing field `tolerance`",
    );
    refused(
        with(json!({"corner_fit_bound":{"tolerance":0.0}})),
        "missing field `mode`",
    );
    refused(
        with(json!({"corner_fit_bound":{"mode":"face_range","tolerance":0.0}})),
        "unknown variant `face_range`",
    );
    refused(
        with(json!({"corner_fit_bound":{"mode":"input_range","tolerance":0.0,"gate":"first_wet"}})),
        "unknown field `gate`",
    );
    let mut pretty = bound(0.0);
    pretty["render_mode"] = "slopingPretty".into();
    refused(
        with(pretty),
        "rasmapper-sloping-with-faces-bounded-corners/1",
    );
    let mut velocity = with(bound(0.0));
    velocity["velocity_normals_files"] = json!(["normals.bin"]);
    refused(velocity, "velocity, derived products");
    let mut derived = with(bound(0.0));
    derived["derived_products"] = json!(["froude"]);
    refused(derived, "velocity, derived products");
    let mut reduced = with(bound(0.0));
    reduced["reduce_max"] = true.into();
    refused(reduced, "velocity, derived products");
}

#[test]
fn depth_groups_carry_the_bound_and_its_records() {
    let m = spiked([0, NX, 0, NY], false);
    let (native_report, native) = groups(&m, m.request(None, json!({})), "native-groups");
    assert!(native_report.get("corner_fit_bound").is_none());
    assert!(native_report.get("sloping_method_id").is_none());
    let (report, bounded) = groups(&m, m.request(None, bound(0.0)), "bounded-groups");
    assert_eq!(report["sloping_method_id"], BOUNDED);
    let summary = &report["corner_fit_bound"];
    assert_eq!(summary["method_id"], BOUNDED);
    assert_eq!(summary["tolerance"], 0.0);
    assert_eq!(summary["bounded_corner_groups"], 1, "{summary}");
    assert_eq!(summary["out_of_range_pixels"], 0, "{summary}");
    let profiles = summary["profiles"].as_array().unwrap();
    assert_eq!(profiles.len(), 2);
    for job in report["jobs"].as_array().unwrap() {
        for group in job["groups"].as_array().unwrap() {
            for frame in group["frames"].as_array().unwrap() {
                let record = &frame["corner_fit_bound"];
                assert_eq!(record["method_id"], BOUNDED, "{frame}");
                let expected = u64::from(frame["source_index"] == 9);
                assert_eq!(record["bounded_corner_groups"], expected, "{frame}");
                assert_eq!(
                    record["same_profile_out_of_range"]["out_of_range_pixels"],
                    0
                );
            }
        }
    }
    // The east job does not reach the spike; the west spike group does.
    assert_eq!(bounded[2], native[2]);
    assert_ne!(bounded[1], native[1]);
    // slopingPretty with the bound stays refused under depth groups.
    let mut pretty = bound(0.0);
    pretty["render_mode"] = "slopingPretty".into();
    let wire = json!({"schema":depth_groups::SCHEMA,"model":m.request(None, pretty),
        "source_indices":[4,9],"depth_units":"feet","max_accumulator_bytes":1<<20,
        "jobs":[{"job_id":"j","reach_id":"r","window":[0,0,20,32],"groups":[{"group_id":"g","timesteps":[4]}]}]});
    let req: GroupRequest = serde_json::from_value(wire).unwrap();
    let error = depth_groups::run(&req, m.dir(), &m.dir().join("pretty-groups"))
        .expect_err("slopingPretty with a bound is refused");
    assert!(
        error.to_string().contains("with-faces-bounded-corners"),
        "{error}"
    );
}
