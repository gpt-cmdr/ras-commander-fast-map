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

pub(crate) fn read_staged(path: &Path) -> Result<Vec<u32>, MapError> {
    let length = fs::metadata(path)?.len();
    if length == 0 || length % 4 != 0 || length > 16 << 30 {
        return Err(invalid("staged cell file must hold a nonempty <u4 array"));
    }
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
    if cells.windows(2).any(|w| w[0] >= w[1]) {
        return Err(invalid("staged cells must be strictly ascending"));
    }
    Ok(cells)
}

/// Largest single binary array of a mesh: the transient of [`load_compact`],
/// which holds one complete array at a time beside the compact rows.
pub(crate) fn compact_transient_bytes(path: &Path) -> Result<u64, MapError> {
    let h = header(path)?;
    let base = path
        .parent()
        .ok_or_else(|| invalid("sloping mesh has no parent"))?;
    check(&h, base)?;
    let mut largest = 0_u64;
    for (key, array) in &h.arrays {
        let width = if key == "vertex_elevations" {
            4
        } else {
            FIELDS.iter().find(|(name, _)| name == key).unwrap().1
        };
        largest = largest.max((array.rows * width) as u64);
    }
    Ok(largest)
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

/// Load the stencil closure of `owners` (sorted unique global cells of this
/// mesh) from a complete binary mesh. Refuses unless `staged` contains every
/// cell of every face incident to an owner vertex.
pub(crate) fn load_compact(
    path: &Path,
    staged: Vec<u32>,
    owners: &[usize],
) -> Result<Compact, MapError> {
    use crate::sloping::{self, Cell, Corner, Face, Mesh};
    let h = header(path)?;
    let base = path
        .parent()
        .ok_or_else(|| invalid("sloping mesh has no parent"))?;
    check(&h, base)?;
    let rows = |key: &str| h.arrays[key].rows;
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
    let n_cells = rows("centers");
    let n_faces = rows("face_cells");
    let n_vertices = rows("vertices");
    if rows("cell_info") != n_cells || rows("vertex_info") != n_vertices {
        return Err(invalid("sloping binary cell dimensions mismatch"));
    }
    if staged.last().is_some_and(|&c| c as usize >= n_cells) {
        return Err(invalid("staged cell out of range"));
    }
    let staged_usize: Vec<usize> = staged.iter().map(|&c| c as usize).collect();
    if owners.windows(2).any(|w| w[0] >= w[1]) {
        return Err(invalid("owner cells must be sorted and unique per mesh"));
    }
    let owner_local = owners
        .iter()
        .map(|&c| {
            staged_usize
                .binary_search(&c)
                .map_err(|_| invalid("staged cells must include every owner cell"))
        })
        .collect::<Result<Vec<_>, _>>()?;

    // Cells: face counts (virtual flag), physical rank (minimum row) and owner rows.
    let info = raw("cell_info")?;
    check_incidence(&info, rows("cell_faces"))?;
    let mut counts = Vec::with_capacity(staged.len());
    let mut ranks = Vec::with_capacity(staged.len());
    let mut physical = 0_usize;
    let mut next = 0;
    for cell in 0..n_cells {
        let [_, count] = i32_pair(&info, cell);
        if next < staged_usize.len() && staged_usize[next] == cell {
            counts.push(count);
            ranks.push(physical);
            next += 1;
        }
        if count > 1 {
            physical += 1;
        }
    }
    let owner_rows: Vec<[usize; 2]> = owners.iter().map(|&c| i32_pair(&info, c)).collect();
    drop(info);
    if ![n_cells, physical].contains(&rows("cell_minimum")) {
        return Err(invalid("sloping binary cell dimensions mismatch"));
    }
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
    let mut ring_vertices = Vec::new();
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
    let mut faces: Vec<usize> = rings.iter().flatten().copied().collect();
    faces.extend(owner_faces.iter().flatten().copied());
    faces.sort_unstable();
    faces.dedup();
    if faces.last().is_some_and(|&f| f >= n_faces) {
        return Err(invalid("sloping corner face out of range"));
    }
    let face_vertices = raw("face_vertices")?;
    let ends: Vec<[usize; 2]> = faces.iter().map(|&f| i32_pair(&face_vertices, f)).collect();
    drop(face_vertices);
    let mut corners: Vec<usize> = ends.iter().flatten().copied().collect();
    corners.sort_unstable();
    corners.dedup();
    if corners.last().is_some_and(|&v| v >= n_vertices) {
        return Err(invalid(
            "sloping face index or polyline range out of bounds",
        ));
    }
    let face_cells = raw("face_cells")?;
    let pairs: Vec<[usize; 2]> = faces.iter().map(|&f| i32_pair(&face_cells, f)).collect();
    drop(face_cells);
    let local_pairs = pairs
        .iter()
        .map(|pair| -> Result<[usize; 2], MapError> {
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
        })
        .collect::<Result<Vec<_>, _>>()?;
    let face_info = raw("face_info")?;
    let spans: Vec<[usize; 2]> = faces.iter().map(|&f| i32_pair(&face_info, f)).collect();
    drop(face_info);
    let face_minimum = raw("face_minimum")?;
    let face_minima: Vec<f32> = faces.iter().map(|&f| f32_at(&face_minimum, f)).collect();
    drop(face_minimum);
    let vertices = raw("vertices")?;
    let points: Vec<[f64; 2]> = corners.iter().map(|&v| f64_pair(&vertices, v)).collect();
    drop(vertices);
    let interior_rows = rows("face_points");
    let face_points = raw("face_points")?;
    let mut interior = 0_usize;
    let polylines = ends
        .iter()
        .zip(&spans)
        .map(
            |(ends, &[start, length])| -> Result<Vec<[f64; 2]>, MapError> {
                if start.checked_add(length).is_none_or(|v| v > interior_rows) {
                    return Err(invalid(
                        "sloping face index or polyline range out of bounds",
                    ));
                }
                interior += length;
                let mut polyline = Vec::with_capacity(length + 2);
                polyline.push(points[position(&corners, ends[0])?]);
                polyline.extend((start..start + length).map(|row| f64_pair(&face_points, row)));
                polyline.push(points[position(&corners, ends[1])?]);
                Ok(polyline)
            },
        )
        .collect::<Result<Vec<_>, _>>()?;
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
    let vertex_elevations = if h.arrays.contains_key("vertex_elevations") {
        let all = raw("vertex_elevations")?;
        Some(corners.iter().map(|&v| f32_at(&all, v)).collect::<Vec<_>>())
    } else {
        None
    };

    let compact_faces = local_pairs
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
        })
        .collect::<Result<Vec<_>, MapError>>()?;
    let mut compact_corners: Vec<Corner> = points
        .iter()
        .map(|&point| Corner {
            point,
            faces: Vec::new(),
            ccw: Vec::new(),
            cw: Vec::new(),
        })
        .collect();
    let mut ring_entries = 0;
    for (&vertex, ring) in ring_vertices.iter().zip(&rings) {
        let n = ring.len();
        ring_entries += n;
        let corner = &mut compact_corners[position(&corners, vertex)?];
        corner.faces = ring
            .iter()
            .map(|&f| position(&faces, f))
            .collect::<Result<_, _>>()?;
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
    let mut stencils: Vec<Option<sloping::CellStencil>> = (0..staged.len()).map(|_| None).collect();
    let mut owner_entries = 0;
    for (&local, global_faces) in owner_local.iter().zip(&owner_faces) {
        owner_entries += global_faces.len();
        if mesh.cells[local].virtual_cell {
            continue;
        }
        let ring = global_faces
            .iter()
            .map(|&f| position(&faces, f))
            .collect::<Result<Vec<_>, _>>()?;
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
    let s = staged.len() as u64;
    let f = faces.len() as u64;
    let w = corners.len() as u64;
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
