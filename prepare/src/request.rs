//! The preparation request (`fast-map-depth-groups-preparation/1`) and job
//! normalisation (fim-commander `_depth_groups._normalise` and ras-commander
//! `_catalog._normalise_jobs`).

use crate::aoi::Aoi;
use crate::error::{Error, Result};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub const SCHEMA: &str = "fast-map-depth-groups-preparation/1";

/// Mapper controls after `map_depth_groups` normalisation.
#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct Controls {
    /// CPUs in the partition (`len(config["cpus"])`), also the request `threads`.
    pub threads: usize,
    pub max_working_mib: u64,
    pub max_prepared_mib: u64,
    pub max_profiles_per_batch: usize,
    pub max_inflight_profiles: u64,
    pub window_cache_mib: Option<u64>,
    pub depth_cutoff_ft: f64,
    pub compression: String,
    pub max_accumulator_mib: u64,
    pub max_scratch_bytes: u64,
    pub stage_cell_subsets: bool,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct GroupIn {
    pub group_id: String,
    pub timesteps: Vec<u64>,
}

/// The AOI: WKB of the polygon layer's features, in `epsg`.
#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct AoiIn {
    /// Files each holding one feature's WKB (as stored in GeoParquet/GeoPackage).
    #[serde(default)]
    pub wkb_files: Vec<PathBuf>,
    /// Hex WKB features, as an alternative to files.
    #[serde(default)]
    pub wkb_hex: Vec<String>,
    pub epsg: u32,
    #[serde(default)]
    pub source: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct JobIn {
    pub job_id: String,
    #[serde(default)]
    pub reach_id: Option<String>,
    pub aoi: AoiIn,
    pub groups: Vec<GroupIn>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct PrepareIn {
    pub schema: String,
    pub hdf_path: PathBuf,
    pub terrain_path: PathBuf,
    pub jobs: Vec<JobIn>,
    pub controls: Controls,
    #[serde(default = "default_policy")]
    pub invalid_cells: String,
    /// Fast-Map executable; `ras2fim-batch method` supplies its capabilities.
    pub engine_path: PathBuf,
}

fn default_policy() -> String {
    "repair".into()
}

/// A normalised job.
pub struct Job {
    pub job_id: String,
    pub reach_id: String,
    pub selectors: Vec<u64>,
    pub groups: Vec<Value>,
    pub aoi: Aoi,
}

impl JobIn {
    pub fn aoi_features(&self) -> Result<Vec<Vec<u8>>> {
        let mut out = Vec::new();
        for f in &self.aoi.wkb_files {
            out.push(std::fs::read(f).map_err(|e| Error::io(format!("{}: {e}", f.display())))?);
        }
        for h in &self.aoi.wkb_hex {
            if h.len() % 2 != 0 {
                return Err(Error::aoi("odd-length WKB hex"));
            }
            let bytes = (0..h.len())
                .step_by(2)
                .map(|i| {
                    u8::from_str_radix(&h[i..i + 2], 16).map_err(|_| Error::aoi("invalid WKB hex"))
                })
                .collect::<Result<Vec<_>>>()?;
            out.push(bytes);
        }
        Ok(out)
    }
}

impl JobIn {
    pub fn aoi_file(&self) -> Option<PathBuf> {
        self.aoi.wkb_files.first().cloned()
    }
}

const RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

fn portable(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        && !RESERVED.contains(&name.to_ascii_uppercase().as_str())
}

/// `_normalise` + `_normalise_jobs` for grouped plain-sloping jobs.
pub fn normalise(jobs: &[JobIn]) -> Result<Vec<Job>> {
    if jobs.is_empty() {
        return Err(Error::invalid("jobs cannot be empty"));
    }
    let mut ids = std::collections::HashSet::new();
    let mut out = Vec::new();
    for job in jobs {
        if !portable(&job.job_id) {
            return Err(Error::invalid("job_id must match [A-Za-z0-9_-]{1,64}"));
        }
        if !ids.insert(job.job_id.to_lowercase()) {
            return Err(Error::invalid(
                "job_id must be unique ignoring case and cannot be a reserved file name",
            ));
        }
        if job.groups.is_empty() {
            return Err(Error::invalid("groups must be a nonempty ordered sequence"));
        }
        let mut seen = std::collections::HashSet::new();
        let mut selectors: Vec<u64> = Vec::new();
        let mut groups = Vec::new();
        for g in &job.groups {
            if !portable(&g.group_id) || !seen.insert(g.group_id.to_lowercase()) {
                return Err(Error::invalid(
                    "group_id must be a unique portable identifier within its job",
                ));
            }
            let unique: std::collections::HashSet<_> = g.timesteps.iter().collect();
            if g.timesteps.is_empty() || unique.len() != g.timesteps.len() {
                return Err(Error::invalid(
                    "Group timesteps must be unique nonnegative saved indices",
                ));
            }
            for &t in &g.timesteps {
                if !selectors.contains(&t) {
                    selectors.push(t);
                }
            }
            groups.push(json!({"group_id": g.group_id, "timesteps": g.timesteps}));
        }
        let source = job
            .aoi
            .source
            .clone()
            .unwrap_or_else(|| format!("{} polygon", job.job_id));
        let aoi = Aoi::from_features(&job.aoi_features()?, job.aoi.epsg, &source)?;
        out.push(Job {
            job_id: job.job_id.clone(),
            reach_id: job.reach_id.clone().unwrap_or_else(|| job.job_id.clone()),
            selectors,
            groups,
            aoi,
        });
    }
    Ok(out)
}

/// `ras2fim-batch method`: the engine's method record.
pub fn engine_method(engine: &Path) -> Result<Value> {
    let out = std::process::Command::new(engine).arg("method").output()?;
    if !out.status.success() {
        return Err(Error::engine(format!(
            "ras2fim-batch method failed: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(serde_json::from_slice(&out.stdout)?)
}

impl PrepareIn {
    /// Validate against the engine's advertised depth-group capabilities, as
    /// `map_depth_groups` and `prepare_sloping_model` do.
    pub fn validate(self) -> Result<(crate::cohort::PrepareInput, Value)> {
        if self.schema != SCHEMA {
            return Err(Error::invalid(format!("schema must be {SCHEMA}")));
        }
        let c = &self.controls;
        if c.threads == 0 || c.threads > 64 {
            return Err(Error::invalid("threads must be 1..=64"));
        }
        if c.max_profiles_per_batch == 0 || c.max_profiles_per_batch > 4096 {
            return Err(Error::invalid("max_profiles_per_batch must be 1..=4096"));
        }
        if c.max_working_mib == 0 || c.max_prepared_mib == 0 || c.max_inflight_profiles == 0 {
            return Err(Error::invalid(
                "budgets and in-flight profiles must be positive",
            ));
        }
        if c.window_cache_mib.is_some_and(|v| v > 4096) {
            return Err(Error::invalid(
                "window_cache_mib must be None or an integer in 0..=4096",
            ));
        }
        if !c.depth_cutoff_ft.is_finite() || c.depth_cutoff_ft < 0.0 {
            return Err(Error::invalid(
                "depth_cutoff_ft must be finite and nonnegative",
            ));
        }
        if c.compression != "NONE" && c.compression != "LZW" {
            return Err(Error::invalid("compression must be NONE or LZW"));
        }
        if !crate::cells::POLICIES.contains(&self.invalid_cells.as_str()) {
            return Err(Error::invalid(
                "invalid_cells must be one of 'repair', 'hull', 'omit', 'raise'",
            ));
        }
        let method = engine_method(&self.engine_path)?;
        let sloping = &method["sloping"];
        let groups = &method["depth_groups"];
        if sloping["method_id"] != "rasmapper-plain-sloping/1" || sloping["stream_outputs"] != true
        {
            return Err(Error::engine(
                "engine lacks rasmapper-plain-sloping/1; install a matching engine",
            ));
        }
        if groups["request_schema"] != "ras-commander-depth-groups-request/1"
            || !groups["sinks"]
                .as_array()
                .is_some_and(|s| s.iter().any(|v| v == "grouped-geotiff"))
            || !groups["compression"]
                .as_array()
                .is_some_and(|s| s.iter().any(|v| v == c.compression.as_str()))
            || groups["aoi_mask"] != "global-terrain-lattice/1"
            || sloping["persistent_session"] != true
        {
            return Err(Error::unsupported(
                "Engine lacks required grouped depth capabilities",
            ));
        }
        if c.max_inflight_profiles > 1 && groups["bounded_profile_preparation"] != true {
            return Err(Error::unsupported(
                "Engine lacks bounded grouped profile preparation",
            ));
        }
        if c.stage_cell_subsets && groups["staged_cells"] != crate::cohort::STAGED_CELLS_METHOD {
            return Err(Error::unsupported(
                "Engine or Commander lacks staged cell subsets",
            ));
        }
        let persistent = sloping["persistent_session"] == true;
        let hdf_path = resolve(&self.hdf_path)?;
        let terrain_path = resolve(&self.terrain_path)?;
        let mut sources = vec![hdf_path.clone(), terrain_path.clone()];
        for j in &self.jobs {
            for f in &j.aoi.wkb_files {
                sources.push(resolve(f)?);
            }
        }
        let sources = sources
            .into_iter()
            .map(|p| Ok((p.clone(), crate::cohort::file_identity(&p)?)))
            .collect::<Result<Vec<_>>>()?;
        let jobs = normalise(&self.jobs)?;
        Ok((
            crate::cohort::PrepareInput {
                hdf_path,
                terrain_path,
                jobs,
                controls: self.controls,
                invalid_cells: self.invalid_cells,
                persistent_session: persistent,
                sources,
            },
            method,
        ))
    }
}

/// `Path.resolve(strict=True)` (Windows verbatim prefix removed).
pub fn resolve(p: &Path) -> Result<PathBuf> {
    let r = std::fs::canonicalize(p).map_err(|e| Error::io(format!("{}: {e}", p.display())))?;
    let s = r.to_string_lossy();
    Ok(PathBuf::from(
        s.strip_prefix(r"\\?\").unwrap_or(&s).to_string(),
    ))
}

/// `_write_json`: indent 2, then rename from `.partial`.
pub fn write_json(path: &Path, value: &Value) -> Result<()> {
    let partial = path.with_extension("partial");
    std::fs::write(&partial, serde_json::to_string_pretty(value)?)?;
    std::fs::rename(&partial, path)?;
    Ok(())
}
