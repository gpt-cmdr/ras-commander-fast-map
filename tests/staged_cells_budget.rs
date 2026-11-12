//! Staged load admission measured with a counting allocator (300 x 300 quad mesh):
//! refusals stay inside their budget and admitted plans bound the heap.
use ras_commander_fast_map::sloping_session::{self, Request};
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

const NX: usize = 300;
const NY: usize = 300;
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
            horizontal[j][i] = if j == 0 {
                add([Some(c(i, 0)), None], [p, q], vec![])
            } else if j == NY {
                add([Some(c(i, NY - 1)), None], [q, p], vec![])
            } else {
                add([Some(c(i, j)), Some(c(i, j - 1))], [p, q], vec![])
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

#[allow(dead_code)] // The shared fixture carries fields other tests read.
struct Model {
    fixture: Fixture,
    mesh: Mesh,
    minima: Vec<f32>,
    profiles: Vec<Vec<f32>>,
    owners: Vec<usize>,
}

fn model(owner_box: [usize; 4]) -> Model {
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
    let arrays = json!({
        "centers":{"path":write(&dir,"centers.bin",le_pairs_f64(&m.centers)),"rows":m.centers.len()},
        "cell_info":{"path":write(&dir,"cell_info.bin",le_pairs_i32(&cell_info)),"rows":cell_info.len()},
        "cell_faces":{"path":write(&dir,"cell_faces.bin",le_pairs_i32(&cell_faces)),"rows":cell_faces.len()},
        "cell_minimum":{"path":write(&dir,"cell_minimum.bin",le_f32(&minima)),"rows":minima.len()},
        "face_vertices":{"path":write(&dir,"face_vertices.bin",le_pairs_i32(&m.face_vertices.iter().map(|e| [e[0] as i32,e[1] as i32]).collect::<Vec<_>>())),"rows":m.face_vertices.len()},
        "face_cells":{"path":write(&dir,"face_cells.bin",le_pairs_i32(&m.face_cells.iter().map(|e| [e[0] as i32,e[1] as i32]).collect::<Vec<_>>())),"rows":m.face_cells.len()},
        "face_info":{"path":write(&dir,"face_info.bin",le_pairs_i32(&face_info)),"rows":face_info.len()},
        "face_points":{"path":write(&dir,"face_points.bin",le_pairs_f64(&interior)),"rows":interior.len()},
        "face_minimum":{"path":write(&dir,"face_minimum.bin",le_f32(&face_minima)),"rows":face_minima.len()},
        "vertices":{"path":write(&dir,"vertices.bin",le_pairs_f64(&m.vertices)),"rows":m.vertices.len()},
        "vertex_info":{"path":write(&dir,"vertex_info.bin",le_pairs_i32(&vertex_info)),"rows":vertex_info.len()},
        "vertex_faces":{"path":write(&dir,"vertex_faces.bin",le_pairs_i32(&vertex_faces)),"rows":vertex_faces.len()},
    });
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

#[allow(dead_code)]
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
        let req: Request = serde_json::from_value(request.clone()).unwrap();
        let out = self.dir().join(name);
        let report = sloping_session::run(&req, self.dir(), &out).map_err(|e| e.to_string())?;
        let mut bytes = Vec::new();
        for p in 0..2 {
            for product in ["depth.bin", "wse.bin"] {
                bytes.push(fs::read(out.join(format!("p{p}")).join(product)).unwrap());
            }
        }
        Ok((report, bytes))
    }
}

// Counting allocator: this binary holds one test, so the global count is its own.
use std::alloc::{GlobalAlloc, Layout, System};
struct Counting;
static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            let now = CURRENT.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK.fetch_max(now, Ordering::Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        unsafe { System.dealloc(p, layout) };
        CURRENT.fetch_sub(layout.size(), Ordering::Relaxed);
    }
    unsafe fn realloc(&self, p: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let q = unsafe { System.realloc(p, layout, size) };
        if !q.is_null() {
            if size > layout.size() {
                let now = CURRENT.fetch_add(size - layout.size(), Ordering::Relaxed) + size
                    - layout.size();
                PEAK.fetch_max(now, Ordering::Relaxed);
            } else {
                CURRENT.fetch_sub(layout.size() - size, Ordering::Relaxed);
            }
        }
        q
    }
}
#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Heap peak above the current level while `work` runs.
fn heap_peak<R>(work: impl FnOnce() -> R) -> (R, u64) {
    let base = CURRENT.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    let result = work();
    (result, (PEAK.load(Ordering::Relaxed) - base) as u64)
}

const CACHE: u64 = 64 << 20;

/// The staged load is charged before it allocates: at any budget below what it
/// needs it is refused with its heap inside that budget (plus the uncharged
/// request bookkeeping), and an admitted run stays inside its planned bytes.
#[test]
fn staged_load_is_admitted_before_it_allocates_and_plans_bound_the_heap() {
    let m = model([0, NX, 0, NY / 2]);
    let staged = closure(&m.mesh, &m.owners);
    let mut base = m.request(Some(&staged), json!({}));
    base.as_object_mut().unwrap().remove("aoi_file");
    base["threads"] = json!(1);
    // Admitted runs: the planned bytes bound the measured heap, full and staged.
    let mut charged = 0;
    for (name, request) in [
        ("full", {
            let mut r = m.request(None, json!({}));
            r.as_object_mut().unwrap().remove("aoi_file");
            r["threads"] = json!(1);
            r
        }),
        ("staged", base.clone()),
    ] {
        let req: Request = serde_json::from_value(request).unwrap();
        let (report, peak) =
            heap_peak(|| sloping_session::run(&req, m.dir(), &m.dir().join(name)).unwrap());
        let planned = report["planned_bytes"].as_u64().unwrap();
        eprintln!("{name}: planned {planned} heap peak {peak}");
        assert!(
            peak <= planned,
            "{name}: heap {peak} above planned {planned}"
        );
        if name == "staged" {
            charged = report["staged_cells"]["load_peak_charged_bytes"]
                .as_u64()
                .unwrap();
        }
    }
    assert!(
        charged > 1 << 20,
        "fixture too small to exercise the charge: {charged}"
    );
    // Uncharged bookkeeping: request and key copies of the owner list, headers.
    let slack = 64 * m.owners.len() as u64 + (1 << 20);
    for fraction in [0.0, 0.25, 0.5, 0.75, 0.99] {
        let limit = (charged as f64 * fraction) as u64;
        let mut request = base.clone();
        request["max_working_bytes"] = json!(CACHE + limit);
        let req: Request = serde_json::from_value(request).unwrap();
        let out = m.dir().join(format!("refused-{fraction}"));
        let (result, peak) = heap_peak(|| sloping_session::run(&req, m.dir(), &out));
        let error = result.unwrap_err().to_string();
        eprintln!("load budget {limit}: heap peak {peak}: {error}");
        assert!(error.contains("staged geometry load"), "{error}");
        assert!(
            peak <= limit + slack,
            "refused load used {peak} bytes, budget {limit} + slack {slack}"
        );
    }
}
