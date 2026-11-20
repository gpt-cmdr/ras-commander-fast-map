//! Plan-HDF reads for the preparation, through ras-commander-fast-hdf 0.5.0's
//! exact readers (the engine behind ras-commander's `backend="rust"` reads).
//!
//! Each function names the ras-commander 0.124.0 function whose values and
//! refusals it reproduces.

use crate::error::{Error, Result};
use ras_commander_fast_hdf::ErrorKind as HdfKind;
use ras_commander_fast_hdf::hdf::{
    NumericArray, NumericValues, ReadOnlyHdfSession, Slice, metadata::MetadataValue,
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Read ceiling for one dataset or selection (values only).
const READ_BUDGET: usize = 8 << 30;

pub struct Hdf {
    pub session: ReadOnlyHdfSession,
    pub path: PathBuf,
}

/// A 2-D numeric dataset in row-major order with its stored element type.
pub struct Table {
    pub rows: usize,
    pub columns: usize,
    pub values: NumericValues,
}

fn not_found(e: &ras_commander_fast_hdf::Error) -> bool {
    e.kind == HdfKind::NotFound
}

impl Hdf {
    pub fn open(path: &Path, threads: usize) -> Result<Self> {
        let session = ReadOnlyHdfSession::with_options(path, threads.max(1), threads > 1)?;
        Ok(Self {
            session,
            path: path.to_path_buf(),
        })
    }

    /// Members of a group, or None when the path is absent.
    pub fn members(&self, path: &str) -> Result<Option<ras_commander_fast_hdf::hdf::GroupMembers>> {
        match self.session.group_members(path) {
            Ok(m) => Ok(Some(m)),
            Err(e) if not_found(&e) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn dataset_shape(&self, path: &str) -> Result<Option<Vec<u64>>> {
        match self.session.numeric_capability(path) {
            Ok(c) => Ok(Some(c.shape)),
            Err(e) if not_found(&e) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn read(&self, path: &str, selection: Option<&[Slice]>) -> Result<NumericArray> {
        Ok(self
            .session
            .read_numeric_bounded(path, selection, READ_BUDGET)?)
    }

    /// A 2-D (or 1-D, as one column) numeric dataset.
    pub fn table(&self, path: &str) -> Result<Table> {
        let a = self.read(path, None)?;
        let (rows, columns) = match a.shape.as_slice() {
            [r] => (*r as usize, 1),
            [r, c] => (*r as usize, *c as usize),
            _ => {
                return Err(Error::invalid(format!(
                    "{path}: expected a 1-D or 2-D dataset"
                )));
            }
        };
        Ok(Table {
            rows,
            columns,
            values: a.values,
        })
    }

    /// Root attribute, else the `Geometry` group attribute, as text.
    fn text_attribute(&self, group: &str, name: &str) -> Result<Option<String>> {
        let attrs = match self.session.group_attributes(group) {
            Ok(a) => a,
            Err(e) if not_found(&e) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        Ok(attrs
            .into_iter()
            .find(|a| a.name == name)
            .map(|a| match a.strings {
                Some(s) if !s.is_empty() => s.join(""),
                _ => String::from_utf8_lossy(&a.raw_bytes)
                    .trim_end_matches('\0')
                    .to_string(),
            }))
    }

    /// The stored `Projection` text, as `HdfUtils.read_crs_evidence` reads it:
    /// the root attribute whenever it is stored (a blank one is absent
    /// evidence, with no fallback), else the `Geometry` group's.
    pub fn projection_wkt(&self) -> Result<Option<String>> {
        if let Some(p) = self.text_attribute("/", "Projection")? {
            return Ok(Some(p));
        }
        self.text_attribute("Geometry", "Projection")
    }

    /// `HdfUtils.collect_hdf_crs_evidence` for this plan HDF.
    pub fn crs_evidence(&self) -> Result<Vec<crate::crs::Evidence>> {
        let read =
            |p: &std::path::Path| -> Result<Option<String>> { Hdf::open(p, 1)?.projection_wkt() };
        Ok(crate::crs::model_crs_evidence(
            self.projection_wkt()?,
            &self.path,
            &read,
        ))
    }

    /// `HdfMesh._mesh_area_names`: `2D Flow Areas/Attributes` `Name` field in
    /// row order, else the legacy `Names` dataset (blank padding removed).
    pub fn mesh_names(&self) -> Result<Vec<String>> {
        let group = "Geometry/2D Flow Areas";
        let Some(members) = self.members(group)? else {
            return Ok(Vec::new());
        };
        let has = |n: &str| {
            members
                .datasets
                .iter()
                .any(|d| d.rsplit('/').next() == Some(n))
        };
        let names: Vec<String> = if has("Attributes") {
            let table = self
                .session
                .simple_compound(&format!("{group}/Attributes"), READ_BUDGET)?;
            if table.shape.len() != 1 {
                return Err(Error::invalid(
                    "2D Flow Areas/Attributes has no rank-one Name field",
                ));
            }
            let column = table
                .columns
                .iter()
                .find(|c| c.name == "Name")
                .ok_or_else(|| {
                    Error::invalid("2D Flow Areas/Attributes has no rank-one Name field")
                })?;
            match &column.value {
                MetadataValue::Strings(v) => v
                    .iter()
                    .map(|s| s.trim_end_matches('\0').to_string())
                    .collect(),
                _ => {
                    return Err(Error::unsupported(
                        "2D Flow Areas/Attributes Name is not text",
                    ));
                }
            }
        } else if has("Names") {
            self.session
                .read_strings(&format!("{group}/Names"))?
                .into_iter()
                .map(|n| n.trim_end_matches('\0').trim_end_matches(' ').to_string())
                .collect()
        } else {
            return Err(Error::invalid("2D Flow Areas inventory is missing"));
        };
        let mut seen = std::collections::HashSet::new();
        for n in &names {
            if n.is_empty() || n.contains('/') || n.contains('\\') {
                return Err(Error::invalid("Invalid 2D flow area name"));
            }
            if !seen.insert(n.clone()) {
                return Err(Error::invalid("2D flow area inventory has duplicate names"));
            }
        }
        Ok(names)
    }

    /// `HdfMesh.get_mesh_cell_points(include_ghost_cells=True)` for one mesh:
    /// every `Cells Center Coordinate` row, as float64 `[x, y]`.
    pub fn cell_centres(&self, mesh: &str) -> Result<Vec<[f64; 2]>> {
        let path = format!("Geometry/2D Flow Areas/{mesh}/Cells Center Coordinate");
        let t = self.table(&path)?;
        if t.columns != 2 {
            return Err(Error::invalid(format!(
                "Cells Center Coordinate of {mesh:?} is not an Nx2 table"
            )));
        }
        let v = as_f64(t.values)?;
        Ok(v.as_chunks::<2>().0.iter().map(|p| [p[0], p[1]]).collect())
    }

    /// Path of a mesh's cell Water Surface series.
    pub fn water_surface_path(mesh: &str) -> String {
        format!(
            "Results/Unsteady/Output/Output Blocks/Base Output/Unsteady Time Series/2D Flow Areas/{mesh}/Water Surface"
        )
    }

    /// Rows `[start, stop)` of a 2-D float dataset as float32 (the stored
    /// float32 values; a float64 dataset is narrowed as NumPy's `astype("<f4")`).
    pub fn rows_f32(&self, path: &str, start: u64, stop: u64, columns: u64) -> Result<Vec<f32>> {
        self.slab_f32(path, start, stop, 0, columns)
    }

    /// Rows `[start, stop)` and columns `[col_lo, col_hi)` of a 2-D float dataset as float32.
    pub fn slab_f32(
        &self,
        path: &str,
        start: u64,
        stop: u64,
        col_lo: u64,
        col_hi: u64,
    ) -> Result<Vec<f32>> {
        let selection = [Slice::new(start, stop, 1), Slice::new(col_lo, col_hi, 1)];
        let a = self.read(path, Some(&selection))?;
        Ok(match a.values {
            NumericValues::F32(v) => v,
            NumericValues::F64(v) => v.into_iter().map(|x| x as f32).collect(),
            _ => {
                return Err(Error::unsupported(format!(
                    "{path}: Water Surface must be floating point"
                )));
            }
        })
    }

    /// `HdfBase.get_result_unit_metadata(strict=True)` for a standalone plan
    /// HDF (`source="plan_hdf"`): `/Geometry/@SI Units` and `/@Units System`
    /// must agree, and the file must hold `/Results`. Returns the request's
    /// `depth_units` (`feet`/`metres`) and the record. ras-commander accepts
    /// more spellings and one disagreement (root `SI Units` with geometry
    /// `False`, resolved from dataset unit labels); those are refused here.
    pub fn result_units(&self, source_file: &str) -> Result<(String, Value)> {
        let file = std::path::Path::new(source_file)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let unresolved = |problem: &str| {
            Error::invalid(format!(
                "Could not resolve result-HDF units for {file}: {problem}. If the true unit system is known, pass unit_system='SI' or unit_system='US Customary'"
            ))
        };
        if self.members("Results")?.is_none() {
            return Err(Error::invalid(format!(
                "Could not resolve result-HDF units for {file}: the file holds no /Results group"
            )));
        }
        let si = self
            .text_attribute("Geometry", "SI Units")?
            .map(|v| v.trim().to_string());
        let system = self
            .text_attribute("/", "Units System")?
            .map(|v| v.trim().to_string());
        let mut evidence = Vec::new();
        let mut systems = Vec::new();
        if let Some(raw) = &si {
            let unit = match raw.as_str() {
                "True" | "true" | "1" => "SI",
                "False" | "false" | "0" => "US Customary",
                _ => return Err(unresolved("unrecognized /Geometry/@SI Units")),
            };
            evidence.push(
                json!({"attribute":"/Geometry/@SI Units","raw_value":raw,"unit_system":unit}),
            );
            systems.push(unit);
        }
        if let Some(raw) = &system {
            let unit = match raw.as_str() {
                "SI Units" | "SI" => "SI",
                "US Customary" => "US Customary",
                _ => return Err(unresolved("unrecognized /@Units System")),
            };
            evidence.push(json!({"attribute":"/@Units System","raw_value":raw,"unit_system":unit}));
            systems.push(unit);
        }
        let Some(first) = systems.first().copied() else {
            return Err(unresolved("no unit-system evidence"));
        };
        if systems.iter().any(|s| *s != first) {
            return Err(unresolved("/Geometry/@SI Units contradicts /@Units System"));
        }
        let (length, depth, velocity, flow, depth_units) = if first == "SI" {
            ("m", "m", "m/s", "m3/s", "metres")
        } else {
            ("ft", "ft", "ft/s", "ft3/s", "feet")
        };
        Ok((
            depth_units.into(),
            json!({
                "status":"resolved","unit_system":first,"length_units":length,"depth_units":depth,
                "velocity_units":velocity,"flow_units":flow,"source":"plan_hdf","source_file":source_file,
                "evidence":evidence,"contradictions":[],"warnings":[]
            }),
        ))
    }
}

/// NumPy `astype(float64)` of a numeric array.
pub fn as_f64(v: NumericValues) -> Result<Vec<f64>> {
    use NumericValues::*;
    Ok(match v {
        F64(v) => v,
        F32(v) => v.into_iter().map(f64::from).collect(),
        I8(v) => v.into_iter().map(f64::from).collect(),
        I16(v) => v.into_iter().map(f64::from).collect(),
        I32(v) => v.into_iter().map(f64::from).collect(),
        I64(v) => v.into_iter().map(|x| x as f64).collect(),
        U8(v) => v.into_iter().map(f64::from).collect(),
        U16(v) => v.into_iter().map(f64::from).collect(),
        U32(v) => v.into_iter().map(f64::from).collect(),
        U64(v) => v.into_iter().map(|x| x as f64).collect(),
        _ => return Err(Error::unsupported("unsupported numeric element type")),
    })
}

/// `np.ascontiguousarray(values, dtype=np.int64)` for integer data, refusing
/// non-integer kinds and unsigned values above `i64::MAX` (as `_assemble` does).
pub fn as_i64(v: NumericValues, what: &str) -> Result<Vec<i64>> {
    use NumericValues::*;
    Ok(match v {
        I8(v) => v.into_iter().map(i64::from).collect(),
        I16(v) => v.into_iter().map(i64::from).collect(),
        I32(v) => v.into_iter().map(i64::from).collect(),
        I64(v) => v,
        U8(v) => v.into_iter().map(i64::from).collect(),
        U16(v) => v.into_iter().map(i64::from).collect(),
        U32(v) => v.into_iter().map(i64::from).collect(),
        U64(v) => v
            .into_iter()
            .map(|x| {
                i64::try_from(x)
                    .map_err(|_| Error::invalid(format!("Connectivity exceeds int64: {what}")))
            })
            .collect::<Result<_>>()?,
        _ => {
            return Err(Error::invalid(format!(
                "Connectivity must contain integers: {what}"
            )));
        }
    })
}

/// Little-endian bytes of `np.ascontiguousarray(values, dtype=target)` for the
/// three mesh payload dtypes (`<i4`, `<f4`, `<f8`), NumPy unsafe-cast semantics.
pub fn payload_bytes(v: &NumericValues, target: &str) -> Result<Vec<u8>> {
    use NumericValues::*;
    macro_rules! cast {
        ($v:expr, $t:ty) => {
            $v.iter().flat_map(|x| (*x as $t).to_le_bytes()).collect()
        };
    }
    Ok(match target {
        "<i4" => match v {
            I8(v) => cast!(v, i32),
            I16(v) => cast!(v, i32),
            I32(v) => cast!(v, i32),
            I64(v) => cast!(v, i32),
            U8(v) => cast!(v, i32),
            U16(v) => cast!(v, i32),
            U32(v) => cast!(v, i32),
            U64(v) => cast!(v, i32),
            F32(v) => cast!(v, i32),
            F64(v) => cast!(v, i32),
            _ => return Err(Error::unsupported("unsupported numeric element type")),
        },
        "<f4" => match v {
            I8(v) => cast!(v, f32),
            I16(v) => cast!(v, f32),
            I32(v) => cast!(v, f32),
            I64(v) => cast!(v, f32),
            U8(v) => cast!(v, f32),
            U16(v) => cast!(v, f32),
            U32(v) => cast!(v, f32),
            U64(v) => cast!(v, f32),
            F32(v) => cast!(v, f32),
            F64(v) => cast!(v, f32),
            _ => return Err(Error::unsupported("unsupported numeric element type")),
        },
        "<f8" => match v {
            I8(v) => cast!(v, f64),
            I16(v) => cast!(v, f64),
            I32(v) => cast!(v, f64),
            I64(v) => cast!(v, f64),
            U8(v) => cast!(v, f64),
            U16(v) => cast!(v, f64),
            U32(v) => cast!(v, f64),
            U64(v) => cast!(v, f64),
            F32(v) => cast!(v, f64),
            F64(v) => cast!(v, f64),
            _ => return Err(Error::unsupported("unsupported numeric element type")),
        },
        _ => return Err(Error::invalid("payload dtype")),
    })
}
