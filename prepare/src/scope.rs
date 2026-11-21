//! `RasFim.validate_2d_model` (`fim/_scope.py` `inspect_model`): require
//! compiled 2D cells and refuse any 1D river, reach or cross section, from the
//! HDF tables and, when present, the geometry text file.

use crate::error::{Error, Result};
use crate::hdf::Hdf;
use ras_commander_fast_hdf::hdf::metadata::MetadataValue;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const BUDGET: usize = 256 << 20;

fn name(raw: &str) -> String {
    raw.trim_matches(|c| matches!(c, '\0' | ' ' | '\t' | '\r' | '\n'))
        .to_string()
}

fn unsupported(m: impl Into<String>) -> Error {
    Error::unsupported(format!("FIM mapping is 2D-only: {}", m.into()))
}

/// Rows of the identity tables of a 1D geometry group (`count()` in `_scope`).
fn records(hdf: &Hdf, group: &str) -> Result<u64> {
    let Some(members) = hdf.members(group)? else {
        return Ok(0);
    };
    let has = |n: &str| {
        members
            .datasets
            .iter()
            .any(|d| d.rsplit('/').next() == Some(n))
    };
    let mut sizes = Vec::new();
    for n in [
        "Attributes",
        "Polyline Info",
        "River Stations",
        "River Names",
        "Reach Names",
    ] {
        if !has(n) {
            continue;
        }
        let path = format!("{group}/{n}");
        let rows = match hdf.dataset_shape(&path)? {
            Some(shape) if !shape.is_empty() => shape[0],
            Some(_) => {
                return Err(unsupported(format!(
                    "malformed model scope identity: {path}"
                )));
            }
            None => match hdf.session.simple_compound(&path, BUDGET) {
                Ok(t) if !t.shape.is_empty() => t.shape[0],
                _ => match hdf.session.read_strings(&path) {
                    Ok(v) => v.len() as u64,
                    Err(_) => {
                        return Err(unsupported(format!(
                            "malformed model scope identity: {path}"
                        )));
                    }
                },
            },
        };
        sizes.push(rows);
    }
    let rec = sizes.into_iter().max().unwrap_or(0);
    if rec == 0 && has("Polyline Points") {
        let path = format!("{group}/Polyline Points");
        match hdf.dataset_shape(&path)? {
            Some(shape) if shape.len() == 2 && shape[1] == 2 => {
                if shape[0] > 0 {
                    return Err(unsupported(format!(
                        "orphaned polyline geometry lacks feature identities: {group}"
                    )));
                }
            }
            _ => {
                return Err(unsupported(format!(
                    "malformed model scope polyline: {group}"
                )));
            }
        }
    }
    Ok(rec)
}

/// The geometry text file the scope check reads, as `_scope` resolves it.
fn geometry_text(hdf: &Hdf, path: &Path) -> Result<Option<PathBuf>> {
    let file = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let stem = path.with_extension("");
    let stem_suffix = stem.extension().map(|e| e.to_string_lossy().to_lowercase());
    let candidate =
        if file.ends_with(".hdf") && stem_suffix.as_deref().is_some_and(|s| s.starts_with('g')) {
            Some(stem)
        } else {
            let attrs = match hdf.session.group_attributes("Plan Data/Plan Information") {
                Ok(a) => Some(a),
                Err(e) if e.kind == ras_commander_fast_hdf::ErrorKind::NotFound => None,
                Err(e) => return Err(e.into()),
            };
            attrs.map(|attrs| {
                let value = attrs
                    .into_iter()
                    .find(|a| a.name == "Geometry Filename")
                    .map(|a| a.strings.map(|s| s.join("")).unwrap_or_default())
                    .unwrap_or_default();
                path.parent()
                    .unwrap_or(Path::new(""))
                    .join(value.replace('\\', "/"))
            })
        };
    Ok(candidate.filter(|c| c.is_file()))
}

/// `GeomParser.extract_river_reach` plus the unparsed-declaration rule, and
/// `Type RM Length L Ch R=` 1D node lines.
fn text_one_d(text: &str) -> (usize, usize) {
    let mut reaches = std::collections::BTreeSet::new();
    let mut nodes = 0;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    for line in text.split("\r\n").flat_map(|l| l.split(['\r', '\n'])) {
        let stripped = line.trim();
        let mut found = false;
        if let Some(rest) = stripped.strip_prefix("River Reach=") {
            let values: Vec<&str> = rest.split(',').map(str::trim).collect();
            if values.len() >= 2 && !values[0].is_empty() && !values[1].is_empty() {
                reaches.insert(format!("{}\u{0}{}", values[0], values[1]));
                found = true;
            }
        } else if let Some(rest) = stripped.strip_prefix("Reach=") {
            let n = rest.trim();
            if !n.is_empty() {
                reaches.insert(format!("{n}\u{0}{n}"));
                found = true;
            }
        }
        if !found
            && (stripped.starts_with("River Reach=") || stripped.starts_with("Reach="))
            && let Some((_, value)) = line.split_once('=')
            && !value
                .trim_matches(|c| matches!(c, ' ' | ',' | '\r' | '\n'))
                .is_empty()
        {
            reaches.insert(format!("unparsed reach declaration\u{0}{stripped}"));
        }
        if stripped.starts_with("Type RM Length L Ch R=") {
            nodes += 1;
        }
    }
    (reaches.len(), nodes)
}

/// The scope record (`FimModelScope.to_dict()` with `dimension: "2D-only"`).
pub fn validate_2d_model(hdf: &Hdf) -> Result<Value> {
    let areas = "Geometry/2D Flow Areas";
    let mut physical: BTreeMap<String, i64> = BTreeMap::new();
    let mut has_physical = false;
    let mut meshes = Vec::new();
    if let Some(members) = hdf.members(areas)? {
        if members
            .datasets
            .iter()
            .any(|d| d.rsplit('/').next() == Some("Attributes"))
        {
            let t = hdf
                .session
                .simple_compound(&format!("{areas}/Attributes"), BUDGET)
                .map_err(|_| unsupported("malformed 2D flow-area attributes"))?;
            if t.shape.len() != 1 {
                return Err(unsupported("malformed 2D flow-area attributes"));
            }
            let names = t.columns.iter().find(|c| c.name == "Name");
            let counts = t.columns.iter().find(|c| c.name == "Cell Count");
            let (Some(names), Some(counts)) = (names, counts) else {
                return Err(unsupported("malformed 2D flow-area attributes"));
            };
            let names = match &names.value {
                MetadataValue::Strings(v) => v.clone(),
                _ => return Err(unsupported("model scope name is not a string")),
            };
            let counts: Vec<f64> = match &counts.value {
                MetadataValue::Signed(v) => v.iter().map(|x| *x as f64).collect(),
                MetadataValue::Unsigned(v) => v.iter().map(|x| *x as f64).collect(),
                MetadataValue::Float(v) => v.clone(),
                _ => return Err(unsupported("malformed 2D flow-area attributes")),
            };
            has_physical = true;
            for (raw, value) in names.iter().zip(counts) {
                let n = name(raw);
                if n.is_empty()
                    || physical.contains_key(&n)
                    || !value.is_finite()
                    || value.fract() != 0.0
                    || value < 0.0
                {
                    return Err(unsupported("invalid 2D physical cell count/name"));
                }
                physical.insert(n, value as i64);
            }
        }
        let mut groups: Vec<String> = members
            .groups
            .iter()
            .map(|g| g.rsplit('/').next().unwrap_or(g).to_string())
            .collect();
        groups.sort();
        for g in groups {
            let path = format!("{areas}/{g}/Cells Center Coordinate");
            let Some(shape) = hdf.dataset_shape(&path)? else {
                continue;
            };
            if shape.len() != 2 || shape[1] != 2 {
                return Err(unsupported(format!("malformed compiled mesh centers: {g}")));
            }
            if has_physical && !physical.contains_key(&g) {
                return Err(unsupported(format!(
                    "compiled mesh missing from physical count table: {g}"
                )));
            }
            let cells = physical.get(&g).copied().unwrap_or(shape[0] as i64);
            if cells as u64 > shape[0] {
                return Err(unsupported(format!(
                    "physical cell count exceeds compiled centers: {g}"
                )));
            }
            if cells > 0 {
                meshes.push(g);
            }
        }
        if physical.iter().any(|(n, c)| *c > 0 && !meshes.contains(n)) {
            return Err(unsupported(
                "physical cell count table contains a mesh without compiled centers",
            ));
        }
    }
    let rivers = records(hdf, "Geometry/River Centerlines")?;
    let sections = records(hdf, "Geometry/Cross Sections")?;
    let mut storage = Vec::new();
    if let Some(m) = hdf.members("Geometry/Storage Areas")? {
        let has_attrs = m
            .datasets
            .iter()
            .any(|d| d.rsplit('/').next() == Some("Attributes"));
        if !has_attrs {
            if !m.datasets.is_empty() || !m.groups.is_empty() {
                return Err(unsupported("storage geometry lacks name attributes"));
            }
        } else {
            let t = hdf
                .session
                .simple_compound("Geometry/Storage Areas/Attributes", BUDGET)
                .map_err(|_| unsupported("malformed storage-area name attributes"))?;
            let names = t
                .columns
                .iter()
                .find(|c| c.name == "Name")
                .ok_or_else(|| unsupported("malformed storage-area name attributes"))?;
            let MetadataValue::Strings(v) = &names.value else {
                return Err(unsupported("model scope name is not a string"));
            };
            storage = v.iter().map(|s| name(s)).collect();
            let unique: std::collections::BTreeSet<_> = storage.iter().collect();
            if storage.iter().any(String::is_empty) || unique.len() != storage.len() {
                return Err(unsupported(
                    "storage-area names must be unique and non-empty",
                ));
            }
        }
    }
    let geometry = geometry_text(hdf, &hdf.path)?;
    let (text_reaches, text_nodes) = match &geometry {
        Some(p) => text_one_d(&String::from_utf8_lossy(&std::fs::read(p)?)),
        None => (0, 0),
    };
    let one_d = rivers > 0 || sections > 0 || text_reaches > 0 || text_nodes > 0;
    if meshes.is_empty() && !one_d && storage.is_empty() {
        return Err(unsupported(
            "model scope has no compiled mesh, river/XS or storage geometry",
        ));
    }
    if meshes.is_empty() {
        return Err(Error::unsupported(
            "FIM mapping is 2D-only: no compiled 2D mesh cells",
        ));
    }
    if one_d {
        return Err(Error::unsupported(format!(
            "FIM mapping is 2D-only; models containing 1D river centerlines/reaches or cross sections are rejected (centerline records={rivers}, cross-section records={sections}, text reaches={text_reaches}, text 1D nodes={text_nodes}). Selecting a 2D mesh does not bypass this check. Pipe networks are permitted."
        )));
    }
    Ok(json!({
        "dimension":"2D-only","mesh_names":meshes,"cross_section_count":sections,
        "reach_count":rivers.max(text_reaches as u64),"storage_area_names":storage,
        "geometry_path":geometry.map(|p| p.to_string_lossy().into_owned()),
    }))
}
