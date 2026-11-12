//! Staged cell subsets: compact stencil closure renders the same bits as the
//! complete mesh, for plain and face sloping and grouped maxima, and refuses
//! subsets that miss a closure cell.
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

struct Model {
    fixture: Fixture,
    mesh: Mesh,
    minima: Vec<f32>,
    profiles: Vec<Vec<f32>>,
    owners: Vec<usize>,
}

fn model(owner_box: [usize; 4]) -> Model {
    model_with(owner_box, true, false)
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
    /// The same request for another owner set (its own ownership polygons).
    fn with_owners(&self, mut request: Value, owners: &[usize]) -> Value {
        let tag = NEXT.fetch_add(1, Ordering::Relaxed);
        let mut geometry = Vec::new();
        for &k in owners {
            let (i, j) = (k % NX, k / NX);
            let ring =
                [v(i, j), v(i + 1, j), v(i + 1, j + 1), v(i, j + 1)].map(|q| self.mesh.vertices[q]);
            geometry.extend(framed_polygon(&ring));
        }
        request["geometry_file"] = write(self.dir(), &format!("owners-{tag}.wkb"), geometry).into();
        request["owner_cells"] = json!(owners.iter().map(|&c| [0, c]).collect::<Vec<_>>());
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

#[test]
fn staged_closure_renders_complete_mesh_bits_plain_and_faces() {
    // Owners straddle square and jittered cells, bent faces and the AOI edge.
    let m = model([2, 8, 1, 6]);
    let staged = closure(&m.mesh, &m.owners);
    assert!(staged.len() < m.mesh.centers.len());
    assert!(m.minima.len() == NX * NY);
    for (mode, options) in [
        ("plain", json!({})),
        ("faces", json!({"render_mode":"slopingPretty"})),
        (
            "shallow",
            json!({"render_mode":"slopingPretty","reduce_shallow_to_horizontal":true}),
        ),
        (
            "depth-weighted",
            json!({"render_mode":"slopingPretty","use_depth_weighted_faces":true}),
        ),
        (
            "depth-weighted-shallow",
            json!({"render_mode":"slopingPretty","use_depth_weighted_faces":true,
                "reduce_shallow_to_horizontal":true}),
        ),
    ] {
        let (full_report, full) = m
            .run(&m.request(None, options.clone()), &format!("{mode}-full"))
            .unwrap();
        let (report, subset) = m
            .run(
                &m.request(Some(&staged), options.clone()),
                &format!("{mode}-staged"),
            )
            .unwrap();
        assert_eq!(report["status"], "complete", "{report}");
        assert_eq!(full, subset, "{mode}: staged bits differ");
        // Not vacuous: wet pixels exist and some pixels are dry/outside.
        let depth: Vec<f32> = full[0]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect();
        assert!(depth.iter().filter(|d| **d > 0.0).count() > 100, "{mode}");
        assert!(
            depth.iter().any(|d| !d.is_finite() || *d == -9999.0),
            "{mode}"
        );
        let record = &report["staged_cells"]["meshes"][0];
        assert_eq!(record["staged_cells"], staged.len());
        assert_eq!(record["parent_cells"], m.mesh.centers.len());
        assert!(full_report.get("staged_cells").is_none());
        assert!(
            report["planned_bytes"].as_u64().unwrap()
                < full_report["planned_bytes"].as_u64().unwrap()
        );
    }
    // A superset of the closure is admitted and renders the same bits.
    let mut superset = staged.clone();
    superset.extend([0, 1, m.mesh.centers.len() - 1]);
    superset.sort_unstable();
    superset.dedup();
    let (_, full) = m.run(&m.request(None, json!({})), "superset-full").unwrap();
    let (_, subset) = m
        .run(&m.request(Some(&superset), json!({})), "superset-staged")
        .unwrap();
    assert_eq!(full, subset);
}

#[test]
fn staged_subset_missing_a_closure_cell_or_owner_is_refused() {
    let m = model([3, 6, 2, 5]);
    let staged = closure(&m.mesh, &m.owners);
    let missing_neighbour: Vec<usize> = staged
        .iter()
        .copied()
        .filter(|&c| {
            c != staged
                .iter()
                .copied()
                .find(|c| !m.owners.contains(c))
                .unwrap()
        })
        .collect();
    let error = m
        .run(&m.request(Some(&missing_neighbour), json!({})), "missing")
        .unwrap_err();
    assert!(error.contains("do not cover"), "{error}");
    let missing_owner: Vec<usize> = staged
        .iter()
        .copied()
        .filter(|&c| c != m.owners[0])
        .collect();
    let error = m
        .run(&m.request(Some(&missing_owner), json!({})), "owner")
        .unwrap_err();
    assert!(error.contains("every owner cell"), "{error}");
    let mut request = m.request(Some(&staged), json!({}));
    request["staged_cells_files"] = json!(["a.bin", "b.bin"]);
    let error = m.run(&request, "count").unwrap_err();
    assert!(error.contains("one file per mesh"), "{error}");
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

#[test]
fn grouped_maxima_with_staged_cells_match_complete_mesh() {
    let m = model([1, 9, 1, 7]);
    let staged = closure(&m.mesh, &m.owners);
    let mut outputs = Vec::new();
    for (name, subset) in [("full", None), ("staged", Some(staged.as_slice()))] {
        let model = m.request(subset, json!({}));
        let wire = json!({"schema":depth_groups::SCHEMA,"model":model,"source_indices":[4,9],
            "depth_units":"feet","max_accumulator_bytes":1<<20,"compression":"LZW","jobs":[
                {"job_id":"west","reach_id":"r1","window":[0,0,20,32],"aoi_file":"aoi.wkb","groups":[
                    {"group_id":"both","timesteps":[9,4]},{"group_id":"one","timesteps":[4]}]},
                {"job_id":"east","reach_id":"r2","window":[16,4,24,24],"groups":[
                    {"group_id":"both","timesteps":[4,9]}]}]});
        let req: GroupRequest = serde_json::from_value(wire).unwrap();
        let report = depth_groups::run(&req, m.dir(), &m.dir().join(name)).unwrap();
        assert_eq!(report["status"], "complete", "{report}");
        assert_eq!(report["geometry_preparations"], 1);
        let mut frames = Vec::new();
        for job in report["jobs"].as_array().unwrap() {
            for group in job["groups"].as_array().unwrap() {
                frames.push((
                    group["retained_count"].clone(),
                    group["frames"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|f| (f["native_wet_count"].clone(), f["retained_count"].clone()))
                        .collect::<Vec<_>>(),
                ));
            }
        }
        let rasters: Vec<_> = ["west/both.tif", "west/one.tif", "east/both.tif"]
            .iter()
            .map(|p| read_tiff(&m.dir().join(name).join(p)))
            .collect();
        outputs.push((frames, rasters, report["planned_bytes"].as_u64().unwrap()));
    }
    assert_eq!(outputs[0].0, outputs[1].0);
    assert_eq!(outputs[0].1, outputs[1].1);
    assert!(outputs[0].1[0].iter().any(|b| f32::from_bits(*b) > 0.0));
    assert!(outputs[1].2 < outputs[0].2);
}

fn boxed(b: [usize; 4]) -> Vec<usize> {
    (b[2]..b[3])
        .flat_map(|j| (b[0]..b[1]).map(move |i| c(i, j)))
        .collect()
}
fn no_aoi(mut r: Value) -> Value {
    r.as_object_mut().unwrap().remove("aoi_file");
    r
}

#[test]
fn perimeter_ghost_closures_and_stored_elevations_keep_bits() {
    // Whole mesh, west edge, north-east corner block and the single corner cell:
    // every closure holds ghost cells and perimeter faces.
    for (name, owner_box, elevations, full_minima) in [
        ("all", [0, NX, 0, NY], true, false),
        ("west", [0, 3, 0, NY], true, true),
        ("ne", [7, NX, 5, NY], true, false),
        ("corner", [0, 1, 0, 1], false, true),
    ] {
        let m = model_with(owner_box, elevations, full_minima);
        let staged = closure(&m.mesh, &m.owners);
        assert!(
            staged.iter().any(|&c| c >= NX * NY),
            "{name}: no ghost cell"
        );
        let mut modes = vec![
            ("plain", json!({})),
            ("faces", json!({"render_mode":"slopingPretty"})),
        ];
        if elevations {
            modes.push((
                "dw",
                json!({"render_mode":"slopingPretty","use_depth_weighted_faces":true}),
            ));
            modes.push((
                "dw-shallow",
                json!({"render_mode":"slopingPretty","use_depth_weighted_faces":true,
                    "reduce_shallow_to_horizontal":true}),
            ));
        }
        for (mode, options) in modes {
            let (_, full) = m
                .run(
                    &no_aoi(m.request(None, options.clone())),
                    &format!("{name}-{mode}-full"),
                )
                .unwrap();
            let (_, subset) = m
                .run(
                    &no_aoi(m.request(Some(&staged), options)),
                    &format!("{name}-{mode}-staged"),
                )
                .unwrap();
            let depth: Vec<f32> = full[0]
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| f32::from_le_bytes(*b))
                .collect();
            assert!(
                depth.iter().any(|d| *d > 0.0 && *d < 1e4),
                "{name}/{mode} vacuous"
            );
            assert_eq!(full, subset, "{name}/{mode}: staged bits differ");
        }
    }
}

fn two_meshes(m: &Model, request: Value, staged: bool) -> Value {
    // The same mesh twice; owners only in mesh 0; staged, mesh 1 holds cell 0 only.
    let mut req = request;
    req["mesh_files"] = json!(["mesh.json", "mesh.json"]);
    let (extra, values): (&str, Vec<f32>) = if staged {
        fs::write(m.dir().join("one.bin"), 0u32.to_le_bytes()).unwrap();
        ("one-v.bin", vec![m.profiles[0][0]])
    } else {
        ("all-v.bin", m.profiles[0].clone())
    };
    write(m.dir(), extra, le_f32(&values));
    if staged {
        req["staged_cells_files"]
            .as_array_mut()
            .unwrap()
            .push(json!("one.bin"));
    }
    for p in req["profiles"].as_array_mut().unwrap() {
        p["values_files"].as_array_mut().unwrap().push(json!(extra));
    }
    req
}

#[test]
fn mesh_without_owners_renders_plain_and_row_streamed() {
    let m = model([2, 8, 1, 6]);
    let staged = closure(&m.mesh, &m.owners);
    let full = two_meshes(&m, no_aoi(m.request(None, json!({}))), false);
    let subset = two_meshes(&m, no_aoi(m.request(Some(&staged), json!({}))), true);
    assert_eq!(
        m.run(&full, "two-full").unwrap().1,
        m.run(&subset, "two-staged").unwrap().1
    );
    let rows: ras_commander_fast_map::native_output::OutputOptions =
        serde_json::from_value(json!({"row_stream_rows":8})).unwrap();
    let mut tiffs = Vec::new();
    for (name, mut req) in [("rows-full", full), ("rows-staged", subset)] {
        req["geotiff_outputs"] = json!(true);
        req["stream_outputs"] = json!(true);
        let r: Request = serde_json::from_value(req).unwrap();
        let out = m.dir().join(name);
        let report =
            sloping_session::run_with_output_options(&r, m.dir(), &out, None, Some(&rows)).unwrap();
        assert_eq!(report["status"], "complete", "{report}");
        tiffs.push(
            (0..2)
                .map(|p| fs::read(out.join(format!("p{p}/depth.tif"))).unwrap())
                .collect::<Vec<_>>(),
        );
    }
    assert_eq!(tiffs[0], tiffs[1]);
}

#[test]
fn one_session_switches_owner_sets_and_refusal_keeps_its_geometry() {
    let m = model([0, 1, 0, 1]);
    let (a, b) = (boxed([1, 4, 1, 4]), boxed([5, 9, 3, 7]));
    let mut both = a.clone();
    both.extend(&b);
    let staged = closure(&m.mesh, &both);
    // One staged file and values for both owner sets.
    let base = no_aoi(m.request(Some(&staged), json!({})));
    let full = no_aoi(m.request(None, json!({})));
    let mut session = sloping_session::PreparedSession::default();
    for (step, owners) in [("a", &a), ("b", &b), ("a2", &a)] {
        let expected = m
            .run(
                &m.with_owners(full.clone(), owners),
                &format!("full-{step}"),
            )
            .unwrap()
            .1;
        let (report, got) = m
            .run_in(
                &m.with_owners(base.clone(), owners),
                &format!("s-{step}"),
                Some(&mut session),
            )
            .unwrap();
        assert_eq!(expected, got, "{step}: staged bits differ");
        // An owner-set change is a different compact geometry.
        assert_eq!(report["session"]["geometry_reused"], false, "{step}");
    }
    // Full geometry in the same session, then back to the subset.
    let (_, got) = m
        .run_in(
            &m.with_owners(full.clone(), &a),
            "s-full",
            Some(&mut session),
        )
        .unwrap();
    assert_eq!(
        got,
        m.run(&m.with_owners(full.clone(), &a), "full-a3")
            .unwrap()
            .1
    );
    let request = m.with_owners(base.clone(), &a);
    m.run_in(&request, "s-a3", Some(&mut session)).unwrap();
    // A refused request for another owner set leaves the admitted geometry in place.
    let mut refused = m.with_owners(base.clone(), &b);
    refused["max_working_bytes"] = json!((64u64 << 20) + 1);
    let error = m
        .run_in(&refused, "s-refused", Some(&mut session))
        .unwrap_err();
    assert!(error.contains("staged geometry load"), "{error}");
    let (report, _) = m.run_in(&request, "s-a4", Some(&mut session)).unwrap();
    assert_eq!(report["session"]["geometry_reused"], true, "{report}");
}

#[test]
fn malformed_compact_dimensions_are_errors_not_panics() {
    for (key, rows_delta, width) in [
        ("face_minimum", -1_i64, 4),
        ("face_info", -1, 8),
        ("vertex_elevations", 1, 4),
    ] {
        let m = model([2, 8, 1, 6]);
        let staged = closure(&m.mesh, &m.owners);
        let path = m.dir().join("mesh.json");
        let mut header: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let array = &mut header["arrays"][key];
        let rows = array["rows"].as_i64().unwrap() + rows_delta;
        let file = m.dir().join(array["path"].as_str().unwrap());
        let mut bytes = fs::read(&file).unwrap();
        bytes.resize((rows * width) as usize, 0);
        fs::write(&file, bytes).unwrap();
        array["rows"] = rows.into();
        fs::write(&path, header.to_string()).unwrap();
        let error = m
            .run(&m.request(Some(&staged), json!({})), "bad")
            .unwrap_err();
        assert!(error.contains("dimension"), "{key}: {error}");
    }
}
