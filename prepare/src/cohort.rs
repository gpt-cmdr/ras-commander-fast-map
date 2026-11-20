//! One preparation cohort: fim-commander's `_depth_groups._cohort` up to the
//! request it hands to Fast-Map, with `RasFim.prepare_sloping_model` inside.

use crate::aoi::Aoi;
use crate::cells::{self, Cells};
use crate::error::{Error, Result};
use crate::geom;
use crate::grid::{self, GridPlan, Terrain};
use crate::hdf::Hdf;
use crate::payload::{self, MeshPayload};
use crate::request::{self, Controls, Job};

/// Normalised cohort input, from either front end.
pub struct PrepareInput {
    pub hdf_path: PathBuf,
    pub terrain_path: PathBuf,
    pub jobs: Vec<Job>,
    pub controls: Controls,
    pub invalid_cells: String,
    pub persistent_session: bool,
    /// The config's `wse_tiles` (fim-commander `_check_wse_tiles` record). A wave the
    /// tiles could serve (`_tile_layout`) is staged from the staged cells' HDF columns
    /// with only staged values checked, as fim-commander's tile path checks them.
    pub wse_tiles: Option<Value>,
    /// Watched sources (`hdf`, `terrain`, `polygon`, `geometry`, `engine`, ...)
    /// with the `(size, mtime_ns)` identity recorded at normalisation.
    pub sources: Vec<Source>,
    /// The engine, for its `terrain-info` cross-check.
    pub engine_path: PathBuf,
}

/// One watched source file.
#[derive(Clone, Debug)]
pub struct Source {
    pub key: String,
    pub path: PathBuf,
    pub identity: (u64, i128),
}

/// `_catalog._check_sources(jobs)`: `RuntimeError` naming the job.
pub fn check_job_sources(sources: &[Source], job_id: &str) -> Result<()> {
    for s in sources {
        if file_identity(&s.path).ok() != Some(s.identity) {
            return Err(Error::new(
                crate::error::Kind::Runtime,
                format!(
                    "Catalog source changed for job {}",
                    crate::error::py_repr(job_id)
                ),
            ));
        }
    }
    Ok(())
}

/// `max_*_mib << 20` without wrapping.
fn mib(v: u64, what: &str) -> Result<u64> {
    v.checked_mul(1 << 20)
        .ok_or_else(|| Error::invalid(format!("{what} is too large")))
}
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::time::Instant;

pub const STAGED_CELLS_METHOD: &str = "ras-commander-staged-cells/1";
/// Recorded on a wave staged by reading only the staged cells' HDF columns
/// (fim-commander records `parent-extract-cell-wse-tiles/1` when it reads tiles).
pub const STAGED_CELL_READS_SOURCE: &str = "plan-hdf-staged-cells/1";

/// What the caller needs after preparation.
pub struct Prepared {
    pub root: PathBuf,
    pub request_path: PathBuf,
    pub request: Value,
    pub plans: Map<String, Value>,
    pub preparation: Value,
    pub prepared_info: Value,
}

/// `(size, mtime_ns)` of a file (`RasFim._file_identity`).
pub fn file_identity(path: &Path) -> Result<(u64, i128)> {
    let m = std::fs::metadata(path)?;
    let t = m.modified()?;
    let ns = match t.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_nanos() as i128,
        Err(e) => -(e.duration().as_nanos() as i128),
    };
    Ok((m.len(), ns))
}

/// CPython 3.12 `sys.getsizeof` of a `str` (compact ASCII, latin-1, UCS-2, UCS-4).
pub fn str_sizeof(s: &str) -> u64 {
    let n = s.chars().count() as u64;
    match s.chars().map(|c| c as u32).max().unwrap_or(0) {
        0..=0x7f => 41 + n,
        0x80..=0xff => 57 + n,
        0x100..=0xffff => 58 + 2 * n,
        _ => 60 + 4 * n,
    }
}

/// CPython 3.12 `sys.getsizeof` of a non-negative int.
pub fn int_sizeof(v: u64) -> u64 {
    let mut digits = 0u64;
    let mut x = v;
    while x > 0 {
        digits += 1;
        x >>= 30;
    }
    24 + 4 * digits.max(1)
}

/// `planned_geometry_bytes`: `cells.memory_usage(deep=True).sum()` of the
/// RangeIndex frame (object mesh names, int64 cell ids, geometry pointers),
/// plus 16 bytes per coordinate and 256 per row.
fn planned_geometry_bytes(cells: &Cells) -> Result<u64> {
    let n = cells.rows.len() as u64;
    let index = 48 + int_sizeof(0) + int_sizeof(n) + int_sizeof(1);
    let names: u64 = cells
        .rows
        .iter()
        .map(|r| 8 + str_sizeof(&cells.mesh_names[r.mesh]))
        .sum();
    let mut coords = 0u64;
    for r in &cells.rows {
        coords += geom::count_coordinates(&r.geom)? as u64;
    }
    Ok(index + names + n * 8 + n * 8 + coords * 16 + n * 256)
}

/// `json.dumps` length of a small JSON value with Python's default separators.
fn py_json_len(v: &Value) -> usize {
    fn write(v: &Value, out: &mut String) {
        match v {
            Value::Object(m) => {
                out.push('{');
                for (k, (key, val)) in m.iter().enumerate() {
                    if k > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(&serde_json::to_string(key).unwrap());
                    out.push_str(": ");
                    write(val, out);
                }
                out.push('}');
            }
            Value::Array(a) => {
                out.push('[');
                for (k, val) in a.iter().enumerate() {
                    if k > 0 {
                        out.push_str(", ");
                    }
                    write(val, out);
                }
                out.push(']');
            }
            other => out.push_str(&other.to_string()),
        }
    }
    let mut s = String::new();
    write(v, &mut s);
    s.len()
}

/// `str(Path(p).resolve())` byte length (Windows verbatim prefix removed).
fn resolved_len(p: &Path) -> Result<u64> {
    let r = std::fs::canonicalize(p)?;
    Ok(request::plain_path(&r.to_string_lossy()).len() as u64)
}

/// `_group_budgets.plan_window_cache(model, jobs, requested_mib)`.
fn plan_window_cache(
    terrain: &Path,
    crs_identity: &Value,
    meshes: &[MeshPayload],
    geometry_file: &Path,
    owner_cells: usize,
    jobs: &[Value],
    requested_mib: Option<u64>,
) -> Result<Value> {
    const MIB: u64 = 1 << 20;
    const WEIGHTS: u64 = 32 * MIB;
    let reserve = |p: &Path| -> Result<u64> { Ok(resolved_len(p)? * 4 + 256) };
    let mut identities = reserve(terrain)? + py_json_len(crs_identity) as u64 * 4 + 4096;
    for m in meshes {
        identities += reserve(&m.header)?;
        for f in &m.files {
            identities += reserve(f)?;
        }
    }
    let geometry_bytes = std::fs::metadata(geometry_file)?.len();
    let address_bytes = owner_cells as u64 * 2 * 8;
    let mut entries = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for job in jobs {
        let aoi = std::fs::read(job["aoi_file"].as_str().unwrap_or_default())?;
        let window: Vec<i64> = serde_json::from_value(job["window"].clone())?;
        if !seen.insert((window.clone(), aoi.clone())) {
            continue;
        }
        let arrays = (window[2] * window[3]) as u64 * 8;
        let payload = arrays + address_bytes + geometry_bytes + aoi.len() as u64;
        entries.push(json!({
            "job_id": job["job_id"], "window": window, "owner_and_ground_bytes": arrays,
            "key_payload_bytes": address_bytes + geometry_bytes + aoi.len() as u64,
            "identity_and_record_reserve_bytes": identities,
            "pixel_weight_allowance_bytes": WEIGHTS,
            "required_bytes": payload + identities + WEIGHTS,
        }));
    }
    let required: u64 = entries
        .iter()
        .map(|e| e["required_bytes"].as_u64().unwrap())
        .sum();
    let effective = requested_mib.unwrap_or(required.div_ceil(MIB));
    if effective > 4096 {
        return Err(Error::budget(format!(
            "Window cache required {required} bytes, allowed {} bytes",
            4096 * MIB
        )));
    }
    if effective > 0 && effective * MIB < required {
        return Err(Error::budget(format!(
            "Window cache required {required} bytes, allowed {} bytes",
            effective * MIB
        )));
    }
    Ok(json!({
        "requested_mib": requested_mib, "effective_mib": effective, "required_bytes": required,
        "retention_enabled": effective > 0, "entries": entries,
        "scope": "distinct windows in one parent call; charged within native working memory",
    }))
}

/// `_tile_layout(tiles, hdf_path, meshes, selectors)` is not None: the tiles
/// could stage this wave exactly (metadata only; no tile is read here).
fn tile_layout(
    tiles: Option<&Value>,
    hdf_path: &Path,
    meshes: &[MeshPayload],
    wave: &[u64],
) -> Result<bool> {
    let Some(t) = tiles else {
        return Ok(false);
    };
    if meshes.len() != 1 {
        return Ok(false);
    }
    let mesh = &meshes[0];
    let time_count = t["time_count"].as_u64().unwrap_or(0);
    let same_hdf = t["hdf_path"]
        .as_str()
        .and_then(|p| request::resolve(Path::new(p)).ok())
        .is_some_and(|p| request::resolve(hdf_path).is_ok_and(|h| h == p));
    if t["mesh_name"].as_str() != Some(mesh.name.as_str())
        || t["cell_count"].as_u64() != Some(mesh.cell_count as u64)
        || !same_hdf
        || wave.iter().any(|&s| s >= time_count)
    {
        return Ok(false);
    }
    let mut by_time: std::collections::BTreeMap<(u64, u64), Vec<(u64, u64)>> = Default::default();
    for c in t["chunks"].as_array().into_iter().flatten() {
        let key = (
            c["time_start"].as_u64().unwrap_or(u64::MAX),
            c["time_stop"].as_u64().unwrap_or(u64::MAX),
        );
        by_time.entry(key).or_default().push((
            c["entity_start"].as_u64().unwrap_or(u64::MAX),
            c["entity_stop"].as_u64().unwrap_or(u64::MAX),
        ));
    }
    let mut end = 0;
    for &(start, stop) in by_time.keys() {
        if start != end {
            return Ok(false);
        }
        end = stop;
    }
    if end != time_count {
        return Ok(false);
    }
    for (&(start, stop), space) in &by_time {
        if !wave.iter().any(|&s| start <= s && s < stop) {
            continue;
        }
        let mut space = space.clone();
        space.sort_unstable();
        let mut edge = 0;
        for (lo, hi) in space {
            if lo != edge {
                return Ok(false);
            }
            edge = hi;
        }
        if edge != mesh.cell_count as u64 {
            return Ok(false);
        }
    }
    Ok(true)
}

fn write_framed_wkb(path: &Path, wkbs: &[Vec<u8>]) -> Result<()> {
    let mut out = Vec::new();
    for w in wkbs {
        out.extend_from_slice(&(w.len() as u32).to_le_bytes());
        out.extend_from_slice(w);
    }
    std::fs::write(path, out)?;
    Ok(())
}

fn path_string(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// Prepare one cohort into `control/cohort-<ordinal>` (`_cohort` up to `request.json`).
pub fn prepare(req: &PrepareInput, control: &Path, ordinal: usize) -> Result<Prepared> {
    let started = Instant::now();
    let mut phases: Vec<(&'static str, f64)> = Vec::new();
    let mut mark = Instant::now();
    let mut lap = |name: &'static str, phases: &mut Vec<(&'static str, f64)>| {
        phases.push((name, mark.elapsed().as_secs_f64()));
        mark = Instant::now();
    };
    let controls: &Controls = &req.controls;
    if !controls.stage_cell_subsets {
        return Err(Error::unsupported(
            "the Rust preparation stages cell subsets only (engines advertising staged_cells)",
        ));
    }
    let hdf_path = req.hdf_path.clone();
    let terrain_path = req.terrain_path.clone();
    if controls.max_profiles_per_batch == 0 {
        return Err(Error::invalid(
            "max_profiles_per_batch must be a positive integer",
        ));
    }
    if req.jobs.is_empty() || req.jobs.iter().any(|j| j.selectors.is_empty()) {
        return Err(Error::invalid(
            "Group timesteps must be unique nonnegative saved indices",
        ));
    }
    let max_prepared = mib(controls.max_prepared_mib, "max_prepared_mib")?;
    let max_working = mib(controls.max_working_mib, "max_working_mib")?;
    let max_accumulator = mib(controls.max_accumulator_mib, "max_accumulator_mib")?;
    // Sources were bound by the caller; refuse if any changed before we start.
    let job_id = req.jobs.first().map(|j| j.job_id.as_str()).unwrap_or("");
    check_job_sources(&req.sources, job_id)?;
    let hdf = Hdf::open(&hdf_path, controls.threads)?;
    // `FimPreparedSlopingModel._check_sources`: a changed source closes the
    // context; a geometry text file that appears re-runs the 1D guard.
    let check_sources = |geometry_absent: bool| -> Result<()> {
        let changed: Vec<String> = req
            .sources
            .iter()
            .filter(|s| file_identity(&s.path).ok() != Some(s.identity))
            .map(|s| crate::error::py_repr(&s.key))
            .collect();
        if !changed.is_empty() {
            return Err(Error::engine(format!(
                "Prepared sloping sources changed; create a new context: [{}]",
                changed.join(", ")
            )));
        }
        if geometry_absent {
            let scope = crate::scope::validate_2d_model(&hdf)?;
            if !scope["geometry_path"].is_null() {
                return Err(Error::engine(
                    "Source geometry appeared; create a new prepared context",
                ));
            }
        }
        Ok(())
    };
    let (depth_units, unit_metadata) = hdf.result_units(&path_string(&hdf_path))?;
    let jobs = &req.jobs;
    let mut source_indices: Vec<u64> = Vec::new();
    for j in jobs {
        for &t in &j.selectors {
            if !source_indices.contains(&t) {
                source_indices.push(t);
            }
        }
    }
    let name = |position: usize| format!("s{position:06}");
    let names: std::collections::HashMap<u64, String> = source_indices
        .iter()
        .enumerate()
        .map(|(k, &i)| (i, name(k)))
        .collect();
    let waves: Vec<Vec<u64>> = source_indices
        .chunks(controls.max_profiles_per_batch)
        .map(<[u64]>::to_vec)
        .collect();
    let root = control.join(format!("cohort-{ordinal}"));
    std::fs::create_dir(&root)?;
    let prepare_tick = Instant::now();

    // ---- RasFim.prepare_sloping_model(profiles={first}, invalid_cells=...) ----
    lap("open_and_units", &mut phases);
    let scope = crate::scope::validate_2d_model(&hdf)?;
    let geometry_absent = scope["geometry_path"].is_null();
    lap("scope", &mut phases);
    let validation_limit = (8u64 << 20).min(max_prepared / 8);
    let prepared_budget = max_prepared - validation_limit;
    let mesh_names = hdf.mesh_names()?;
    let cell_tick = Instant::now();
    let cells = cells::load(&hdf, &mesh_names, &req.invalid_cells, controls.threads)?;
    let cell_seconds = cell_tick.elapsed().as_secs_f64();
    lap("cells", &mut phases);
    let geometry_bytes = planned_geometry_bytes(&cells)?;
    if geometry_bytes >= prepared_budget {
        return Err(Error::unsupported(
            "Prepared geometry exceeds max_prepared_mib",
        ));
    }
    let terrain = Terrain::open(&terrain_path)?;
    crate::crs::require_model_crs(&hdf.crs_evidence()?, terrain.epsg)?;
    terrain.check_engine(&req.engine_path)?;
    let crs_identity = json!({"kind": "epsg"});
    // Python's `ras-fim-prepared-XXXXXXXX` has the same length.
    let prepared_dir = control.join(format!("ras-fim-prepared-rust{ordinal:04}"));
    std::fs::create_dir(&prepared_dir)?;
    let meshes = payload::write_meshes(
        &hdf,
        &mesh_names,
        &prepared_dir,
        (prepared_budget - geometry_bytes) as i64,
    )?;
    let mesh_bytes: u64 = meshes.iter().map(|m| m.bytes).sum();
    lap("terrain_crs_and_mesh_payloads", &mut phases);
    // The parent context stages one complete profile (counted, not written).
    let first_index = source_indices[0];
    let values_bytes: u64 = meshes.iter().map(|m| m.cell_count as u64 * 4).sum();
    if mesh_bytes + values_bytes > prepared_budget - geometry_bytes {
        return Err(Error::unsupported(
            "Prepared sloping inputs exceed max_prepared_mib",
        ));
    }
    // ras-commander's time-series view reads the saved-time axis first.
    let time_path = "Results/Unsteady/Output/Output Blocks/Base Output/Unsteady Time Series/Time";
    if hdf.dataset_shape(time_path)?.is_none() {
        return Err(Error::invalid(format!(
            "Path {time_path} not found in HDF file"
        )));
    }
    let time_counts = meshes
        .iter()
        .map(|m| {
            let shape = hdf
                .dataset_shape(&Hdf::water_surface_path(&m.name))?
                .unwrap_or_default();
            if shape.len() != 2 || shape[1] != m.cell_count as u64 {
                return Err(Error::invalid("Sloping profile source dimensions changed"));
            }
            Ok(shape[0])
        })
        .collect::<Result<Vec<_>>>()?;
    payload::check_parent_profile(&hdf, &meshes, &name(0), first_index, &time_counts)?;
    let info_staged = mesh_bytes + values_bytes;
    let planned_prepared = geometry_bytes + mesh_bytes + values_bytes + validation_limit;
    let invalid_cells = cells.describe(None)?;
    let prepared_info = json!({
        "cache_kind": "fast-map-prepare-staged-inputs/1",
        "profiles": {name(0): first_index},
        "physical_cells": cells.physical_cells(),
        "mesh_count": meshes.len(),
        "corner_elevations": meshes.iter().map(|m| (m.name.clone(), Value::Null)).collect::<Map<_, _>>(),
        "unique_selectors": 1,
        "profile_views_prepared": meshes.len(),
        "planned_geometry_bytes": geometry_bytes,
        "staged_bytes": info_staged,
        "planned_prepared_bytes": planned_prepared,
        "max_prepared_bytes": prepared_budget + validation_limit,
        "validation_cache_budget_bytes": validation_limit,
        "invalid_cells": invalid_cells,
        "native_state_reused": false,
        "native_session_enabled": req.persistent_session,
        "native_window_cache_budget_bytes": 0,
        "native_hydraulic_cache_budget_bytes": 0,
        "profile_staging_chunk_bytes": 8 << 20,
        "terrain_crs_identity": crs_identity,
        "cell_polygon_seconds": cell_seconds,
        "scope": scope,
    });
    check_sources(geometry_absent)?;

    // ---- per job: AOI, plan_grid, validation, AOI file ----
    let mut staged_bytes = planned_prepared;
    let mut source_wave =
        vec![json!({"indices": [], "staged_bytes": info_staged, "parent_profile": true})];
    let mut plans = Map::new();
    let mut native_jobs = Vec::new();
    let mut ownership: Vec<usize> = Vec::new();
    for job in jobs {
        let aoi: &Aoi = &job.aoi;
        let geometry = aoi.in_grid(terrain.epsg)?;
        cells.check_scope(Some(geometry))?;
        let plan: GridPlan = grid::plan_grid(&cells, &terrain, geometry, controls.max_working_mib)?;
        if plan.window[2] == 0 || plan.window[3] == 0 {
            return Err(Error::unsupported(
                "Grouped AOI must contain a nonempty native window",
            ));
        }
        grid::validate_grid(&cells, &plan)?;
        ownership.extend_from_slice(&plan.positions);
        plans.insert(
            job.job_id.clone(),
            json!({"shape": [plan.window[3], plan.window[2]], "transform": plan.transform.to_gdal(),
                   "window": plan.window, "epsg": terrain.epsg}),
        );
        let aoi_path = root.join(format!("{}.wkb", job.job_id));
        write_framed_wkb(&aoi_path, &[geom::wkb(geometry)?])?;
        native_jobs.push(json!({
            "job_id": job.job_id, "reach_id": job.job_id, "window": plan.window,
            "aoi_file": path_string(&aoi_path), "groups": job.groups,
        }));
    }
    lap("aoi_grid_validation", &mut phases);
    ownership.sort_unstable();
    ownership.dedup();
    let extra_bytes = ownership.len() as u64 * 256 + source_indices.len() as u64 * 256;
    let allowed = max_prepared;
    if staged_bytes + extra_bytes > allowed {
        return Err(Error::budget(format!(
            "Grouped preparation required {} bytes, allowed {allowed}",
            staged_bytes + extra_bytes
        )));
    }
    let addresses: Vec<[i64; 2]> = ownership
        .iter()
        .map(|&i| [cells.rows[i].mesh as i64, cells.rows[i].cell_id])
        .collect();
    let subsets = payload::staged_cell_subsets(&meshes, &addresses)?;
    let staged_files = payload::write_staged_cells(&root, &subsets)?;
    // stage_waves(0, subsets)
    let per_selector: u64 = subsets.iter().map(|s| s.len() as u64).sum();
    let mut staged = Vec::new();
    for (wave_index, wave) in waves.iter().enumerate() {
        let wave_root = root.join(format!("selectors-{wave_index}"));
        std::fs::create_dir(&wave_root)?;
        let required = staged_bytes + per_selector * 4 * wave.len() as u64;
        if required > allowed {
            return Err(Error::budget(format!(
                "Prepared selectors required {required} bytes, allowed {allowed} bytes"
            )));
        }
        let wanted: Vec<(String, u64)> = wave.iter().map(|i| (names[i].clone(), *i)).collect();
        let staged_reads = tile_layout(req.wse_tiles.as_ref(), &hdf_path, &meshes, wave)?;
        let (units, views, size) = payload::stage_profiles(
            &hdf,
            &wave_root,
            &meshes,
            &wanted,
            &subsets,
            allowed as i64 - staged_bytes as i64,
            &time_counts,
            !staged_reads,
        )?;
        staged.extend(units.into_iter().map(|u| {
            json!({"id": u.id, "values_files": u.values_files.iter().map(|p| path_string(p)).collect::<Vec<_>>()})
        }));
        staged_bytes += size as u64;
        let mut record =
            json!({"indices": wave, "staged_bytes": size, "profile_views_prepared": views});
        if staged_reads {
            record["wse_source"] = json!(STAGED_CELL_READS_SOURCE);
        }
        source_wave.push(record);
        check_sources(geometry_absent)?;
    }
    lap("staged_cells_and_wse", &mut phases);
    let staged_cells = json!({
        "method": STAGED_CELLS_METHOD,
        "staged_cell_counts": subsets.iter().map(Vec::len).collect::<Vec<_>>(),
        "parent_cell_counts": meshes.iter().map(|m| m.cell_count).collect::<Vec<_>>(),
    });
    if staged_bytes + extra_bytes > allowed {
        return Err(Error::budget(format!(
            "Grouped preparation required {} bytes, allowed {allowed}",
            staged_bytes + extra_bytes
        )));
    }
    let geometry_path = root.join("cells.wkb");
    let wkbs = ownership
        .iter()
        .map(|&i| geom::wkb(&cells.rows[i].geom))
        .collect::<Result<Vec<_>>>()?;
    write_framed_wkb(&geometry_path, &wkbs)?;
    let mut model = json!({
        "schema": "ras-commander-sloping-request/1",
        "terrain": path_string(&terrain_path),
        "epsg": terrain.epsg,
        "crs_identity": crs_identity,
        "terrain_transform": terrain.transform.to_gdal(),
        "window": native_jobs[0]["window"],
        "geometry_file": path_string(&geometry_path),
        "mesh_files": meshes.iter().map(|m| path_string(&m.header)).collect::<Vec<_>>(),
        "owner_cells": addresses,
        "staged_cells_files": staged_files.iter().map(|p| path_string(p)).collect::<Vec<_>>(),
        "profiles": staged,
        "threads": controls.threads,
        "max_inflight_profiles": controls.max_inflight_profiles,
        "max_working_bytes": max_working,
    });
    let cache = plan_window_cache(
        &terrain_path,
        &crs_identity,
        &meshes,
        &geometry_path,
        addresses.len(),
        &native_jobs,
        controls.window_cache_mib,
    )?;
    model["window_cache_bytes"] = json!(cache["effective_mib"].as_u64().unwrap() << 20);
    let request = json!({
        "schema": "ras-commander-depth-groups-request/1",
        "model": model,
        "source_indices": source_indices,
        "jobs": native_jobs,
        "depth_units": depth_units,
        "depth_cutoff_ft": controls.depth_cutoff_json.clone().unwrap_or(json!(controls.depth_cutoff_ft)),
        "products": ["depth_max"],
        "compression": controls.compression,
        "sink": "grouped-geotiff",
        "max_accumulator_bytes": max_accumulator,
        "scratch": {"path": path_string(control), "node_local": true, "max_bytes": controls.max_scratch_bytes},
    });
    let request_path = root.join("request.json");
    request::write_json(&request_path, &request)?;
    lap("cells_wkb_and_request", &mut phases);
    let preparation = json!({
        "waves": source_wave,
        "wave_count": waves.len(),
        "prepared_bytes": staged_bytes,
        "prepare_seconds": prepare_tick.elapsed().as_secs_f64(),
        "unit_metadata": unit_metadata,
        "invalid_cells": prepared_info["invalid_cells"],
        "ownership_cell_count": ownership.len(),
        "parent_physical_cell_count": cells.rows.len(),
        "staged_cells": staged_cells,
        "window_cache": cache,
        "preparation_engine": {"name": "fast-map-prepare", "version": env!("CARGO_PKG_VERSION"),
                                "total_seconds": started.elapsed().as_secs_f64(),
                                "phase_seconds": phases.iter().map(|(k, v)| (k.to_string(), json!(v))).collect::<Map<_, _>>()},
    });
    Ok(Prepared {
        root,
        request_path,
        request,
        plans,
        preparation,
        prepared_info,
    })
}
