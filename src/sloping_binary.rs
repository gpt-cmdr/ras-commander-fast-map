//! Typed geometry handoff; JSON contains only small array descriptors.
use crate::{
    MapError,
    sloping_session::{Geometry, InputCell, InputFace, InputVertex},
};
use serde::Deserialize;
use std::{collections::HashMap, fs, path::Path};

const SCHEMA: &str = "ras-commander-sloping-mesh/1";
const FIELDS: [(&str, usize); 12] = [
    ("centers", 16),
    ("cell_info", 8),
    ("cell_faces", 8),
    ("cell_minimum", 4),
    ("face_vertices", 8),
    ("face_cells", 8),
    ("face_info", 8),
    ("face_points", 16),
    ("face_minimum", 4),
    ("vertices", 16),
    ("vertex_info", 8),
    ("vertex_faces", 8),
];
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Array {
    path: String,
    rows: usize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    schema: String,
    arrays: HashMap<String, Array>,
}
fn invalid(message: &str) -> MapError {
    MapError::Invalid(message.into())
}
fn header(path: &Path) -> Result<Header, MapError> {
    if fs::metadata(path)?.len() > 65536 {
        return Err(invalid("sloping binary header too large"));
    }
    let value: Header = serde_json::from_slice(&fs::read(path)?)
        .map_err(|e| invalid(&format!("sloping binary header: {e}")))?;
    if value.schema != SCHEMA
        || value
            .arrays
            .keys()
            .any(|key| key != "vertex_elevations" && !FIELDS.iter().any(|(name, _)| name == key))
        || FIELDS
            .iter()
            .any(|(key, _)| !value.arrays.contains_key(*key))
    {
        return Err(invalid("invalid sloping binary schema or array fields"));
    }
    Ok(value)
}
fn check(h: &Header, base: &Path) -> Result<u64, MapError> {
    let mut total = 0_u64;
    for (key, width) in FIELDS.into_iter().chain(
        h.arrays
            .contains_key("vertex_elevations")
            .then_some(("vertex_elevations", 4)),
    ) {
        let array = &h.arrays[key];
        let bytes = array
            .rows
            .checked_mul(width)
            .filter(|&n| n <= 16_usize << 30)
            .ok_or_else(|| invalid("sloping binary array too large"))? as u64;
        if fs::metadata(base.join(&array.path))?.len() != bytes {
            return Err(invalid("sloping binary array length mismatch"));
        }
        total = total
            .checked_add(bytes)
            .ok_or_else(|| invalid("sloping binary size overflow"))?;
    }
    Ok(total)
}
pub fn input_bytes(path: &Path) -> Result<u64, MapError> {
    let h = header(path)?;
    check(
        &h,
        path.parent()
            .ok_or_else(|| invalid("sloping mesh has no parent"))?,
    )
}
pub fn load(path: &Path) -> Result<Geometry, MapError> {
    let h = header(path)?;
    let base = path
        .parent()
        .ok_or_else(|| invalid("sloping mesh has no parent"))?;
    check(&h, base)?;
    let raw = |key: &str| -> Result<Vec<u8>, MapError> {
        let value = fs::read(base.join(&h.arrays[key].path))?;
        let width = if key == "vertex_elevations" {
            4
        } else {
            FIELDS.iter().find(|(name, _)| *name == key).unwrap().1
        };
        if value.len() != h.arrays[key].rows * width {
            return Err(invalid("sloping binary array changed size"));
        }
        Ok(value)
    };
    let pairs = |key: &str| -> Result<Vec<[f64; 2]>, MapError> {
        Ok(raw(key)?
            .as_chunks::<16>()
            .0
            .iter()
            .map(|p| {
                [
                    f64::from_le_bytes(p[..8].try_into().unwrap()),
                    f64::from_le_bytes(p[8..].try_into().unwrap()),
                ]
            })
            .collect())
    };
    let integers = |key: &str| -> Result<Vec<[usize; 2]>, MapError> {
        raw(key)?
            .as_chunks::<8>()
            .0
            .iter()
            .map(|p| {
                let a = i32::from_le_bytes(p[..4].try_into().unwrap());
                let b = i32::from_le_bytes(p[4..].try_into().unwrap());
                // Orientation columns may be negative. Incidence uses only column0;
                // offsets/counts and face address pairs require nonnegative values below.
                Ok([a as usize, b as usize])
            })
            .collect()
    };
    let floats = |key: &str| -> Result<Vec<f32>, MapError> {
        Ok(raw(key)?
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| f32::from_le_bytes(*p))
            .collect())
    };
    let incidence =
        |info: Vec<[usize; 2]>, values: Vec<[usize; 2]>| -> Result<Vec<Vec<usize>>, MapError> {
            let mut expected = 0_usize;
            let mut result = Vec::with_capacity(info.len());
            for [start, count] in info {
                if start != expected || start.checked_add(count).is_none_or(|v| v > values.len()) {
                    return Err(invalid("invalid ordered sloping incidence"));
                }
                result.push(values[start..start + count].iter().map(|v| v[0]).collect());
                expected += count;
            }
            if expected != values.len() {
                return Err(invalid("unreferenced sloping incidence"));
            }
            Ok(result)
        };
    let centers = pairs("centers")?;
    let cell_faces = incidence(integers("cell_info")?, integers("cell_faces")?)?;
    let minima = floats("cell_minimum")?;
    if centers.len() != cell_faces.len()
        || ![
            centers.len(),
            cell_faces.iter().filter(|v| v.len() > 1).count(),
        ]
        .contains(&minima.len())
    {
        return Err(invalid("sloping binary cell dimensions mismatch"));
    }
    let mut physical_minima = minima.into_iter();
    let cells = centers
        .into_iter()
        .zip(cell_faces)
        .map(|(point, faces)| {
            let minimum_elevation = if faces.len() > 1 {
                physical_minima.next().unwrap()
            } else {
                0.0
            };
            InputCell {
                point,
                minimum_elevation,
                virtual_cell: faces.len() == 1,
                faces,
            }
        })
        .collect::<Vec<_>>();
    let coordinates = pairs("vertices")?;
    let vertex_faces = incidence(integers("vertex_info")?, integers("vertex_faces")?)?;
    if coordinates.len() != vertex_faces.len() {
        return Err(invalid("sloping vertex dimensions mismatch"));
    }
    let vertices = coordinates
        .into_iter()
        .zip(vertex_faces)
        .map(|(point, faces)| {
            let n = faces.len();
            InputVertex {
                point,
                ccw: (0..n).map(|i| (i + 1) % n).collect(),
                cw: (0..n).map(|i| (i + n - 1) % n).collect(),
                faces,
            }
        })
        .collect::<Vec<_>>();
    let face_cells = integers("face_cells")?;
    let face_vertices = integers("face_vertices")?;
    let face_info = integers("face_info")?;
    let face_minima = floats("face_minimum")?;
    let interior = pairs("face_points")?;
    let count = face_cells.len();
    if [face_vertices.len(), face_info.len(), face_minima.len()]
        .iter()
        .any(|&n| n != count)
    {
        return Err(invalid("sloping face dimensions mismatch"));
    }
    let mut faces = Vec::with_capacity(count);
    for (((pair, ends), [start, length]), minimum_elevation) in face_cells
        .into_iter()
        .zip(face_vertices)
        .zip(face_info)
        .zip(face_minima)
    {
        if pair.iter().any(|&v| v >= cells.len())
            || ends.iter().any(|&v| v >= vertices.len())
            || start.checked_add(length).is_none_or(|v| v > interior.len())
        {
            return Err(invalid(
                "sloping face index or polyline range out of bounds",
            ));
        }
        let mut polyline = Vec::with_capacity(length + 2);
        polyline.push(vertices[ends[0]].point);
        polyline.extend_from_slice(&interior[start..start + length]);
        polyline.push(vertices[ends[1]].point);
        faces.push(InputFace {
            cells: pair,
            vertices: ends,
            minimum_elevation,
            perimeter: cells[pair[1]].virtual_cell,
            polyline,
        });
    }
    let vertex_elevations = if h.arrays.contains_key("vertex_elevations") {
        Some(floats("vertex_elevations")?)
    } else {
        None
    };
    Ok(Geometry {
        vertex_elevations,
        cells,
        faces,
        vertices,
    })
}

/// Staged-cell subset: sorted unique little-endian u32 global cell ids per mesh.
pub const STAGED_METHOD: &str = "ras-commander-staged-cells/1";

/// Compact geometry for a staged cell subset. Cells are the staged cells in
/// staged (ascending global) order; faces are every face incident to a vertex
/// of an owner cell, ascending by global id; corners are those faces'
/// endpoints, ascending by global id. Only owner-cell vertices keep their
/// complete face rings, so every corner value an owner stencil reads is the
/// complete-mesh value. Other corners carry empty rings and are never read.
pub(crate) struct Compact {
    pub mesh: crate::sloping::Mesh,
    pub stencils: Vec<Option<crate::sloping::CellStencil>>,
    pub staged: Vec<u32>,
    /// Bytes these rows occupy in the binary mesh format: the planning
    /// analogue of [`input_bytes`] for the complete mesh.
    pub equivalent_bytes: u64,
    pub parent: [usize; 3],
}

/// Admission for a staged load. Every allocation is charged before it is
/// made, from sizes known from the info rows; complete mesh arrays are
/// released when dropped, everything else stays charged (intermediates are
/// counted as if retained, so the charge bounds the load's heap). One budget
/// spans every mesh of a request.
pub(crate) struct LoadBudget {
    limit: u64,
    used: std::cell::Cell<u64>,
    peak: std::cell::Cell<u64>,
}

impl LoadBudget {
    pub(crate) fn new(limit: u64) -> Self {
        Self {
            limit,
            used: 0.into(),
            peak: 0.into(),
        }
    }
    pub(crate) fn charge(&self, bytes: u64, what: &str) -> Result<(), MapError> {
        let used = self
            .used
            .get()
            .checked_add(bytes)
            .ok_or_else(|| MapError::Budget("staged geometry load overflow".into()))?;
        if used > self.limit {
            return Err(MapError::Budget(format!(
                "staged geometry load ({what}): required {used} bytes, allowed {} bytes",
                self.limit
            )));
        }
        self.used.set(used);
        self.peak.set(self.peak.get().max(used));
        Ok(())
    }
    fn release(&self, bytes: u64) {
        self.used.set(self.used.get().saturating_sub(bytes));
    }
    /// Bytes still charged: the loaded compact geometry plus intermediates.
    pub(crate) fn used(&self) -> u64 {
        self.used.get()
    }
    /// Largest charge reached while loading.
    pub(crate) fn peak(&self) -> u64 {
        self.peak.get()
    }
}

/// One complete mesh array, charged while it is held.
struct Charged<'a> {
    bytes: Vec<u8>,
    budget: &'a LoadBudget,
}
impl Drop for Charged<'_> {
    fn drop(&mut self) {
        self.budget.release(self.bytes.len() as u64);
    }
}
impl std::ops::Deref for Charged<'_> {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.bytes
    }
}

pub(crate) fn read_staged(path: &Path, budget: &LoadBudget) -> Result<Vec<u32>, MapError> {
    let length = fs::metadata(path)?.len();
    if length == 0 || length % 4 != 0 || length > 16 << 30 {
        return Err(invalid("staged cell file must hold a nonempty <u4 array"));
    }
    // The raw bytes and the decoded list are held together briefly.
    budget.charge(length * 2, "staged cell list")?;
    let raw = fs::read(path)?;
    if raw.len() as u64 != length {
        return Err(invalid("staged cell file changed size"));
    }
    let cells: Vec<u32> = raw
        .as_chunks::<4>()
        .0
        .iter()
        .map(|v| u32::from_le_bytes(*v))
        .collect();
    drop(raw);
    budget.release(length);
    if cells.windows(2).any(|w| w[0] >= w[1]) {
        return Err(invalid("staged cells must be strictly ascending"));
    }
    Ok(cells)
}

fn i32_pair(raw: &[u8], row: usize) -> [usize; 2] {
    let p = &raw[row * 8..row * 8 + 8];
    [
        i32::from_le_bytes(p[..4].try_into().unwrap()) as usize,
        i32::from_le_bytes(p[4..].try_into().unwrap()) as usize,
    ]
}
fn f64_pair(raw: &[u8], row: usize) -> [f64; 2] {
    let p = &raw[row * 16..row * 16 + 16];
    [
        f64::from_le_bytes(p[..8].try_into().unwrap()),
        f64::from_le_bytes(p[8..].try_into().unwrap()),
    ]
}
fn f32_at(raw: &[u8], row: usize) -> f32 {
    f32::from_le_bytes(raw[row * 4..row * 4 + 4].try_into().unwrap())
}
/// The ordered-incidence rule of [`load`], without materialising rows.
fn check_incidence(info: &[u8], values: usize) -> Result<(), MapError> {
    let mut expected = 0_usize;
    for row in 0..info.len() / 8 {
        let [start, count] = i32_pair(info, row);
        if start != expected || start.checked_add(count).is_none_or(|v| v > values) {
            return Err(invalid("invalid ordered sloping incidence"));
        }
        expected += count;
    }
    if expected != values {
        return Err(invalid("unreferenced sloping incidence"));
    }
    Ok(())
}
fn position(sorted: &[usize], value: usize) -> Result<usize, MapError> {
    sorted
        .binary_search(&value)
        .map_err(|_| invalid("staged sloping closure index missing"))
}
/// Collect fallible items into a vector allocated at its known length once:
/// `collect::<Result<Vec<_>, _>>()` cannot see the length and grows by
/// doubling, which can exceed the charged size.
fn exact<T>(
    len: usize,
    items: impl Iterator<Item = Result<T, MapError>>,
) -> Result<Vec<T>, MapError> {
    let mut out = Vec::with_capacity(len);
    for item in items {
        out.push(item?);
    }
    Ok(out)
}

fn bytes<T>(count: usize) -> u64 {
    (count as u64).saturating_mul(std::mem::size_of::<T>() as u64)
}

/// Load the stencil closure of `owners` (sorted unique global cells of this
/// mesh) from a complete binary mesh under `budget`. Refuses unless `staged`
/// contains every cell of every face incident to an owner vertex. Applies the
/// complete loader's dimension checks, so malformed input is an error, never
/// a panic.
pub(crate) fn load_compact(
    path: &Path,
    staged: Vec<u32>,
    owners: &[u32],
    budget: &LoadBudget,
) -> Result<Compact, MapError> {
    use crate::sloping::{self, Cell, CellStencil, Corner, Face, Mesh};
    let h = header(path)?;
    let base = path
        .parent()
        .ok_or_else(|| invalid("sloping mesh has no parent"))?;
    check(&h, base)?;
    let rows = |key: &str| h.arrays[key].rows;
    let raw = |key: &str| -> Result<Charged<'_>, MapError> {
        let width = if key == "vertex_elevations" {
            4
        } else {
            FIELDS.iter().find(|(name, _)| *name == key).unwrap().1
        };
        let expected = h.arrays[key].rows * width;
        budget.charge(expected as u64, key)?;
        let value = match fs::read(base.join(&h.arrays[key].path)) {
            Ok(value) if value.len() == expected => value,
            Ok(_) => {
                budget.release(expected as u64);
                return Err(invalid("sloping binary array changed size"));
            }
            Err(e) => {
                budget.release(expected as u64);
                return Err(e.into());
            }
        };
        Ok(Charged {
            bytes: value,
            budget,
        })
    };
    let n_cells = rows("centers");
    let n_faces = rows("face_cells");
    let n_vertices = rows("vertices");
    // The complete loader's dimension rules, before any indexed read.
    if rows("cell_info") != n_cells || rows("vertex_info") != n_vertices {
        return Err(invalid("sloping binary cell dimensions mismatch"));
    }
    if [
        rows("face_vertices"),
        rows("face_info"),
        rows("face_minimum"),
    ]
    .iter()
    .any(|&n| n != n_faces)
    {
        return Err(invalid("sloping face dimensions mismatch"));
    }
    if h.arrays
        .get("vertex_elevations")
        .is_some_and(|a| a.rows != n_vertices)
    {
        return Err(MapError::Invalid(
            "sloping corner elevation dimensions or values invalid".into(),
        ));
    }
    if staged.last().is_some_and(|&c| c as usize >= n_cells) {
        return Err(invalid("staged cell out of range"));
    }
    if owners.windows(2).any(|w| w[0] >= w[1]) {
        return Err(invalid("owner cells must be sorted and unique per mesh"));
    }
    let s = staged.len();
    // staged_usize, counts, ranks, cell_points and cells, plus owner positions.
    budget.charge(
        bytes::<usize>(s) * 3
            + bytes::<[f64; 2]>(s)
            + bytes::<Cell>(s)
            + bytes::<usize>(owners.len()) * 3,
        "staged cell rows",
    )?;
    let staged_usize: Vec<usize> = staged.iter().map(|&c| c as usize).collect();
    let owner_local = exact(
        owners.len(),
        owners.iter().map(|&c| {
            staged_usize
                .binary_search(&(c as usize))
                .map_err(|_| invalid("staged cells must include every owner cell"))
        }),
    )?;

    // Cells: face counts (virtual flag), physical rank (minimum row) and owner rows.
    let info = raw("cell_info")?;
    check_incidence(&info, rows("cell_faces"))?;
    let mut counts = Vec::with_capacity(s);
    let mut ranks = Vec::with_capacity(s);
    let mut physical = 0_usize;
    let mut next = 0;
    for cell in 0..n_cells {
        let [_, count] = i32_pair(&info, cell);
        if next < s && staged_usize[next] == cell {
            counts.push(count);
            ranks.push(physical);
            next += 1;
        }
        if count > 1 {
            physical += 1;
        }
    }
    let owner_rows: Vec<[usize; 2]> = owners
        .iter()
        .map(|&c| i32_pair(&info, c as usize))
        .collect();
    drop(info);
    if ![n_cells, physical].contains(&rows("cell_minimum")) {
        return Err(invalid("sloping binary cell dimensions mismatch"));
    }
    let owner_entries: usize = owner_rows.iter().map(|r| r[1]).sum();
    // owner_faces (Vec<Vec>) and the ring-vertex list (two per entry).
    budget.charge(
        bytes::<Vec<usize>>(owners.len()) + bytes::<usize>(owner_entries) * 3,
        "owner faces",
    )?;
    let cell_faces = raw("cell_faces")?;
    let owner_faces: Vec<Vec<usize>> = owner_rows
        .iter()
        .map(|&[start, count]| {
            (start..start + count)
                .map(|row| i32_pair(&cell_faces, row)[0])
                .collect()
        })
        .collect();
    drop(cell_faces);
    let face_vertices = raw("face_vertices")?;
    let mut ring_vertices = Vec::with_capacity(owner_entries * 2);
    for &face in owner_faces.iter().flatten() {
        if face >= n_faces {
            return Err(invalid("cell face out of range"));
        }
        ring_vertices.extend(i32_pair(&face_vertices, face));
    }
    drop(face_vertices);
    ring_vertices.sort_unstable();
    ring_vertices.dedup();
    if ring_vertices.last().is_some_and(|&v| v >= n_vertices) {
        return Err(invalid(
            "sloping face index or polyline range out of bounds",
        ));
    }

    // Complete face rings of owner vertices define the compact faces.
    let info = raw("vertex_info")?;
    check_incidence(&info, rows("vertex_faces"))?;
    let ring_rows: Vec<[usize; 2]> = ring_vertices.iter().map(|&v| i32_pair(&info, v)).collect();
    drop(info);
    let ring_entries: usize = ring_rows.iter().map(|r| r[1]).sum();
    let face_bound = ring_entries + owner_entries;
    // ring_rows, rings (Vec<Vec>), the face list, and per face: ends, pairs,
    // local pairs, spans, minima, corner ids (2 per face) and the final Face.
    budget.charge(
        bytes::<[usize; 2]>(ring_vertices.len())
            + bytes::<Vec<usize>>(ring_vertices.len())
            + bytes::<usize>(ring_entries)
            + bytes::<usize>(face_bound)
            + face_bound as u64
                * (bytes::<[usize; 2]>(1) * 4 + 4 + bytes::<usize>(2) + bytes::<Face>(1)),
        "closure faces",
    )?;
    let vertex_faces = raw("vertex_faces")?;
    let rings: Vec<Vec<usize>> = ring_rows
        .iter()
        .map(|&[start, count]| {
            (start..start + count)
                .map(|row| i32_pair(&vertex_faces, row)[0])
                .collect()
        })
        .collect();
    drop(vertex_faces);
    let mut faces: Vec<usize> = Vec::with_capacity(face_bound);
    faces.extend(rings.iter().flatten().copied());
    faces.extend(owner_faces.iter().flatten().copied());
    faces.sort_unstable();
    faces.dedup();
    if faces.last().is_some_and(|&f| f >= n_faces) {
        return Err(invalid("sloping corner face out of range"));
    }
    let f = faces.len();
    let face_vertices = raw("face_vertices")?;
    let ends: Vec<[usize; 2]> = faces.iter().map(|&f| i32_pair(&face_vertices, f)).collect();
    drop(face_vertices);
    let mut corners: Vec<usize> = Vec::with_capacity(2 * ends.len());
    corners.extend(ends.iter().flatten().copied());
    corners.sort_unstable();
    corners.dedup();
    if corners.last().is_some_and(|&v| v >= n_vertices) {
        return Err(invalid(
            "sloping face index or polyline range out of bounds",
        ));
    }
    let w = corners.len();
    let face_cells = raw("face_cells")?;
    let pairs: Vec<[usize; 2]> = faces.iter().map(|&f| i32_pair(&face_cells, f)).collect();
    drop(face_cells);
    let local_pairs = exact(
        pairs.len(),
        pairs.iter().map(|pair| -> Result<[usize; 2], MapError> {
            if pair.iter().any(|&c| c >= n_cells) {
                return Err(invalid(
                    "sloping face index or polyline range out of bounds",
                ));
            }
            let mut local = [0; 2];
            for (slot, &c) in local.iter_mut().zip(pair) {
                *slot = staged_usize.binary_search(&c).map_err(|_| {
                    invalid("staged cells do not cover the sloping stencil closure")
                })?;
            }
            Ok(local)
        }),
    )?;
    let face_info = raw("face_info")?;
    let spans: Vec<[usize; 2]> = faces.iter().map(|&f| i32_pair(&face_info, f)).collect();
    drop(face_info);
    let interior_rows = rows("face_points");
    let mut interior = 0_usize;
    for &[start, length] in &spans {
        if start.checked_add(length).is_none_or(|v| v > interior_rows) {
            return Err(invalid(
                "sloping face index or polyline range out of bounds",
            ));
        }
        interior += length;
    }
    let face_minimum = raw("face_minimum")?;
    let face_minima: Vec<f32> = faces.iter().map(|&f| f32_at(&face_minimum, f)).collect();
    drop(face_minimum);
    // Corners (points, elevations and Corner rows with their rings) and
    // polylines (headers and points, endpoints included).
    let elevations = h.arrays.contains_key("vertex_elevations");
    budget.charge(
        bytes::<[f64; 2]>(w)
            + if elevations { bytes::<f32>(w) } else { 0 }
            + bytes::<Corner>(w)
            + bytes::<usize>(ring_entries) * 3
            + bytes::<Vec<[f64; 2]>>(f)
            + bytes::<[f64; 2]>(interior + 2 * f),
        "closure corners and polylines",
    )?;
    let vertices = raw("vertices")?;
    let points: Vec<[f64; 2]> = corners.iter().map(|&v| f64_pair(&vertices, v)).collect();
    drop(vertices);
    let face_points = raw("face_points")?;
    let polylines = exact(
        ends.len(),
        ends.iter().zip(&spans).map(
            |(ends, &[start, length])| -> Result<Vec<[f64; 2]>, MapError> {
                let mut polyline = Vec::with_capacity(length + 2);
                polyline.push(points[position(&corners, ends[0])?]);
                polyline.extend((start..start + length).map(|row| f64_pair(&face_points, row)));
                polyline.push(points[position(&corners, ends[1])?]);
                Ok(polyline)
            },
        ),
    )?;
    drop(face_points);
    let centers = raw("centers")?;
    let cell_points: Vec<[f64; 2]> = staged_usize
        .iter()
        .map(|&c| f64_pair(&centers, c))
        .collect();
    drop(centers);
    let cell_minimum = raw("cell_minimum")?;
    // The complete loader assigns minima to physical cells in order (rank),
    // whichever of the two admitted layouts the array uses.
    let cells: Vec<Cell> = counts
        .iter()
        .zip(&ranks)
        .map(|(&count, &rank)| Cell {
            minimum: if count > 1 {
                f32_at(&cell_minimum, rank)
            } else {
                0.0
            },
            virtual_cell: count == 1,
        })
        .collect();
    drop(cell_minimum);
    let vertex_elevations = if elevations {
        let all = raw("vertex_elevations")?;
        Some(corners.iter().map(|&v| f32_at(&all, v)).collect::<Vec<_>>())
    } else {
        None
    };

    let compact_faces = exact(
        local_pairs.len(),
        local_pairs
            .iter()
            .zip(&ends)
            .zip(&face_minima)
            .zip(&polylines)
            .map(|(((&cells_pair, ends), &minimum), polyline)| {
                Ok(Face {
                    cells: cells_pair,
                    vertices: [position(&corners, ends[0])?, position(&corners, ends[1])?],
                    minimum,
                    perimeter: cells[cells_pair[1]].virtual_cell,
                    application: sloping::face_application(
                        polyline,
                        [cell_points[cells_pair[0]], cell_points[cells_pair[1]]],
                    )?,
                })
            }),
    )?;
    let mut compact_corners: Vec<Corner> = points
        .iter()
        .map(|&point| Corner {
            point,
            faces: Vec::new(),
            ccw: Vec::new(),
            cw: Vec::new(),
        })
        .collect();
    for (&vertex, ring) in ring_vertices.iter().zip(&rings) {
        let n = ring.len();
        let corner = &mut compact_corners[position(&corners, vertex)?];
        corner.faces = exact(n, ring.iter().map(|&f| position(&faces, f)))?;
        corner.ccw = (0..n).map(|i| (i + 1) % n).collect();
        corner.cw = (0..n).map(|i| (i + n - 1) % n).collect();
    }
    let mesh = Mesh {
        cells,
        faces: compact_faces,
        corners: compact_corners,
    };
    // A mesh without owner cells keeps only its staged cells; nothing reads it.
    if !owners.is_empty() {
        mesh.validate()?;
    }
    // Stencils: one slot per staged cell; per owner its polygon, corner
    // products, corner/face slots, elevations and a physical ring that may
    // grow to twice its points while extending.
    let ring_points: usize = owner_faces
        .iter()
        .flatten()
        .map(|&g| faces.binary_search(&g).map_or(0, |i| polylines[i].len()))
        .sum();
    budget.charge(
        bytes::<Option<CellStencil>>(s)
            + owner_entries as u64
                * (bytes::<[f64; 2]>(1) + 8 + bytes::<[usize; 2]>(2) + 4 + bytes::<usize>(1))
            + bytes::<[f64; 2]>(ring_points) * 2,
        "owner stencils",
    )?;
    let mut stencils: Vec<Option<CellStencil>> = (0..s).map(|_| None).collect();
    for (&local, global_faces) in owner_local.iter().zip(&owner_faces) {
        if mesh.cells[local].virtual_cell {
            continue;
        }
        let ring = exact(
            global_faces.len(),
            global_faces.iter().map(|&f| position(&faces, f)),
        )?;
        let mut stencil = sloping::prepare_stencil(&mesh, local, &ring)?;
        sloping::set_physical_ring(
            &mut stencil,
            local,
            cell_points[local],
            ring.iter()
                .map(|&f| (mesh.faces[f].cells, polylines[f].as_slice())),
        );
        stencils[local] = Some(stencil);
    }
    if let Some(elevations) = &vertex_elevations {
        sloping::set_vertex_elevations(&mut stencils, elevations, mesh.corners.len())?;
    }
    let (s, f, w) = (s as u64, f as u64, w as u64);
    let equivalent_bytes = s * (16 + 8 + 4)
        + owner_entries as u64 * 8
        + f * (8 + 8 + 8 + 4)
        + interior as u64 * 16
        + w * (16 + 8)
        + ring_entries as u64 * 8
        + if vertex_elevations.is_some() {
            w * 4
        } else {
            0
        };
    Ok(Compact {
        mesh,
        stencils,
        staged,
        equivalent_bytes,
        parent: [n_cells, n_faces, n_vertices],
    })
}
