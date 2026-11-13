//! Native depth maxima over saved output times, with separate reach masks.
//! The hydraulic renderer is unchanged; only finalized Float32 depths are reduced.
use crate::{
    MapError, model,
    native_output::Compression,
    sloping_session::{self, PreparedSession},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

pub const SCHEMA: &str = "ras-commander-depth-groups-request/1";
pub const REPORT_SCHEMA: &str = "ras-commander-depth-groups-report/1";
pub const METHOD: &str = "masked-float32-depth-group-maximum/1";
const CHUNK: usize = 16384;

#[derive(Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DepthUnits {
    Feet,
    Metres,
}

#[derive(Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
pub enum Sink {
    #[default]
    #[serde(rename = "grouped-geotiff")]
    Geotiff,
    #[serde(rename = "grouped-buffer")]
    Buffer,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub schema: String,
    /// Shared complete parent model. Profiles correspond exactly to source_indices.
    pub model: sloping_session::Request,
    pub source_indices: Vec<u64>,
    pub jobs: Vec<Job>,
    pub depth_units: DepthUnits,
    #[serde(default = "cutoff_default")]
    pub depth_cutoff_ft: f64,
    #[serde(default = "products_default")]
    pub products: Vec<String>,
    #[serde(default)]
    pub compression: Compression,
    #[serde(default)]
    pub sink: Sink,
    /// Ceiling on resident group payload; zero spills all groups.
    pub max_accumulator_bytes: u64,
    #[serde(default)]
    pub scratch: Option<Scratch>,
    /// For a model in a CRS without an EPSG code (`model.crs_geokeys`): the
    /// caller's WKT of that CRS, given verbatim as the group metadata's
    /// `crs_wkt` (GeoTIFF groups carry the GeoKey tags themselves). Required
    /// with `crs_geokeys`, refused without.
    #[serde(default)]
    pub crs_wkt: Option<String>,
}
fn cutoff_default() -> f64 {
    0.5
}
fn products_default() -> Vec<String> {
    vec!["depth_max".into()]
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scratch {
    /// Existing, explicitly provisioned node-local directory, never an output fallback.
    pub path: String,
    pub max_bytes: u64,
    pub node_local: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Job {
    pub job_id: String,
    pub reach_id: String,
    pub window: [usize; 4],
    #[serde(default)]
    pub aoi_file: Option<String>,
    pub groups: Vec<Group>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Group {
    pub group_id: String,
    pub timesteps: Vec<u64>,
}

pub fn method_record() -> Value {
    let mut record = json!({"method_id":METHOD,"request_schema":SCHEMA,"report_schema":REPORT_SCHEMA,
        "sinks":["grouped-geotiff","grouped-buffer"],"compression":["NONE","LZW"],
        "products":["depth_max"],"render_modes":["sloping"],"saved_timesteps_only":true,
        "aoi_mask":crate::aoi::METHOD,"local_spill":true,"max_inflight_profiles":1,
        "buffer_protocol":"ras-commander-depth-group-buffer/1",
        "staged_cells":crate::sloping_binary::STAGED_METHOD,
        "hydraulic_qualification":"native-per-time-depth-reduction/prepared-2d-component/1",
        "qualification_scope":"3 saved private mixed-fixture profiles, physical 2D component, inclusive 0.5 ft cutoff, full/cropped windows, ordered/reverse/single groups, resident NONE and local-spill LZW; not native Max-WSE rendering"});
    crate::crs::advertise(&mut record);
    record["crs_wkt"] =
        json!("required with model.crs_geokeys: the group metadata crs_wkt, verbatim");
    record
}

fn budget(what: &str, required: u64, allowed: u64) -> MapError {
    MapError::Budget(format!(
        "{what}: required {required} bytes, allowed {allowed} bytes"
    ))
}
fn add(a: u64, b: u64) -> Result<u64, MapError> {
    a.checked_add(b)
        .ok_or_else(|| budget("group plan overflow", u64::MAX, a))
}
fn mul(a: u64, b: u64) -> Result<u64, MapError> {
    a.checked_mul(b)
        .ok_or_else(|| budget("group plan overflow", u64::MAX, a))
}
fn pixels(job: &Job) -> Result<usize, MapError> {
    job.window[2]
        .checked_mul(job.window[3])
        .filter(|&n| n > 0 && n as u64 <= model::MAX_PROFILE_PIXELS)
        .ok_or_else(|| MapError::Invalid("group window empty or too large".into()))
}

impl Request {
    pub fn validate(&self) -> Result<f32, MapError> {
        match (self.model.crs_geokeys.is_some(), &self.crs_wkt) {
            (true, Some(wkt)) if !wkt.trim().is_empty() && wkt.len() <= 65_536 => {}
            (true, _) => {
                return Err(MapError::Invalid(
                    "a depth-groups model with crs_geokeys needs crs_wkt (at most 64 KiB)".into(),
                ));
            }
            (false, Some(_)) => {
                return Err(MapError::Invalid(
                    "crs_wkt belongs to crs_geokeys models only".into(),
                ));
            }
            (false, None) => {}
        }
        crate::crs::OutputCrs::resolve(self.model.epsg, self.model.crs_geokeys.as_ref())?;
        if self.schema != SCHEMA
            || self.jobs.is_empty()
            || self.source_indices.is_empty()
            || self.source_indices.len() != self.model.profiles.len()
            || self.products != ["depth_max"]
            || !self.depth_cutoff_ft.is_finite()
            || self.depth_cutoff_ft < 0.0
        {
            return Err(MapError::Invalid(
                "invalid depth-groups schema, jobs, selectors, products or cutoff".into(),
            ));
        }
        if self.compression == Compression::Deflate {
            return Err(MapError::Unsupported(
                "depth groups write NONE or LZW; DEFLATE belongs to final_format".into(),
            ));
        }
        self.model.options.validate()?;
        if self.model.options != crate::sloping::Options::default()
            || self.model.velocity_normals_files.is_some()
            || self.model.shear_stress.is_some()
            || !self.model.derived_products.is_empty()
            || self.model.reduce_max
            || self.model.stream_outputs
            || self.model.geotiff_outputs
            || self.model.max_inflight_profiles != 1
        {
            return Err(MapError::Unsupported("depth groups require plain sloping, no velocity/derived/output flags, and one in-flight profile".into()));
        }
        let mut selectors = HashSet::new();
        let mut profile_ids = HashSet::new();
        for (&index, profile) in self.source_indices.iter().zip(&self.model.profiles) {
            model::validate_id(&profile.id)?;
            if !selectors.insert(index) || !profile_ids.insert(model::id_key(&profile.id)) {
                return Err(MapError::Invalid(
                    "duplicate source index or profile ID".into(),
                ));
            }
        }
        let mut requested = HashSet::new();
        let mut jobs = HashSet::new();
        for job in &self.jobs {
            model::validate_id(&job.job_id)?;
            model::validate_id(&job.reach_id)?;
            pixels(job)?;
            if !jobs.insert(model::id_key(&job.job_id)) || job.groups.is_empty() {
                return Err(MapError::Invalid("duplicate job ID or empty groups".into()));
            }
            let mut groups = HashSet::new();
            for group in &job.groups {
                model::validate_id(&group.group_id)?;
                if !groups.insert(model::id_key(&group.group_id)) || group.timesteps.is_empty() {
                    return Err(MapError::Invalid(
                        "duplicate group ID or empty timesteps".into(),
                    ));
                }
                let mut seen = HashSet::new();
                for &index in &group.timesteps {
                    if !selectors.contains(&index) || !seen.insert(index) {
                        return Err(MapError::Invalid(
                            "missing or repeated group source index".into(),
                        ));
                    }
                    requested.insert(index);
                }
            }
        }
        if requested != selectors {
            return Err(MapError::Invalid("unrequested source indices".into()));
        }
        let threshold = (self.depth_cutoff_ft
            * match self.depth_units {
                DepthUnits::Feet => 1.0,
                DepthUnits::Metres => 0.3048,
            }) as f32;
        if !threshold.is_finite() {
            return Err(MapError::Invalid("cutoff exceeds Float32 range".into()));
        }
        Ok(threshold)
    }
}

/// Maxima select existing bits. Nodata/nonfinite values never acquire validity.
fn accumulate(acc: &mut [f32], depth: &[f32], threshold: f32) {
    for (a, &d) in acc.iter_mut().zip(depth) {
        if d.is_finite()
            && d != crate::sloping::NODATA
            && d >= threshold
            && (!a.is_finite() || d > *a)
        {
            *a = d;
        }
    }
}

enum Accumulator {
    Resident(Vec<f32>),
    Spill(PathBuf),
}
impl Accumulator {
    fn update(&mut self, depth: &[f32], threshold: f32) -> Result<(), MapError> {
        match self {
            Self::Resident(acc) => accumulate(acc, depth, threshold),
            Self::Spill(path) => {
                let mut file = OpenOptions::new().read(true).write(true).open(path)?;
                let mut bytes = vec![0; CHUNK * 4];
                let mut acc = vec![0.0; CHUNK];
                for (chunk, values) in depth.chunks(CHUNK).enumerate() {
                    let offset = (chunk * CHUNK * 4) as u64;
                    file.seek(SeekFrom::Start(offset))?;
                    file.read_exact(&mut bytes[..values.len() * 4])?;
                    for (a, b) in acc
                        .iter_mut()
                        .zip(bytes.as_chunks::<4>().0)
                        .take(values.len())
                    {
                        *a = f32::from_le_bytes(*b);
                    }
                    accumulate(&mut acc[..values.len()], values, threshold);
                    for (b, a) in bytes
                        .as_chunks_mut::<4>()
                        .0
                        .iter_mut()
                        .zip(&acc)
                        .take(values.len())
                    {
                        b.copy_from_slice(&a.to_le_bytes());
                    }
                    file.seek(SeekFrom::Start(offset))?;
                    file.write_all(&bytes[..values.len() * 4])?;
                }
            }
        }
        Ok(())
    }
    fn rows(
        &self,
        width: usize,
        height: usize,
        consume: &mut dyn FnMut(Vec<f32>, usize) -> Result<(), MapError>,
    ) -> Result<(), MapError> {
        let rows = crate::geotiff::strip_rows(width).min(height);
        match self {
            Self::Resident(data) => {
                for chunk in data.chunks(width * rows) {
                    consume(chunk.to_vec(), chunk.len() / width)?;
                }
            }
            Self::Spill(path) => {
                let mut file = File::open(path)?;
                for first in (0..height).step_by(rows) {
                    let nrows = rows.min(height - first);
                    let mut bytes = vec![0; nrows * width * 4];
                    file.read_exact(&mut bytes)?;
                    let data = bytes
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|b| f32::from_le_bytes(*b))
                        .collect();
                    consume(data, nrows)?;
                }
            }
        }
        Ok(())
    }
}

struct ScratchDir(PathBuf);
impl Drop for ScratchDir {
    fn drop(&mut self) {
        // Only this invocation's private directory is ever removed.
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn scratch_dir(spec: &Scratch, base: &Path) -> Result<ScratchDir, MapError> {
    let parent = model::resolve(base, &spec.path).canonicalize()?;
    let name = parent.to_string_lossy();
    if !spec.node_local
        || !parent.is_dir()
        || name.starts_with("\\\\?\\UNC\\")
        || (name.starts_with("\\\\") && !name.starts_with("\\\\?\\"))
    {
        return Err(MapError::Invalid(
            "scratch must be an explicitly provisioned node-local directory (UNC is refused)"
                .into(),
        ));
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| MapError::Io(e.to_string()))?
        .as_nanos();
    let dir = parent.join(format!("fast-map-groups-{}-{stamp}", std::process::id()));
    fs::create_dir(&dir)?;
    Ok(ScratchDir(dir))
}

/// Streaming consumer; methods are serialized with backpressure. Bytes are never JSON arrays.
pub trait BufferSink: Send {
    fn begin(&mut self, metadata: &Value) -> Result<(), MapError>;
    fn write(&mut self, values: &[f32]) -> Result<(), MapError>;
    fn end(&mut self) -> Result<(), MapError>;
}

/// JSON header line, exactly byte_length little-endian bytes, repeated; terminal JSON line.
pub struct BinarySink<W: Write>(pub W);
impl<W: Write + Send> BufferSink for BinarySink<W> {
    fn begin(&mut self, metadata: &Value) -> Result<(), MapError> {
        writeln!(
            self.0,
            "{}",
            json!({"schema":"ras-commander-depth-group-buffer/1","group":metadata})
        )?;
        Ok(())
    }
    fn write(&mut self, values: &[f32]) -> Result<(), MapError> {
        crate::horizontal::write_f32(&mut self.0, values)
    }
    fn end(&mut self) -> Result<(), MapError> {
        self.0.flush()?;
        Ok(())
    }
}

/// An owned Rust delivery adapter. Reserve the largest group payload in admission.
pub struct OwnedBufferSink<F> {
    callback: F,
    metadata: Value,
    values: Vec<f32>,
}
impl<F> OwnedBufferSink<F> {
    pub fn new(callback: F) -> Self {
        Self {
            callback,
            metadata: Value::Null,
            values: Vec::new(),
        }
    }
}
impl<F: FnMut(&Value, Vec<f32>) -> Result<(), MapError> + Send> BufferSink for OwnedBufferSink<F> {
    fn begin(&mut self, metadata: &Value) -> Result<(), MapError> {
        self.metadata = metadata.clone();
        self.values = Vec::with_capacity(metadata["byte_length"].as_u64().unwrap() as usize / 4);
        Ok(())
    }
    fn write(&mut self, values: &[f32]) -> Result<(), MapError> {
        self.values.extend_from_slice(values);
        Ok(())
    }
    fn end(&mut self) -> Result<(), MapError> {
        (self.callback)(&self.metadata, std::mem::take(&mut self.values))
    }
}

pub fn run(req: &Request, base: &Path, out: &Path) -> Result<Value, MapError> {
    let mut session = PreparedSession::default();
    run_with_session(req, base, out, &mut session, None)
}

/// Output is tentative until the aggregate receipt and terminal status are complete.
/// A buffer consumer owns any memory it retains after `end`; callers budget it separately.
pub fn run_with_session(
    req: &Request,
    base: &Path,
    out: &Path,
    session: &mut PreparedSession,
    mut buffer: Option<&mut dyn BufferSink>,
) -> Result<Value, MapError> {
    let threshold = req.validate()?;
    if (req.sink == Sink::Buffer) != buffer.is_some() {
        return Err(MapError::Invalid(
            "grouped-buffer requires a buffer sink; grouped-geotiff does not".into(),
        ));
    }
    if out.exists() {
        return Err(MapError::Invalid(
            "choose a new group output directory".into(),
        ));
    }
    let started = Instant::now();
    let initial_counts = session.preparation_counts();
    let mut resident = 0;
    let mut spilled = 0;
    let mut largest = 0;
    let mut receipt_bytes = 0;
    let mut writer_bytes = 0;
    let mut layout = Vec::new();
    for job in &req.jobs {
        let bytes = mul(pixels(job)? as u64, 4)?;
        largest = largest.max(bytes);
        // Row writer has offsets/counts and finish-directory copies, plus bounded row buffers.
        writer_bytes = writer_bytes.max(add(
            mul(job.window[3] as u64, 128)?,
            mul((job.window[2] as u64).max(8192), 64)?,
        )?);
        for group in &job.groups {
            let keep = add(resident, bytes)? <= req.max_accumulator_bytes;
            if keep {
                resident = add(resident, bytes)?;
            } else {
                spilled = add(spilled, bytes)?;
            }
            receipt_bytes = add(
                receipt_bytes,
                add(16384, mul(group.timesteps.len() as u64, 8192)?)?,
            )?;
            layout.push(keep);
        }
    }
    if spilled > 0 {
        let allowed = req.scratch.as_ref().map_or(0, |s| s.max_bytes);
        if spilled > allowed {
            return Err(budget("local scratch", spilled, allowed));
        }
    }
    // One owned-buffer output is allowed even when the accumulator lives on disk.
    // Reserve two full request clones, index/receipt trees, writer buffers and transfer scratch.
    let request_bytes = add(mul(req.model.owner_cells.len() as u64, 64)?, receipt_bytes)?;
    let reserved = add(
        add(resident, largest)?,
        add(add(writer_bytes, request_bytes)?, 4 << 20)?,
    )?;
    if reserved >= req.model.max_working_bytes {
        return Err(budget(
            "group resident/output plan",
            reserved,
            req.model.max_working_bytes,
        ));
    }
    let mut model = req.model.clone();
    model.max_working_bytes -= reserved;
    // Planning loads a staged compact geometry: its load is charged against
    // the budget left after the group reserve, not the unlimited planning probe.
    model.load_budget = Some(model.max_working_bytes);
    let prepared_before = session.preparation_counts().1;
    let mut peak_mapping = 0;
    // All windows are admitted before terrain payload reads or accumulator allocation.
    // Header/descriptors are inspected by the existing sloping planner.
    model.profiles.truncate(1);
    let planned = (|| -> Result<u64, MapError> {
        let mut peak = 0;
        for job in &req.jobs {
            model.window = job.window;
            model.aoi_file = job.aoi_file.clone();
            let available = model.max_working_bytes;
            model.max_working_bytes = u64::MAX;
            let plan = sloping_session::plan_with_session(&model, base, session);
            model.max_working_bytes = available;
            let plan = plan?;
            let total = add(reserved, plan)?;
            if total > req.model.max_working_bytes {
                return Err(budget(
                    "mapping plus group plan",
                    total,
                    req.model.max_working_bytes,
                ));
            }
            peak = peak.max(plan);
        }
        Ok(peak)
    })();
    // A refused request does not keep geometry it prepared while planning.
    if planned.is_err() && session.preparation_counts().1 != prepared_before {
        session.release_geometry();
    }
    peak_mapping = peak_mapping.max(planned?);
    let scratch = if spilled > 0 {
        Some(scratch_dir(req.scratch.as_ref().unwrap(), base)?)
    } else {
        None
    };
    fs::create_dir(out)?;
    let mut report = json!({"schema":REPORT_SCHEMA,"method":method_record(),"status":"in_progress",
        "engine":{"version":crate::ENGINE_VERSION,"source_commit":crate::ENGINE_COMMIT,"terrio":"0.4.2"},
        "depth_units":req.depth_units,"depth_cutoff_ft":req.depth_cutoff_ft,"effective_cutoff":threshold,
        "planned_bytes":add(reserved,peak_mapping)?,"resident_accumulator_bytes":resident,
        "scratch_peak_bytes":spilled,"scratch_read_bytes":0,"scratch_write_bytes":0,
        "preparation_waves":1,"geometry_preparations":0,"terrain_preparations":0,
        "hydraulic_preparations":0,"effective_inflight_profiles":1,"threads":model.threads,
        "compute_seconds":0.0,"prepare_seconds":started.elapsed().as_secs_f64(),"reduce_seconds":0.0,"write_seconds":0.0,
        "jobs":req.jobs.iter().map(|job| json!({"job_id":job.job_id,"reach_id":job.reach_id,
            "groups":job.groups.iter().map(|group| json!({"group_id":group.group_id,
                "source_indices":group.timesteps,"status":"in_progress",
                "frames":group.timesteps.iter().map(|i| json!({"source_index":i,"status":"pending"})).collect::<Vec<_>>()
            })).collect::<Vec<_>>() })).collect::<Vec<_>>() });
    crate::sink::write_json(&out.join("report.json"), &report)?;
    let mut delivered = Vec::new();
    let execution = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
        || -> Result<(), MapError> {
            let setup = Instant::now();
            let terrain_identity =
                sloping_session::identity(&model::resolve(base, &req.model.terrain))?;
            let parent_key = sloping_session::geometry_key(&req.model, base)?;
            let parent_identity = format!(
                "fnv1a64:{:016x}",
                crate::sink::fnv1a64(parent_key.as_bytes())
            );
            let terrain = session.install(|| {
                crate::terrain::inspect(&model::resolve(base, &req.model.terrain).to_string_lossy())
            })?;
            // The TIFF reader can reconstruct approximate legacy WKT. Delivery
            // uses the canonical registry definition of the already validated EPSG.
            let crs =
                crate::crs::OutputCrs::resolve(req.model.epsg, req.model.crs_geokeys.as_ref())?;
            let wkt = match &crs {
                crate::crs::OutputCrs::Epsg(code) => terrio::crs::SpatialRef::from_epsg(*code)
                    .map_err(|e| MapError::Unsupported(format!("group CRS definition: {e}")))?
                    .to_wkt()
                    .ok_or_else(|| MapError::Invalid("group CRS has no WKT".into()))?,
                // A CRS without an EPSG code: the caller's WKT, verbatim.
                crate::crs::OutputCrs::GeoKeys(_) => req.crs_wkt.clone().unwrap_or_default(),
            };
            let mut accumulators = Vec::new();
            let mut slot = 0;
            for job in &req.jobs {
                for _group in &job.groups {
                    let n = pixels(job)?;
                    if layout[slot] {
                        accumulators.push(Accumulator::Resident(vec![f32::NAN; n]));
                    } else {
                        let path = scratch.as_ref().unwrap().0.join(format!("{slot}.f32"));
                        let mut file = OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .open(&path)?;
                        let chunk: Vec<_> =
                            (0..CHUNK).flat_map(|_| f32::NAN.to_le_bytes()).collect();
                        let mut remaining = n * 4;
                        while remaining > 0 {
                            let count = remaining.min(chunk.len());
                            file.write_all(&chunk[..count])?;
                            remaining -= count;
                        }
                        accumulators.push(Accumulator::Spill(path));
                    }
                    slot += 1;
                }
            }
            report["prepare_seconds"] = (report["prepare_seconds"].as_f64().unwrap()
                + setup.elapsed().as_secs_f64())
            .into();
            report["scratch_write_bytes"] = spilled.into();
            crate::sink::write_json(&out.join("report.json"), &report)?;
            let mut group_reduce_seconds = vec![0.0; layout.len()];
            let mut frames: Vec<HashMap<u64, Value>> =
                req.jobs.iter().map(|_| HashMap::new()).collect();
            for (profile, &source_index) in req.model.profiles.iter().zip(&req.source_indices) {
                session.begin_grouped_profile();
                model.profiles[0] = profile.clone();
                let mut offset = 0;
                for (j, job) in req.jobs.iter().enumerate() {
                    if !job
                        .groups
                        .iter()
                        .any(|g| g.timesteps.contains(&source_index))
                    {
                        offset += job.groups.len();
                        continue;
                    }
                    model.window = job.window;
                    model.aoi_file = job.aoi_file.clone();
                    let mut seen = false;
                    let mut consume = |_: &sloping_session::Profile,
                                       depth: Vec<f32>,
                                       timing: Value|
                     -> Result<(), MapError> {
                        if seen || depth.len() != pixels(job)? {
                            return Err(MapError::Internal(
                                "repeated or malformed rendered frame".into(),
                            ));
                        }
                        seen = true;
                        let native_wet =
                            depth.iter().filter(|d| d.is_finite() && **d > 0.0).count();
                        let retained = depth
                            .iter()
                            .filter(|d| {
                                d.is_finite() && **d != crate::sloping::NODATA && **d >= threshold
                            })
                            .count();
                        let tick = Instant::now();
                        for (g, group) in job
                            .groups
                            .iter()
                            .enumerate()
                            .filter(|(_, g)| g.timesteps.contains(&source_index))
                        {
                            let _ = group;
                            let tick = Instant::now();
                            accumulators[offset + g].update(&depth, threshold)?;
                            group_reduce_seconds[offset + g] += tick.elapsed().as_secs_f64();
                            if !layout[offset + g] {
                                for key in ["scratch_read_bytes", "scratch_write_bytes"] {
                                    report[key] = add(
                                        report[key].as_u64().unwrap(),
                                        mul(depth.len() as u64, 4)?,
                                    )?
                                    .into();
                                }
                            }
                        }
                        let seconds = tick.elapsed().as_secs_f64();
                        report["reduce_seconds"] =
                            (report["reduce_seconds"].as_f64().unwrap() + seconds).into();
                        let frame = json!({"source_index":source_index,
                        "status":if native_wet == 0 {"no_flooded_pixels"} else {"complete"},
                        "native_wet_count":native_wet,"retained_count":retained,
                        "hydraulic_seconds":timing["hydraulic_seconds"],"render_seconds":timing["render_seconds"],
                        "hydraulic_reused":timing["hydraulic_reused"]});
                        for (g, group) in job.groups.iter().enumerate() {
                            if let Some(position) =
                                group.timesteps.iter().position(|i| *i == source_index)
                            {
                                report["jobs"][j]["groups"][g]["frames"][position] = frame.clone();
                            }
                        }
                        frames[j].insert(source_index, frame);
                        Ok(())
                    };
                    let mapped =
                        sloping_session::run_to_sink(&model, base, out, session, &mut consume)?;
                    if report.get("staged_cells").is_none()
                        && let Some(record) = mapped.get("staged_cells")
                    {
                        report["staged_cells"] = record.clone();
                    }
                    if mapped["status"] != "complete" || !seen {
                        return Err(MapError::Invalid(format!(
                            "incomplete mapped sample {source_index}: {}",
                            mapped["error"]
                        )));
                    }
                    for (key, flag) in [
                        ("geometry_preparations", "geometry_reused"),
                        ("terrain_preparations", "terrain_reused"),
                    ] {
                        if mapped["session"][flag] != true {
                            report[key] = (report[key].as_u64().unwrap() + 1).into();
                        }
                    }
                    if mapped["profiles"][0]["hydraulic_reused"] != true {
                        report["hydraulic_preparations"] =
                            (report["hydraulic_preparations"].as_u64().unwrap() + 1).into();
                    }
                    report["compute_seconds"] = (report["compute_seconds"].as_f64().unwrap()
                        + mapped["profiles"][0]["render_seconds"].as_f64().unwrap())
                    .into();
                    report["prepare_seconds"] = (report["prepare_seconds"].as_f64().unwrap()
                        + mapped["geometry_prepare_seconds"].as_f64().unwrap()
                        + mapped["ownership_prepare_seconds"].as_f64().unwrap()
                        + mapped["terrain_prepare_seconds"].as_f64().unwrap()
                        + mapped["profiles"][0]["hydraulic_seconds"].as_f64().unwrap())
                    .into();
                    offset += job.groups.len();
                }
                session.end_grouped_profile();
            }
            let mut slot = 0;
            for (j, job) in req.jobs.iter().enumerate() {
                let [col, row, width, height] = job.window;
                let gt = req.model.terrain_transform;
                let transform = [
                    gt[0] + col as f64 * gt[1],
                    gt[1],
                    0.0,
                    gt[3] + row as f64 * gt[5],
                    0.0,
                    gt[5],
                ];
                for (g, group) in job.groups.iter().enumerate() {
                    let tick = Instant::now();
                    let ordered: Vec<_> = group
                        .timesteps
                        .iter()
                        .map(|i| {
                            frames[j].get(i).cloned().ok_or_else(|| {
                                MapError::Internal("missing group contribution".into())
                            })
                        })
                        .collect::<Result<_, _>>()?;
                    let hydraulic_seconds: f64 = ordered
                        .iter()
                        .map(|f| f["hydraulic_seconds"].as_f64().unwrap())
                        .sum();
                    let compute_seconds: f64 = ordered
                        .iter()
                        .map(|f| f["render_seconds"].as_f64().unwrap())
                        .sum();
                    let mut metadata = json!({"job_id":job.job_id,"group_id":group.group_id,"shape":[height,width],
                    "byte_length":mul(pixels(job)? as u64,4)?,"dtype":"<f4","nodata":"NaN",
                    "units":req.depth_units,"depth_cutoff_ft":req.depth_cutoff_ft,"effective_cutoff":threshold,
                    "affine":transform,"strict_extent":[transform[0],transform[3]+height as f64*gt[5],transform[0]+width as f64*gt[1],transform[3]],
                    "crs_wkt":wkt,"epsg":crs.epsg(),
                    "crs_wkt_fnv1a64":format!("{:016x}",crate::sink::fnv1a64(wkt.as_bytes())),
                    "lattice":{"terrain_transform":gt,"terrain_shape":[terrain.height,terrain.width],
                        "source_window":job.window,"terrain_identity":terrain_identity,"parent_identity":parent_identity,"terrain_file_bytes":fs::metadata(model::resolve(base,&req.model.terrain))?.len(),
                        "source_crs_wkt_fnv1a64":terrain.crs_wkt_fnv1a64,"aoi_mask":crate::aoi::METHOD},
                    "source_indices":group.timesteps,"frames":ordered,
                    "engine":report["engine"],"timings":{"hydraulic_seconds":hydraulic_seconds,"compute_seconds":compute_seconds,
                        "reduce_seconds":group_reduce_seconds[slot]}});
                    if let Some(keys) = crs.geokeys() {
                        metadata["output_crs"] = crs.echo();
                        // The keys the group TIFF carries (PixelIsPoint is
                        // written PixelIsArea), matching output_crs.
                        metadata["crs_geokeys"] = json!(keys.written());
                    }
                    let mut writer = if req.sink == Sink::Geotiff {
                        let dir = out.join(&job.job_id);
                        if !dir.exists() {
                            fs::create_dir(&dir)?;
                        }
                        let path = dir.join(format!("{}.tif.partial", group.group_id));
                        delivered.push(path.clone());
                        Some(session.install(|| {
                            crate::geotiff::create_then_finish_crs(
                                &path,
                                width,
                                height,
                                transform,
                                &crs,
                                req.compression,
                            )
                        })?)
                    } else {
                        buffer.as_mut().unwrap().begin(&metadata)?;
                        None
                    };
                    let mut valid = 0u64;
                    let mut hash = 0xcbf29ce484222325u64;
                    session.install(|| {
                        accumulators[slot].rows(width, height, &mut |data, rows| {
                            for &value in &data {
                                valid += u64::from(value.is_finite());
                                for byte in value.to_le_bytes() {
                                    hash = (hash ^ byte as u64).wrapping_mul(0x100000001b3);
                                }
                            }
                            // `rows` yields one `geotiff::strip_rows` chunk per call, one
                            // TIFF block row of this writer: terrio encodes it on this worker
                            // without its pool (as in `geotiff::RowWriter::write_rows`).
                            if let Some(writer) = writer.as_mut() {
                                writer
                                    .write_rows(
                                        &wbraster::raster::RasterData::F32(data),
                                        rows,
                                        None,
                                    )
                                    .map_err(|e| {
                                        MapError::Io(format!("group GeoTIFF write: {e}"))
                                    })?;
                            } else {
                                buffer.as_mut().unwrap().write(&data)?;
                            }
                            Ok(())
                        })
                    })?;
                    if let Some(writer) = writer {
                        // Finalization can reach terrio's pool: off the worker.
                        crate::terrio_pool::call(|| writer.finish())?
                            .map_err(|e| MapError::Io(format!("group GeoTIFF finish: {e}")))?;
                        crate::geotiff::finish_crs(delivered.last().unwrap(), &crs)?;
                        let final_path = out
                            .join(&job.job_id)
                            .join(format!("{}.tif", group.group_id));
                        fs::rename(delivered.last().unwrap(), &final_path)?;
                        delivered.push(final_path.clone());
                        metadata["output"] = json!({"path":format!("{}/{}.tif",job.job_id,group.group_id),"compression":req.compression,"bytes":fs::metadata(&final_path)?.len()});
                    } else {
                        buffer.as_mut().unwrap().end()?;
                        metadata["output"] = json!({"sink":"grouped-buffer"});
                    }
                    metadata["status"] = "complete".into();
                    metadata["retained_count"] = valid.into();
                    metadata["all_dry"] = (valid == 0).into();
                    metadata["output"]["float32_fnv1a64"] = format!("{hash:016x}").into();
                    let seconds = tick.elapsed().as_secs_f64();
                    metadata["write_seconds"] = seconds.into();
                    metadata["timings"]["write_seconds"] = seconds.into();
                    report["write_seconds"] =
                        (report["write_seconds"].as_f64().unwrap() + seconds).into();
                    if !layout[slot] {
                        report["scratch_read_bytes"] = add(
                            report["scratch_read_bytes"].as_u64().unwrap(),
                            mul(pixels(job)? as u64, 4)?,
                        )?
                        .into();
                    }
                    report["jobs"][j]["groups"][g] = metadata;
                    slot += 1;
                }
            }
            Ok(())
        },
    ));
    session.end_grouped_profile();
    match execution {
        Ok(Ok(())) => report["status"] = "complete".into(),
        failure => {
            report["status"] = "aborted".into();
            report["error"] = match failure {
                Ok(Err(e)) => e.to_string(),
                _ => "grouped mapping panic".into(),
            }
            .into();
            for path in &delivered {
                let _ = fs::remove_file(path);
            }
            for job in report["jobs"].as_array_mut().unwrap() {
                for group in job["groups"].as_array_mut().unwrap() {
                    group["status"] = "failed".into();
                    group.as_object_mut().unwrap().remove("output");
                }
            }
        }
    }
    let counts = session.preparation_counts();
    report["terrain_preparations"] = (counts.0 - initial_counts.0).into();
    report["geometry_preparations"] = (counts.1 - initial_counts.1).into();
    report["total_seconds"] = started.elapsed().as_secs_f64().into();
    if let Err(error) = crate::sink::write_json(&out.join("report.json"), &report) {
        for path in &delivered {
            let _ = fs::remove_file(path);
        }
        report["status"] = "aborted".into();
        report["error"] = error.to_string().into();
        for job in report["jobs"].as_array_mut().unwrap() {
            for group in job["groups"].as_array_mut().unwrap() {
                group["status"] = "failed".into();
                group.as_object_mut().unwrap().remove("output");
            }
        }
        let _ = crate::sink::write_json(&out.join("report.json"), &report);
        return Err(error);
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cutoff_and_nodata_preserve_selected_bits() {
        let before = f32::from_bits(0.5f32.to_bits() - 1);
        let after = f32::from_bits(0.5f32.to_bits() + 1);
        let mut a = vec![f32::NAN; 8];
        accumulate(
            &mut a,
            &[
                before,
                0.5,
                after,
                f32::NAN,
                f32::INFINITY,
                crate::sloping::NODATA,
                10001.0,
                2.0,
            ],
            0.5,
        );
        assert!(a[0].is_nan() && a[3..6].iter().all(|v| v.is_nan()));
        assert_eq!(a[1].to_bits(), 0.5f32.to_bits());
        assert_eq!(a[2].to_bits(), after.to_bits());
        assert_eq!(a[6], 10001.0);
        accumulate(&mut a, &[0.5; 8], 0.5);
        assert_eq!(a[7], 2.0);
    }
}
