//! Cell ownership polygons: `_invalid_cells.load_cells` (ras-commander 0.124.0).
//!
//! Native polygons come from fast-hdf's `Topology::assemble` (the kernel behind
//! `HdfMesh.get_mesh_cell_polygons(strict=False, engine="native")`). Invalid
//! cells are rebuilt and overlaps resolved with the same GEOS 3.13.1 operations,
//! in the same order, that shapely 2.1.2 runs for ras-commander.

use crate::error::{Error, Kind, Result};
use crate::geom::{self, Envelope, Index};
use crate::hdf::{Hdf, as_f64, as_i64};
use geos::{Geom, Geometry};
use ndarray::Array2;
use ras_commander_fast_hdf::mesh::{GeometryPolicy, OmissionKind, Topology};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub const POLICIES: [&str; 4] = ["repair", "hull", "omit", "raise"];

const MESH_ARRAYS: [&str; 6] = [
    "FacePoints Coordinate",
    "Faces FacePoint Indexes",
    "Faces Perimeter Info",
    "Faces Perimeter Values",
    "Cells Face and Orientation Info",
    "Cells Face and Orientation Values",
];

/// One ownership row: a Polygon piece of a cell (a split cell has several rows).
pub struct Row {
    pub mesh: usize,
    pub cell_id: i64,
    pub geom: Geometry,
    pub env: Envelope,
}

/// An invalid-cell or overlap record (`load_cells` records, without `extent`
/// in `json`; `extent` kept for `check_scope`).
pub struct Record {
    pub json: Value,
    pub extent: Option<Geometry>,
}

pub struct Cells {
    pub mesh_names: Vec<String>,
    pub rows: Vec<Row>,
    pub records: Vec<Record>,
    pub policy: String,
    pub index: Index,
}

struct Raw {
    points: Array2<f64>,
    ends: Array2<i64>,
    perim_info: Array2<i64>,
    perim: Array2<f64>,
    cell_info: Array2<i64>,
    cell_faces: Array2<i64>,
}

fn arr_f64(hdf: &Hdf, path: &str) -> Result<Array2<f64>> {
    let t = hdf.table(path)?;
    Array2::from_shape_vec((t.rows, t.columns), as_f64(t.values)?)
        .map_err(|e| Error::invalid(e.to_string()))
}
fn arr_i64(hdf: &Hdf, path: &str) -> Result<Array2<i64>> {
    let t = hdf.table(path)?;
    Array2::from_shape_vec((t.rows, t.columns), as_i64(t.values, path)?)
        .map_err(|e| Error::invalid(e.to_string()))
}

fn read_raw(hdf: &Hdf, mesh: &str) -> Result<Raw> {
    let p = |n: &str| format!("Geometry/2D Flow Areas/{mesh}/{n}");
    for n in MESH_ARRAYS {
        if hdf.dataset_shape(&p(n))?.is_none() {
            return Err(Error::unsupported("Incomplete native mesh layout"));
        }
    }
    let raw = Raw {
        points: arr_f64(hdf, &p(MESH_ARRAYS[0]))?,
        ends: arr_i64(hdf, &p(MESH_ARRAYS[1]))?,
        perim_info: arr_i64(hdf, &p(MESH_ARRAYS[2]))?,
        perim: arr_f64(hdf, &p(MESH_ARRAYS[3]))?,
        cell_info: arr_i64(hdf, &p(MESH_ARRAYS[4]))?,
        cell_faces: arr_i64(hdf, &p(MESH_ARRAYS[5]))?,
    };
    // Every array is indexed by its first two columns below.
    let narrow = [
        raw.points.ncols(),
        raw.ends.ncols(),
        raw.perim_info.ncols(),
        raw.cell_info.ncols(),
        raw.cell_faces.ncols(),
    ]
    .iter()
    .any(|c| *c < 2)
        || (raw.perim.nrows() > 0 && raw.perim.ncols() < 2);
    if narrow {
        return Err(Error::new(
            Kind::FastHdf,
            format!("Native polygon assembly failed for {mesh}: mesh tables need two columns"),
        ));
    }
    Ok(raw)
}

/// `_face_lines`: one cell's face polylines in HDF face order.
fn face_lines(raw: &Raw, faces: &[i64]) -> Result<Vec<Vec<[f64; 2]>>> {
    let get = |a: &Array2<f64>, r: i64| -> Result<[f64; 2]> {
        let r = usize::try_from(r)
            .ok()
            .filter(|r| *r < a.nrows())
            .ok_or_else(|| Error::invalid("face point index out of range"))?;
        Ok([a[[r, 0]], a[[r, 1]]])
    };
    let mut lines = Vec::new();
    for &face in faces {
        let f = usize::try_from(face)
            .ok()
            .filter(|f| *f < raw.ends.nrows())
            .ok_or_else(|| Error::invalid("face index out of range"))?;
        let (a, b) = (raw.ends[[f, 0]], raw.ends[[f, 1]]);
        let (offset, inner) = (raw.perim_info[[f, 0]], raw.perim_info[[f, 1]]);
        let mut coords = vec![get(&raw.points, a)?];
        if inner > 0 {
            for r in offset..offset + inner {
                coords.push(get(&raw.perim, r)?);
            }
        }
        coords.push(get(&raw.points, b)?);
        lines.push(coords);
    }
    Ok(lines)
}

/// `_candidate(lines, policy)`.
fn candidate(lines: &[Vec<[f64; 2]>], policy: &str) -> Result<Geometry> {
    if policy == "hull" {
        let points: Vec<[f64; 2]> = lines.iter().flatten().copied().collect();
        return Ok(geom::multipoint(&points)?.convex_hull()?);
    }
    let multi = Geometry::create_multiline_string(
        lines
            .iter()
            .map(|l| geom::line_string(l))
            .collect::<Result<Vec<_>>>()?,
    )?;
    let noded = multi.node()?;
    let parts = geom::parts(&noded)?;
    let polygons = Geometry::polygonize(&parts)?;
    geom::union_all(geom::parts(&polygons)?)
}

/// `_reason`: the reference reason code plus GEOS's explanation of the face ring.
fn reason(code: &str, lines: &[Vec<[f64; 2]>]) -> Result<String> {
    if lines.is_empty() || lines.iter().map(|l| l.len() - 1).sum::<usize>() < 3 {
        return Ok(code.to_string());
    }
    let norm = |p: [f64; 2], q: [f64; 2]| {
        ((p[0] - q[0]) * (p[0] - q[0]) + (p[1] - q[1]) * (p[1] - q[1])).sqrt()
    };
    let mut chain: Vec<Vec<[f64; 2]>> = vec![lines[0].clone()];
    for line in &lines[1..] {
        let end = *chain.last().unwrap().last().unwrap();
        if norm(line[0], end) <= norm(*line.last().unwrap(), end) {
            chain.push(line.clone());
        } else {
            chain.push(line.iter().rev().copied().collect());
        }
    }
    let ring: Vec<[f64; 2]> = chain
        .iter()
        .flat_map(|l| l[..l.len() - 1].iter().copied())
        .collect();
    let mut explanation = geom::polygon(&[ring])?.is_valid_reason()?;
    if explanation == "Valid Geometry" {
        explanation = "face polylines touch or cross out of ring order".into();
    }
    Ok(format!("{code}: {explanation}"))
}

/// Positive-area Polygon parts, by descending area, then lower-left.
fn polygon_parts(g: &Geometry) -> Result<Vec<Geometry>> {
    let mut parts = Vec::new();
    for p in geom::parts(g)? {
        if geom::is_polygon(&p)? && p.area()? > 0.0 {
            parts.push(p);
        }
    }
    let mut keyed = parts
        .into_iter()
        .map(|p| {
            let e = geom::envelope(&p)?;
            Ok((-p.area()?, e.min[0], e.min[1], p))
        })
        .collect::<Result<Vec<_>>>()?;
    keyed.sort_by(|a, b| {
        a.0.partial_cmp(&b.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .then(a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal))
    });
    Ok(keyed.into_iter().map(|k| k.3).collect())
}

/// Native polygons, the invalid records, then the policy (`load_cells`).
pub fn load(hdf: &Hdf, mesh_names: &[String], policy: &str, threads: usize) -> Result<Cells> {
    if !POLICIES.contains(&policy) {
        return Err(Error::invalid(
            "invalid_cells must be one of 'repair', 'hull', 'omit', 'raise'",
        ));
    }
    // Native assembly (fast_hdf._native_mesh_polygons, strict=False, rust mode).
    let mut originals: Vec<(usize, i64, Geometry)> = Vec::new();
    let mut omitted: Vec<(usize, i64, &'static str, Vec<i64>)> = Vec::new();
    let mut raws = Vec::new();
    for (m, name) in mesh_names.iter().enumerate() {
        let raw = read_raw(hdf, name)?;
        let top = Topology::new(
            raw.points.clone(),
            raw.ends.clone(),
            raw.perim_info.clone(),
            raw.perim.clone(),
            raw.cell_info.clone(),
            raw.cell_faces.clone(),
        );
        let packed = top
            .assemble(GeometryPolicy::QuarantineInvalid, threads.max(1))
            .map_err(|e| {
                Error::new(
                    Kind::FastHdf,
                    format!("Native polygon assembly failed for {name}: {e}"),
                )
            })?;
        let invalid: BTreeSet<i64> = packed
            .diagnostics
            .iter()
            .filter(|d| d.kind == OmissionKind::InvalidGeometry)
            .map(|d| d.cell_id)
            .collect();
        let assembled: BTreeSet<i64> = packed.cell_ids.iter().copied().collect();
        for cell in 0..raw.cell_info.nrows() as i64 {
            if assembled.contains(&cell) {
                continue;
            }
            let (start, length) = (
                raw.cell_info[[cell as usize, 0]],
                raw.cell_info[[cell as usize, 1]],
            );
            if invalid.contains(&cell) || length == 0 {
                let code = if invalid.contains(&cell) {
                    "no_polygon"
                } else {
                    "zero_faces"
                };
                let faces = (start..start + length)
                    .map(|r| {
                        usize::try_from(r)
                            .ok()
                            .filter(|r| *r < raw.cell_faces.nrows())
                            .map(|r| raw.cell_faces[[r, 0]])
                            .ok_or_else(|| Error::invalid("cell face span out of range"))
                    })
                    .collect::<Result<Vec<_>>>()?;
                omitted.push((m, cell, code, faces));
            }
        }
        let mut bad = Vec::new();
        for (k, id) in packed.cell_ids.iter().enumerate() {
            let ring =
                &packed.coordinates[packed.offsets[k] as usize..packed.offsets[k + 1] as usize];
            let polygon = geom::polygon(&[ring.to_vec()])?;
            if !polygon.is_valid()? {
                bad.push(*id);
            }
            originals.push((m, *id, polygon));
        }
        if !bad.is_empty() {
            return Err(Error::new(
                Kind::FastHdf,
                format!(
                    "Native polygon geometry is invalid for {name}, cell IDs {:?}",
                    &bad[..bad.len().min(10)]
                ),
            ));
        }
        raws.push(raw);
    }
    if originals.is_empty() && omitted.is_empty() {
        return Err(Error::invalid(
            "sloping mapping requires physical 2D cell polygons",
        ));
    }
    let n = originals.len();
    let envs: Vec<Envelope> = originals
        .iter()
        .map(|o| geom::envelope(&o.2))
        .collect::<Result<_>>()?;
    let index = Index::new(&envs);
    if omitted.is_empty() {
        let rows = originals
            .into_iter()
            .zip(envs)
            .map(|((mesh, cell_id, geom), env)| Row {
                mesh,
                cell_id,
                geom,
                env,
            })
            .collect();
        return Ok(Cells {
            mesh_names: mesh_names.to_vec(),
            rows,
            records: Vec::new(),
            policy: policy.into(),
            index,
        });
    }
    // Invalid records in (mesh_name, cell_id) order.
    // `order` takes record meshes in diagnostics (HDF mesh) order, before sorting.
    let diagnostics_meshes: Vec<usize> = omitted.iter().map(|o| o.0).collect();
    omitted.sort_by(|a, b| mesh_names[a.0].cmp(&mesh_names[b.0]).then(a.1.cmp(&b.1)));
    struct Entry {
        mesh: usize,
        cell_id: i64,
        reason: String,
        action: String,
        original_area: Option<f64>,
        fallback_area: Option<f64>,
        pieces: usize,
        extent: Option<Geometry>,
        substitute: Option<Geometry>,
        geometry: Option<Geometry>,
    }
    let mut entries = Vec::new();
    for (m, cell, code, faces) in &omitted {
        let lines = face_lines(&raws[*m], faces)?;
        let mut e = Entry {
            mesh: *m,
            cell_id: *cell,
            reason: reason(code, &lines)?,
            action: "omitted".into(),
            original_area: None,
            fallback_area: None,
            pieces: 0,
            extent: None,
            substitute: None,
            geometry: None,
        };
        if !lines.is_empty() {
            let points: Vec<[f64; 2]> = lines.iter().flatten().copied().collect();
            e.extent = Some(geom::multipoint(&points)?.convex_hull()?);
            e.original_area = Some(candidate(&lines, "repair")?.area()?);
            if policy == "repair" || policy == "hull" {
                e.substitute = Some(candidate(&lines, policy)?);
            }
        }
        entries.push(e);
    }
    // Mesh order by first appearance in the rows, then the records' meshes.
    let mut order: HashMap<usize, usize> = HashMap::new();
    for (m, _, _) in &originals {
        let k = order.len();
        order.entry(*m).or_insert(k);
    }
    for m in &diagnostics_meshes {
        let k = order.len();
        order.entry(*m).or_insert(k);
    }
    let rank = |i: usize| (order[&originals[i].0], originals[i].1);
    let mut geometries: Vec<Option<Geometry>> = originals
        .iter()
        .map(|o| geom::clone(&o.2).map(Some))
        .collect::<Result<_>>()?;
    let query = |g: &Geometry| -> Result<Vec<usize>> {
        let mut hits = index.query_intersects(g, |j| &originals[j].2)?;
        hits.sort_unstable();
        Ok(hits)
    };
    let overlapping = |i: usize| -> Result<Vec<usize>> {
        let mut found = Vec::new();
        for j in query(&originals[i].2)? {
            if j != i
                && originals[j].0 == originals[i].0
                && originals[i].2.intersection(&originals[j].2)?.area()? > 0.0
            {
                found.push(j);
            }
        }
        Ok(found)
    };
    let mut seed = BTreeSet::new();
    for e in &entries {
        if let Some(ext) = &e.extent {
            for i in query(ext)? {
                if originals[i].0 == e.mesh {
                    seed.insert(i);
                }
            }
        }
    }
    let mut pending: Vec<usize> = seed.iter().copied().collect();
    pending.sort_by_key(|&i| rank(i));
    let mut seen: BTreeSet<usize> = pending.iter().copied().collect();
    let mut partners: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    let mut cursor = 0;
    while cursor < pending.len() {
        let i = pending[cursor];
        cursor += 1;
        let found = overlapping(i)?;
        for &j in &found {
            if seen.insert(j) {
                pending.push(j);
            }
        }
        partners.insert(i, found);
    }
    let mut keys: Vec<usize> = partners.keys().copied().collect();
    keys.sort_by_key(|&i| rank(i));
    let mut trimmed: Vec<(usize, Record)> = Vec::new();
    for i in keys {
        let lower: Vec<usize> = partners[&i]
            .iter()
            .copied()
            .filter(|&j| rank(j) < rank(i))
            .collect();
        if lower.is_empty() {
            continue;
        }
        let winners: Vec<Geometry> = lower
            .iter()
            .filter_map(|&j| geometries[j].as_ref())
            .map(geom::clone)
            .collect::<Result<_>>()?;
        let remainder = if winners.is_empty() {
            geom::clone(&originals[i].2)?
        } else {
            originals[i].2.difference(&geom::union_all(winners)?)?
        };
        let parts = polygon_parts(&remainder)?;
        // `float(sum(...))` starts at +0.0 (Rust's `Sum` for f64 starts at -0.0).
        let mut fallback = 0.0f64;
        for part in &parts {
            fallback += part.area()?;
        }
        let count = parts.len();
        geometries[i] = match count {
            0 => None,
            1 => Some(parts.into_iter().next().unwrap()),
            _ => Some(Geometry::create_multipolygon(parts)?),
        };
        let mut overlap: Vec<i64> = lower.iter().map(|&j| originals[j].1).collect();
        overlap.sort_unstable();
        trimmed.push((
            i,
            Record {
                json: json!({
                    "mesh": mesh_names[originals[i].0], "cell_id": originals[i].1,
                    "reason": "overlaps a lower-ID cell next to an invalid cell",
                    "action": if count > 0 { "overlap_trimmed" } else { "overlap_consumed" },
                    "original_area": originals[i].2.area()?, "fallback_area": fallback,
                    "pieces": count, "overlap_with": overlap,
                }),
                extent: Some(geom::clone(&originals[i].2)?),
            },
        ));
    }
    // Invalid cells take only area no valid cell owns, in record order.
    let mut additions: Vec<(usize, Geometry)> = Vec::new();
    for e in entries.iter_mut() {
        let Some(shape) = e.substitute.take() else {
            continue;
        };
        let mut occupied: Vec<Geometry> = Vec::new();
        for i in query(&shape)? {
            if originals[i].0 == e.mesh
                && let Some(g) = &geometries[i]
            {
                occupied.push(geom::clone(g)?);
            }
        }
        for (m, g) in &additions {
            if *m == e.mesh && g.intersects(&shape)? {
                occupied.push(geom::clone(g)?);
            }
        }
        let shape = if occupied.is_empty() {
            shape
        } else {
            shape.difference(&geom::union_all(occupied)?)?
        };
        let Some(shape) = polygon_parts(&shape)?.into_iter().next() else {
            continue;
        };
        if !shape.is_valid()? {
            continue;
        }
        additions.push((e.mesh, geom::clone(&shape)?));
        e.action = if policy == "repair" {
            "repaired".into()
        } else {
            "convex_hull".into()
        };
        e.fallback_area = Some(shape.area()?);
        e.pieces = 1;
        e.geometry = Some(shape);
    }
    // Rows: kept originals, then additions, exploded, stable-sorted by mesh/cell.
    let mut rows: Vec<(usize, i64, Geometry)> = Vec::new();
    for (k, g) in geometries.into_iter().enumerate() {
        if let Some(g) = g {
            rows.push((originals[k].0, originals[k].1, g));
        }
    }
    let mut records: Vec<Record> = Vec::new();
    for e in entries.iter_mut() {
        if let Some(g) = e.geometry.take() {
            rows.push((e.mesh, e.cell_id, g));
        }
        records.push(Record {
            json: json!({
                "mesh": mesh_names[e.mesh], "cell_id": e.cell_id, "reason": e.reason, "action": e.action,
                "original_area": e.original_area, "fallback_area": e.fallback_area, "pieces": e.pieces,
                "overlap_with": [],
            }),
            extent: e.extent.take(),
        });
    }
    records.extend(trimmed.into_iter().map(|(_, r)| r));
    let mut exploded: Vec<(usize, i64, Geometry)> = Vec::new();
    for (m, id, g) in rows {
        if geom::is_polygon(&g)? {
            exploded.push((m, id, g));
        } else {
            for p in geom::parts(&g)? {
                exploded.push((m, id, p));
            }
        }
    }
    let stride = exploded.iter().map(|r| r.1).max().map_or(1, |v| v + 1);
    let mut keyed: Vec<(i64, usize)> = exploded
        .iter()
        .enumerate()
        .map(|(k, r)| (order[&r.0] as i64 * stride + r.1, k))
        .collect();
    keyed.sort_by_key(|k| k.0);
    let mut slots: Vec<Option<(usize, i64, Geometry)>> = exploded.into_iter().map(Some).collect();
    let mut out = Vec::with_capacity(n);
    for (_, k) in keyed {
        let (mesh, cell_id, geom) = slots[k].take().unwrap();
        let env = geom::envelope(&geom)?;
        out.push(Row {
            mesh,
            cell_id,
            geom,
            env,
        });
    }
    let out_envs: Vec<Envelope> = out.iter().map(|r| r.env).collect();
    let index = Index::new(&out_envs);
    if out.is_empty() {
        let listed: Vec<String> = records
            .iter()
            .take(10)
            .map(|r| {
                format!(
                    "{} cell {} ({})",
                    r.json["mesh"].as_str().unwrap_or(""),
                    r.json["cell_id"],
                    r.json["action"].as_str().unwrap_or("")
                )
            })
            .collect();
        return Err(Error::invalid(format!(
            "sloping mapping has no physical 2D cell polygons left under invalid_cells={}: {}",
            crate::error::py_repr(policy),
            listed.join(", ")
        )));
    }
    Ok(Cells {
        mesh_names: mesh_names.to_vec(),
        rows: out,
        records,
        policy: policy.into(),
        index,
    })
}

impl Cells {
    /// `_invalid_cells.describe(records, policy, aoi)` (`fim-invalid-cells/1`).
    pub fn describe(&self, aoi: Option<&Geometry>) -> Result<Value> {
        let mut cells = Vec::new();
        for r in &self.records {
            let mut entry = r.json.clone();
            entry["intersects_aoi"] = match (aoi, &r.extent) {
                (Some(a), Some(e)) => Value::Bool(e.intersects(a)?),
                _ => Value::Null,
            };
            cells.push(entry);
        }
        Ok(
            json!({"schema":"fim-invalid-cells/1","policy":self.policy,"count":cells.len(),"cells":cells}),
        )
    }

    /// `_invalid_cells.check_scope`: refuse `raise` only when an invalid cell can
    /// affect the output.
    pub fn check_scope(&self, aoi: Option<&Geometry>) -> Result<()> {
        if self.policy != "raise" || self.records.is_empty() {
            return Ok(());
        }
        let mut hits = Vec::new();
        for r in &self.records {
            let hit = match (aoi, &r.extent) {
                (None, _) | (_, None) => true,
                (Some(a), Some(e)) => e.intersects(a)?,
            };
            if hit {
                hits.push(r);
            }
        }
        if let Some(first) = hits.first() {
            let listed: Vec<String> = hits
                .iter()
                .take(10)
                .map(|r| {
                    format!(
                        "{} cell {}",
                        r.json["mesh"].as_str().unwrap_or(""),
                        r.json["cell_id"]
                    )
                })
                .collect();
            return Err(Error::new(
                Kind::MeshCellPolygon,
                format!(
                    "Invalid 2D cell polygons intersect the requested output ({}; {}). Pass invalid_cells='repair', 'hull' or 'omit' to map without them, or an AOI that excludes them.",
                    listed.join(", "),
                    first.json["reason"].as_str().unwrap_or("")
                ),
            ));
        }
        Ok(())
    }

    /// Unique `(mesh_name, cell_id)` count (`info["physical_cells"]`).
    pub fn physical_cells(&self) -> usize {
        let set: BTreeSet<(usize, i64)> = self.rows.iter().map(|r| (r.mesh, r.cell_id)).collect();
        set.len()
    }
}
