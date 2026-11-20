//! Binary inputs for Fast-Map's sloping session:
//! `_sloping_inputs._write_binary_mesh_payloads`, `_sloping.staged_cell_subsets`,
//! `_sloping.write_staged_cells` and `_sloping._stage_profiles` (complete-row
//! HDF path with cell subsets), ras-commander 0.124.0.

use crate::error::{Error, Result};
use crate::hdf::{self, Hdf};
use std::io::Write;
use std::path::{Path, PathBuf};

/// `_BINARY_FIELDS` in order: payload key, HDF field, dtype.
pub const BINARY_FIELDS: [(&str, &str, &str); 11] = [
    ("cell_info", "Cells Face and Orientation Info", "<i4"),
    ("cell_faces", "Cells Face and Orientation Values", "<i4"),
    ("cell_minimum", "Cells Minimum Elevation", "<f4"),
    ("face_vertices", "Faces FacePoint Indexes", "<i4"),
    ("face_cells", "Faces Cell Indexes", "<i4"),
    ("face_info", "Faces Perimeter Info", "<i4"),
    ("face_points", "Faces Perimeter Values", "<f8"),
    ("face_minimum", "Faces Minimum Elevation", "<f4"),
    ("vertices", "FacePoints Coordinate", "<f8"),
    ("vertex_info", "FacePoints Face and Orientation Info", "<i4"),
    (
        "vertex_faces",
        "FacePoints Face and Orientation Values",
        "<i4",
    ),
];

/// The `i4` incidence arrays the staged-cell closure reads, kept from staging.
pub struct MeshTopology {
    pub cell_info: Vec<i32>,
    pub cell_faces: Vec<i32>,
    pub face_vertices: Vec<i32>,
    pub face_cells: Vec<i32>,
    pub vertex_info: Vec<i32>,
    pub vertex_faces: Vec<i32>,
}

pub struct MeshPayload {
    pub name: String,
    pub cell_count: usize,
    pub header: PathBuf,
    pub files: Vec<PathBuf>,
    pub bytes: u64,
    pub topology: MeshTopology,
}

fn write_all(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut f = std::fs::File::create(path)?;
    for chunk in bytes.chunks(1 << 20) {
        f.write_all(chunk)?;
    }
    Ok(())
}

fn i32s(bytes: &[u8]) -> Vec<i32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

/// Python `json.dumps` of the mesh descriptor (default separators).
fn header_json(arrays: &[(String, String, usize)]) -> String {
    let body: Vec<String> = arrays
        .iter()
        .map(|(k, p, r)| format!("\"{k}\": {{\"path\": \"{p}\", \"rows\": {r}}}"))
        .collect();
    format!(
        "{{\"schema\": \"ras-commander-sloping-mesh/1\", \"arrays\": {{{}}}}}",
        body.join(", ")
    )
}

/// `_write_binary_mesh_payloads(include_max=False, include_vertex_elevations=False)`.
/// `staged` counts bytes against `max_staged_bytes` exactly as Python does.
pub fn write_meshes(
    hdf: &Hdf,
    names: &[String],
    root: &Path,
    max_staged_bytes: i64,
) -> Result<Vec<MeshPayload>> {
    let mut staged: i64 = 0;
    let mut out = Vec::new();
    let over = || Error::unsupported("Prepared sloping inputs exceed max_prepared_mib");
    for (index, name) in names.iter().enumerate() {
        let centres = hdf.cell_centres(name)?;
        let mut arrays: Vec<(String, String, usize)> = Vec::new();
        let mut files = Vec::new();
        let mut keep: std::collections::HashMap<&str, Vec<i32>> = Default::default();
        let mut stage = |key: &str, bytes: Vec<u8>, columns: usize, item: usize| -> Result<()> {
            let rows = bytes.len() / item / columns;
            if rows * item * columns != bytes.len() {
                return Err(Error::invalid(format!(
                    "cannot reshape {key} into {columns} columns"
                )));
            }
            // `_write_array` casts a memoryview, which fails for an empty array.
            if bytes.is_empty() {
                return Err(Error::new(
                    crate::error::Kind::Type,
                    "memoryview: cannot cast view with zeros in shape or strides",
                ));
            }
            let filename = format!("mesh-{index}-{key}.bin");
            if staged + bytes.len() as i64 > max_staged_bytes {
                return Err(over());
            }
            let path = root.join(&filename);
            write_all(&path, &bytes)?;
            staged += bytes.len() as i64;
            arrays.push((key.to_string(), filename, rows));
            files.push(path);
            Ok(())
        };
        let centre_bytes: Vec<u8> = centres
            .iter()
            .flat_map(|p| [p[0].to_le_bytes(), p[1].to_le_bytes()])
            .flatten()
            .collect();
        stage("centers", centre_bytes, 2, 8)?;
        for (key, field, dtype) in BINARY_FIELDS {
            let t = hdf.table(&format!("Geometry/2D Flow Areas/{name}/{field}"))?;
            let bytes = hdf::payload_bytes(&t.values, dtype)?;
            let columns = if key.ends_with("minimum") { 1 } else { 2 };
            let item = if dtype == "<f8" { 8 } else { 4 };
            if dtype == "<i4" {
                keep.insert(key, i32s(&bytes));
            }
            stage(key, bytes, columns, item)?;
        }
        let encoded = header_json(&arrays);
        if staged + encoded.len() as i64 > max_staged_bytes {
            return Err(over());
        }
        let header = root.join(format!("mesh-{index}.json"));
        write_all(&header, encoded.as_bytes())?;
        staged += encoded.len() as i64;
        let bytes = files
            .iter()
            .map(|f| std::fs::metadata(f).map(|m| m.len()))
            .sum::<std::io::Result<u64>>()?
            + encoded.len() as u64;
        let mut take = |k: &str| keep.remove(k).unwrap_or_default();
        out.push(MeshPayload {
            name: name.clone(),
            cell_count: centres.len(),
            header,
            files,
            bytes,
            topology: MeshTopology {
                cell_info: take("cell_info"),
                cell_faces: take("cell_faces"),
                face_vertices: take("face_vertices"),
                face_cells: take("face_cells"),
                vertex_info: take("vertex_info"),
                vertex_faces: take("vertex_faces"),
            },
        });
    }
    Ok(out)
}

/// `_gather_incidence`: CSR rows `rows` of `values` (first column), row order.
fn gather(info: &[i32], values: &[i32], rows: &[i64]) -> Result<Vec<i64>> {
    let nvalues = (values.len() / 2) as i64;
    let mut out = Vec::new();
    for &r in rows {
        let r = usize::try_from(r)
            .ok()
            .filter(|r| 2 * r + 1 < info.len())
            .ok_or_else(|| Error::invalid("index out of range"))?;
        let (start, count) = (i64::from(info[2 * r]), i64::from(info[2 * r + 1]));
        if count < 0 || start < 0 || start + count > nvalues {
            return Err(Error::invalid("Invalid sloping incidence rows"));
        }
        for k in start..start + count {
            out.push(i64::from(values[2 * k as usize]));
        }
    }
    Ok(out)
}

fn unique(mut v: Vec<i64>) -> Vec<i64> {
    v.sort_unstable();
    v.dedup();
    v
}

/// `staged_cell_subsets(mesh_files, owner_cells)`: per mesh, the ascending
/// cells whose WSE can reach an owner pixel (`ras-commander-staged-cells/1`).
pub fn staged_cell_subsets(meshes: &[MeshPayload], owners: &[[i64; 2]]) -> Result<Vec<Vec<u32>>> {
    let mut subsets = Vec::new();
    for (index, mesh) in meshes.iter().enumerate() {
        let t = &mesh.topology;
        let cells = unique(
            owners
                .iter()
                .filter(|o| o[0] == index as i64)
                .map(|o| o[1])
                .collect(),
        );
        if cells.is_empty() {
            subsets.push(vec![0u32]);
            continue;
        }
        let ncells = (t.cell_info.len() / 2) as i64;
        if cells[0] < 0 || *cells.last().unwrap() >= ncells {
            return Err(Error::invalid("Owner cell outside sloping mesh"));
        }
        let faces = gather(&t.cell_info, &t.cell_faces, &cells)?;
        let mut vertices = Vec::with_capacity(faces.len() * 2);
        for &f in &faces {
            let f = usize::try_from(f)
                .ok()
                .filter(|f| 2 * f + 1 < t.face_vertices.len())
                .ok_or_else(|| Error::invalid("face index out of range"))?;
            vertices.push(i64::from(t.face_vertices[2 * f]));
            vertices.push(i64::from(t.face_vertices[2 * f + 1]));
        }
        let vertices = unique(vertices);
        let ring = gather(&t.vertex_info, &t.vertex_faces, &vertices)?;
        let mut all_faces = faces;
        all_faces.extend(ring);
        let all_faces = unique(all_faces);
        let mut closure = cells.clone();
        for f in all_faces {
            let f = usize::try_from(f)
                .ok()
                .filter(|f| 2 * f + 1 < t.face_cells.len())
                .ok_or_else(|| Error::invalid("face index out of range"))?;
            closure.push(i64::from(t.face_cells[2 * f]));
            closure.push(i64::from(t.face_cells[2 * f + 1]));
        }
        let closure = unique(closure);
        if closure[0] < 0
            || *closure.last().unwrap() >= ncells
            || *closure.last().unwrap() >= 1 << 32
        {
            return Err(Error::invalid("Sloping face cell outside mesh"));
        }
        subsets.push(closure.into_iter().map(|c| c as u32).collect());
    }
    Ok(subsets)
}

/// `write_staged_cells(root, subsets)`: one `<u4` file per mesh.
pub fn write_staged_cells(root: &Path, subsets: &[Vec<u32>]) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for (index, cells) in subsets.iter().enumerate() {
        let path = root.join(format!("staged-cells-{index}.bin"));
        let bytes: Vec<u8> = cells.iter().flat_map(|c| c.to_le_bytes()).collect();
        write_all(&path, &bytes)?;
        paths.push(path);
    }
    Ok(paths)
}

/// One staged profile: its name and the files (one per mesh).
pub struct Unit {
    pub id: String,
    pub values_files: Vec<PathBuf>,
}

/// Ceiling of one Water Surface read (values), as `_profile_block`'s bounded slabs.
const READ_BLOCK_BYTES: usize = 64 << 20;

/// Column ranges `[lo, hi)` covering ascending `cells`, split where two staged
/// cells are more than one slim tile width (16,384 cells) apart.
fn column_clusters(cells: &[u32]) -> Vec<(u64, u64)> {
    let mut out: Vec<(u64, u64)> = Vec::new();
    for &c in cells {
        let c = u64::from(c);
        match out.last_mut() {
            Some((_, hi)) if c < *hi + 16_384 => *hi = c + 1,
            _ => out.push((c, c + 1)),
        }
    }
    out
}

/// `_stage_profiles(hdf, root, meshes, {}, selected, deduplicate=True,
/// max_staged_bytes, cell_subsets)`: each file holds the subset cells' float32
/// values in ascending cell order, named by the selector's position in
/// `selected`. Reads are ascending time blocks of at most 64 MiB.
///
/// `complete_rows=true` is the HDF path: every complete source row is read and
/// must be finite. `complete_rows=false` follows fim-commander's staged-cell
/// path (`_stage_tile_profiles`, engineer decision 2026-11-13): only the column
/// ranges holding staged cells are read, and only staged values must be finite.
/// The staged bytes are the same either way. Returns (units, views, staged bytes).
#[allow(clippy::too_many_arguments)] // Mirrors `_stage_profiles` plus the staging mode.
pub fn stage_profiles(
    hdf: &Hdf,
    root: &Path,
    meshes: &[MeshPayload],
    selected: &[(String, u64)],
    subsets: &[Vec<u32>],
    max_staged_bytes: i64,
    time_counts: &[u64],
    complete_rows: bool,
) -> Result<(Vec<Unit>, usize, i64)> {
    // Commander's guard: an unsorted, repeated or out-of-range subset must never
    // stage unset values (it also refuses a mesh without cells).
    if subsets.len() != meshes.len()
        || subsets.iter().zip(meshes).any(|(c, m)| {
            c.is_empty()
                || c.last().is_some_and(|&last| last as usize >= m.cell_count)
                || c.windows(2).any(|w| w[1] <= w[0])
        })
    {
        return Err(Error::invalid(
            "cell_subsets must be ascending in-range cells, one array per mesh",
        ));
    }
    // First occurrence of each selector (deduplicate=True).
    let mut first: std::collections::BTreeMap<u64, usize> = Default::default();
    for (k, (_, s)) in selected.iter().enumerate() {
        first.entry(*s).or_insert(k);
    }
    // Admission in profile order, as Python checks before each file.
    let mut staged: i64 = 0;
    for (k, (_, s)) in selected.iter().enumerate() {
        if first[s] != k {
            continue;
        }
        for subset in subsets {
            if staged + subset.len() as i64 * 4 > max_staged_bytes {
                return Err(Error::unsupported(
                    "Prepared sloping inputs exceed max_prepared_mib",
                ));
            }
            staged += subset.len() as i64 * 4;
        }
    }
    let mut bad: Option<(usize, usize)> = None;
    for (i, mesh) in meshes.iter().enumerate() {
        let path = Hdf::water_surface_path(&mesh.name);
        if let Some((&t, _)) = first.iter().next_back()
            && t >= time_counts[i]
        {
            return Err(Error::invalid(format!(
                "time index {t} is outside the saved times of {}",
                crate::error::py_repr(&mesh.name)
            )));
        }
        let times: Vec<u64> = first.keys().copied().collect();
        let subset = &subsets[i];
        let clusters = if complete_rows {
            vec![(0u64, mesh.cell_count as u64)]
        } else {
            column_clusters(subset)
        };
        let width: u64 = clusters.iter().map(|(lo, hi)| hi - lo).sum();
        let per = (READ_BLOCK_BYTES as u64 / (width * 4).max(1)).max(1);
        let mut k = 0;
        while k < times.len() {
            let start = times[k];
            let mut stop = start + 1;
            let mut j = k + 1;
            while j < times.len() && times[j] == stop && stop - start < per {
                stop += 1;
                j += 1;
            }
            let rows = (stop - start) as usize;
            let mut blocks = Vec::with_capacity(clusters.len());
            for &(lo, hi) in &clusters {
                let block = hdf.slab_f32(&path, start, stop, lo, hi)?;
                if block.len() != rows * (hi - lo) as usize {
                    return Err(Error::invalid("Sloping profile source dimensions changed"));
                }
                blocks.push(block);
            }
            for n in 0..rows {
                let profile = first[&(start + n as u64)];
                let mut values = Vec::with_capacity(subset.len());
                let mut cluster = 0;
                for &c in subset {
                    let c = u64::from(c);
                    while clusters[cluster].1 <= c {
                        cluster += 1;
                    }
                    let (lo, hi) = clusters[cluster];
                    values.push(blocks[cluster][n * (hi - lo) as usize + (c - lo) as usize]);
                }
                let finite = if complete_rows {
                    let w = mesh.cell_count;
                    blocks[0][n * w..(n + 1) * w].iter().all(|v| v.is_finite())
                } else {
                    values.iter().all(|v| v.is_finite())
                };
                if !finite {
                    if bad.is_none_or(|b| (profile, i) < b) {
                        bad = Some((profile, i));
                    }
                    continue;
                }
                let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
                write_all(&root.join(format!("values-{profile}-{i}.bin")), &bytes)?;
            }
            k = j;
        }
    }
    if let Some((profile, i)) = bad {
        return Err(Error::invalid(format!(
            "Invalid complete cell profile {} for {}",
            crate::error::py_repr(&selected[profile].0),
            crate::error::py_repr(&meshes[i].name)
        )));
    }
    let units = selected
        .iter()
        .map(|(name, s)| Unit {
            id: name.clone(),
            values_files: (0..meshes.len())
                .map(|i| root.join(format!("values-{}-{i}.bin", first[s])))
                .collect(),
        })
        .collect();
    let views = if first.is_empty() { 0 } else { meshes.len() };
    Ok((units, views, staged))
}

/// The parent context's complete profile (`prepare_sloping_model` stages the
/// cohort's first selector from the HDF before any subset): every value of that
/// saved time, in every mesh, must be finite.
pub fn check_parent_profile(
    hdf: &Hdf,
    meshes: &[MeshPayload],
    name: &str,
    selector: u64,
    time_counts: &[u64],
) -> Result<()> {
    for (i, mesh) in meshes.iter().enumerate() {
        if selector >= time_counts[i] {
            return Err(Error::invalid(format!(
                "time index {selector} is outside the saved times of {}",
                crate::error::py_repr(&mesh.name)
            )));
        }
        let row = hdf.rows_f32(
            &Hdf::water_surface_path(&mesh.name),
            selector,
            selector + 1,
            mesh.cell_count as u64,
        )?;
        if row.len() != mesh.cell_count {
            return Err(Error::invalid("Sloping profile source dimensions changed"));
        }
        if !row.iter().all(|v| v.is_finite()) {
            return Err(Error::invalid(format!(
                "Invalid complete cell profile {} for {}",
                crate::error::py_repr(name),
                crate::error::py_repr(&mesh.name)
            )));
        }
    }
    Ok(())
}
