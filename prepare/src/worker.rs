//! Drop-in grouped worker: `fast-map-prepare worker <config.json>` does what
//! `python -m fim_commander.mapping._depth_groups <config.json>` (`_worker`) does,
//! with no Python: cohorts by job key, the Rust preparation, the unchanged
//! engine's `sloping-server` session, report validation (`validate_receipt`),
//! GeoTIFF checks and delivery, and `control/result.json`.
//!
//! The supervisor (`map_depth_groups`) is unchanged: it writes the config,
//! samples memory, enforces the timeout and writes the receipt.

use crate::aoi::Aoi;
use crate::cohort::{self, PrepareInput, file_identity};
use crate::error::{Error, Kind, Result};
use crate::geom;
use crate::request::{Controls, Job};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The Python exception class name `_worker` records for each refusal kind.
pub fn python_class(kind: Kind) -> &'static str {
    match kind {
        Kind::Unsupported => "FimUnsupportedError",
        Kind::Invalid => "ValueError",
        Kind::Aoi => "FimAoiError",
        Kind::MeshCellPolygon => "MeshCellPolygonError",
        Kind::Budget => "MemoryError",
        Kind::Engine => "FimEngineError",
        Kind::Io => "OSError",
    }
}

fn write_json(path: &Path, value: &Value) -> Result<()> {
    crate::request::write_json(path, value)
}

fn s<'a>(v: &'a Value, what: &str) -> Result<&'a str> {
    v.as_str()
        .ok_or_else(|| Error::invalid(format!("config {what} must be a string")))
}

fn controls(config: &Value) -> Result<Controls> {
    let c = &config["controls"];
    let u = |k: &str| {
        c[k].as_u64()
            .ok_or_else(|| Error::invalid(format!("controls.{k} must be an integer")))
    };
    let cpus = config["cpus"].as_array().map_or(0, Vec::len);
    if cpus == 0 {
        return Err(Error::invalid("config cpus must be a nonempty list"));
    }
    Ok(Controls {
        threads: cpus,
        max_working_mib: u("max_working_mib")?,
        max_prepared_mib: u("max_prepared_mib")?,
        max_profiles_per_batch: u("max_profiles_per_batch")? as usize,
        max_inflight_profiles: u("max_inflight_profiles")?,
        window_cache_mib: c["window_cache_mib"].as_u64(),
        depth_cutoff_ft: c["depth_cutoff_ft"]
            .as_f64()
            .ok_or_else(|| Error::invalid("controls.depth_cutoff_ft"))?,
        compression: s(&c["compression"], "controls.compression")?.to_string(),
        max_accumulator_mib: u("max_accumulator_mib")?,
        max_scratch_bytes: u("max_scratch_bytes")?,
        stage_cell_subsets: c["stage_cell_subsets"].as_bool().unwrap_or(false),
    })
}

fn hex(h: &str) -> Result<Vec<u8>> {
    if !h.len().is_multiple_of(2) {
        return Err(Error::aoi("odd-length WKB hex"));
    }
    (0..h.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&h[i..i + 2], 16).map_err(|_| Error::aoi("invalid WKB hex")))
        .collect()
}

/// One cohort's normalised input from `_normalise` job records.
fn cohort_input(jobs: &[Value], config: &Value) -> Result<PrepareInput> {
    let first = &jobs[0];
    let options = &first["options"];
    if options["render_mode"] != "sloping"
        || options["reduce_shallow_to_horizontal"] != false
        || options["use_depth_weighted_faces"] != false
        || first["interpolation"] != "sloping"
    {
        return Err(Error::unsupported(
            "Grouped maxima initially require plain sloping",
        ));
    }
    if !options["postprocessing_hdf"].is_null() {
        return Err(Error::unsupported(
            "the Rust preparation does not read a postprocessing HDF",
        ));
    }
    let mut sources = Vec::new();
    let mut normal = Vec::new();
    for job in jobs {
        for src in job["sources"]
            .as_object()
            .into_iter()
            .flat_map(|m| m.values())
        {
            let path = PathBuf::from(s(&src["path"], "sources.path")?);
            let id = src["identity"]
                .as_array()
                .ok_or_else(|| Error::invalid("sources.identity"))?;
            let size = id
                .first()
                .and_then(Value::as_u64)
                .ok_or_else(|| Error::invalid("sources.identity"))?;
            let mtime = id
                .get(1)
                .and_then(Value::as_i64)
                .ok_or_else(|| Error::invalid("sources.identity"))?;
            sources.push((path, (size, i128::from(mtime))));
        }
        let aoi = &job["aoi"];
        let crs = s(&aoi["crs"], "aoi.crs")?;
        let epsg = terrio::crs::SpatialRef::from_wkt(crs).epsg.ok_or_else(|| {
            Error::unsupported("AOI CRS has no EPSG identity; reprojection is not supported")
        })?;
        let geometry = geom::from_wkb(&hex(s(&aoi["wkb"], "aoi.wkb")?)?)?;
        let groups: Vec<Value> = job["groups"].as_array().cloned().unwrap_or_default();
        let selectors = job["timesteps"]
            .as_array()
            .ok_or_else(|| Error::invalid("job timesteps"))?
            .iter()
            .map(|v| {
                v.as_u64()
                    .ok_or_else(|| Error::invalid("timesteps must be saved indices"))
            })
            .collect::<Result<Vec<_>>>()?;
        normal.push(Job {
            job_id: s(&job["job_id"], "job_id")?.to_string(),
            reach_id: s(&job["reach_id"], "reach_id")?.to_string(),
            selectors,
            groups,
            aoi: Aoi {
                geometry,
                epsg,
                source: aoi["source"].as_str().unwrap_or("").to_string(),
            },
        });
    }
    Ok(PrepareInput {
        hdf_path: PathBuf::from(s(&first["hdf_path"], "hdf_path")?),
        terrain_path: PathBuf::from(s(&first["terrain_path"], "terrain_path")?),
        jobs: normal,
        controls: controls(config)?,
        invalid_cells: s(&options["invalid_cells"], "options.invalid_cells")?.to_string(),
        persistent_session: config["engine"]["method"]["sloping"]["persistent_session"] == true,
        wse_tiles: config.get("wse_tiles").cloned(),
        sources,
    })
}

/// First-IFD TIFF tags needed to check a delivered GeoTIFF.
struct TiffHeader {
    width: u64,
    height: u64,
    samples: u64,
    bits: u64,
    format: u64,
    compression: u64,
    nodata: Option<String>,
    scale: Vec<f64>,
    tie: Vec<f64>,
    epsg: Option<u64>,
}

fn tiff_header(path: &Path) -> Result<TiffHeader> {
    let mut f = std::fs::File::open(path)?;
    let mut head = [0u8; 16];
    f.read_exact(&mut head[..8])?;
    let le = match &head[..2] {
        b"II" => true,
        b"MM" => false,
        _ => return Err(Error::engine("delivered file is not a TIFF")),
    };
    let rd = |b: &[u8], n: usize| -> u64 {
        let mut v = 0u64;
        for k in 0..n {
            let byte = if le { b[n - 1 - k] } else { b[k] };
            v = (v << 8) | u64::from(byte);
        }
        v
    };
    let big = match rd(&head[2..4], 2) {
        42 => false,
        43 => {
            f.read_exact(&mut head[8..16])?;
            true
        }
        _ => return Err(Error::engine("delivered file is not a TIFF")),
    };
    let first = if big {
        rd(&head[8..16], 8)
    } else {
        rd(&head[4..8], 4)
    };
    f.seek(SeekFrom::Start(first))?;
    let mut nb = [0u8; 8];
    let count = if big {
        f.read_exact(&mut nb)?;
        rd(&nb, 8)
    } else {
        f.read_exact(&mut nb[..2])?;
        rd(&nb[..2], 2)
    };
    let size = if big { 20 } else { 12 };
    let mut entries = vec![0u8; count as usize * size];
    f.read_exact(&mut entries)?;
    let mut h = TiffHeader {
        width: 0,
        height: 0,
        samples: 1,
        bits: 0,
        format: 1,
        compression: 1,
        nodata: None,
        scale: Vec::new(),
        tie: Vec::new(),
        epsg: None,
    };
    let type_size = |t: u64| match t {
        1 | 2 | 6 | 7 => 1,
        3 | 8 => 2,
        4 | 9 | 11 => 4,
        5 | 10 | 12 | 16 | 17 => 8,
        _ => 0,
    };
    for e in entries.chunks_exact(size) {
        let tag = rd(&e[0..2], 2);
        let typ = rd(&e[2..4], 2);
        let n = if big {
            rd(&e[4..12], 8)
        } else {
            rd(&e[4..8], 4)
        };
        let inline = if big { &e[12..20] } else { &e[8..12] };
        let total = type_size(typ) * n as usize;
        let data: Vec<u8> = if total <= inline.len() {
            inline[..total].to_vec()
        } else {
            let off = if big { rd(inline, 8) } else { rd(inline, 4) };
            let here = f.stream_position()?;
            f.seek(SeekFrom::Start(off))?;
            let mut buf = vec![0u8; total];
            f.read_exact(&mut buf)?;
            f.seek(SeekFrom::Start(here))?;
            buf
        };
        let ints = || -> Vec<u64> {
            let w = type_size(typ);
            if w == 0 {
                return Vec::new();
            }
            data.chunks_exact(w).map(|c| rd(c, w)).collect()
        };
        let doubles = || -> Vec<f64> {
            data.as_chunks::<8>()
                .0
                .iter()
                .map(|c| f64::from_bits(rd(c, 8)))
                .collect()
        };
        match tag {
            256 => h.width = ints()[0],
            257 => h.height = ints()[0],
            258 => h.bits = ints()[0],
            259 => h.compression = ints()[0],
            277 => h.samples = ints()[0],
            339 => h.format = ints()[0],
            33550 => h.scale = doubles(),
            33922 => h.tie = doubles(),
            42113 => {
                h.nodata = Some(
                    String::from_utf8_lossy(&data)
                        .trim_end_matches('\0')
                        .trim()
                        .to_string(),
                )
            }
            34735 => {
                let keys = ints();
                for k in keys.as_chunks::<4>().0.iter().skip(1) {
                    if k[0] == 3072 && k[1] == 0 {
                        h.epsg = Some(k[3]);
                    }
                }
            }
            _ => {}
        }
    }
    Ok(h)
}

/// `validate_receipt(report, jobs, plans, depth_units, compression)`.
fn validate_report(
    report: &Value,
    jobs: &[Value],
    plans: &serde_json::Map<String, Value>,
    depth_units: &str,
    compression: &str,
) -> Result<()> {
    let bad = |m: &str| Error::engine(m.to_string());
    if report["schema"] != "ras-commander-depth-groups-report/1" || report["status"] != "complete" {
        return Err(bad("Incomplete grouped native receipt"));
    }
    let actual = report["jobs"]
        .as_array()
        .ok_or_else(|| bad("Grouped receipt jobs differ from request order"))?;
    if actual.len() != jobs.len()
        || actual
            .iter()
            .zip(jobs)
            .any(|(a, j)| a["job_id"] != j["job_id"])
    {
        return Err(bad("Grouped receipt jobs differ from request order"));
    }
    for (job, delivered) in jobs.iter().zip(actual) {
        let groups = delivered["groups"]
            .as_array()
            .ok_or_else(|| bad("Grouped receipt groups differ from request order"))?;
        let requested = job["groups"].as_array().cloned().unwrap_or_default();
        if groups.len() != requested.len()
            || groups
                .iter()
                .zip(&requested)
                .any(|(g, r)| g["group_id"] != r["group_id"])
        {
            return Err(bad("Grouped receipt groups differ from request order"));
        }
        let plan = &plans[job["job_id"].as_str().unwrap_or_default()];
        let shape: Vec<i64> = serde_json::from_value(plan["shape"].clone())?;
        let t: Vec<f64> = serde_json::from_value(plan["transform"].clone())?;
        let window = &plan["window"];
        let pixels = (shape[0] * shape[1]) as u64;
        let extent = json!([
            t[0],
            t[3] + shape[0] as f64 * t[5],
            t[0] + shape[1] as f64 * t[1],
            t[3]
        ]);
        for (req, group) in requested.iter().zip(groups) {
            let crs_ok = group["crs_wkt"]
                .as_str()
                .map(|w| {
                    terrio::crs::SpatialRef::from_wkt(w).epsg
                        == plan["epsg"].as_u64().map(|e| e as u32)
                })
                .unwrap_or(false);
            let frames = group["frames"].as_array();
            let frame_ids: Option<Vec<Value>> =
                frames.map(|f| f.iter().map(|x| x["source_index"].clone()).collect());
            let path = format!(
                "{}/{}.tif",
                job["job_id"].as_str().unwrap_or(""),
                req["group_id"].as_str().unwrap_or("")
            );
            if group["status"] != "complete"
                || group["source_indices"] != req["timesteps"]
                || frame_ids.as_ref() != req["timesteps"].as_array()
                || group["shape"] != json!(shape)
                || group["affine"] != json!(t)
                || group["strict_extent"] != extent
                || !crs_ok
                || group["epsg"] != plan["epsg"]
                || group["lattice"]["source_window"] != *window
                || group["units"] != depth_units
                || group["nodata"] != "NaN"
                || group["byte_length"].as_u64() != Some(pixels * 4)
                || group["dtype"] != "<f4"
                || group["output"]["path"] != path.as_str()
                || group["output"]["compression"] != compression
            {
                return Err(bad(
                    "Grouped output metadata or sample list differs from request",
                ));
            }
            let count = group["retained_count"]
                .as_u64()
                .ok_or_else(|| bad("Invalid grouped retained pixel count"))?;
            if count > pixels || group["all_dry"] != (count == 0) {
                return Err(bad("Invalid grouped retained pixel count"));
            }
            for frame in frames.into_iter().flatten() {
                let native = frame["native_wet_count"].as_u64();
                let retained = frame["retained_count"].as_u64();
                let ok = frame["source_index"].is_u64()
                    && native.is_some_and(|n| n <= pixels)
                    && retained.is_some_and(|r| r <= pixels)
                    && frame["status"]
                        == if native.unwrap_or(0) > 0 {
                            "complete"
                        } else {
                            "no_flooded_pixels"
                        };
                if !ok {
                    return Err(bad(
                        "Unknown/failed grouped frame status or invalid pixel count",
                    ));
                }
            }
        }
    }
    Ok(())
}

fn move_file(source: &Path, target: &Path) -> Result<()> {
    let partial = target.with_extension("tif.partial");
    if std::fs::rename(source, &partial).is_err() {
        std::fs::copy(source, &partial)?;
        std::fs::remove_file(source)?;
    }
    std::fs::rename(&partial, target)?;
    Ok(())
}

/// The engine's persistent `sloping-server` with one command (`NativeSlopingSession`).
fn run_session(engine: &Path, request: &Path, native_out: &Path, errors: &Path) -> Result<()> {
    let err = std::fs::File::create(errors)?;
    let mut child = Command::new(engine)
        .arg("sloping-server")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(err)
        .spawn()?;
    let command = format!(
        "{}\n",
        json!({"request": request.to_string_lossy(), "output": native_out.to_string_lossy()})
    );
    let outcome = (|| -> Result<String> {
        if command.len() > 65536 {
            return Err(Error::engine(
                "Native session command exceeds protocol limit",
            ));
        }
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| Error::engine("no session stdin"))?;
        stdin
            .write_all(command.as_bytes())
            .map_err(|_| Error::engine("Prepared native session command failed"))?;
        stdin.flush()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::engine("no session stdout"))?;
        let mut line = String::new();
        BufReader::new(stdout.take(65537)).read_line(&mut line)?;
        drop(stdin);
        Ok(line)
    })();
    let _ = child.kill();
    let _ = child.wait();
    let tail = || {
        let text = std::fs::read(errors).unwrap_or_default();
        String::from_utf8_lossy(&text[text.len().saturating_sub(4000)..]).into_owned()
    };
    let line = outcome?;
    if line.len() > 65536 || !line.ends_with('\n') {
        return Err(Error::engine(format!(
            "Missing grouped session completion: {}",
            tail()
        )));
    }
    let terminal: Value = serde_json::from_str(line.trim_end())?;
    if terminal != json!({"command_status": "complete"}) {
        let mut detail = tail();
        if let Ok(text) = std::fs::read_to_string(native_out.join("report.json"))
            && let Ok(v) = serde_json::from_str::<Value>(&text)
            && let Some(e) = v["error"].as_str()
        {
            detail = e.to_string();
        }
        return Err(Error::engine(format!(
            "Grouped native command failed: {detail}"
        )));
    }
    Ok(())
}

/// One cohort: prepare, run the engine, validate, deliver (`_cohort`, no consumer).
pub fn run_cohort(jobs: &[Value], config: &Value, control: &Path, ordinal: usize) -> Result<Value> {
    let input = cohort_input(jobs, config)?;
    let prepared = cohort::prepare(&input, control, ordinal)?;
    let engine = PathBuf::from(s(&config["engine"]["engine_path"], "engine.engine_path")?);
    let native_out = prepared.root.join("native");
    for (p, id) in &input.sources {
        if file_identity(p)? != *id {
            return Err(Error::engine(format!(
                "Catalog source changed: {}",
                p.display()
            )));
        }
    }
    run_session(
        &engine,
        &prepared.request_path,
        &native_out,
        &prepared.root.join("engine.stderr"),
    )?;
    let mut report: Value =
        serde_json::from_str(&std::fs::read_to_string(native_out.join("report.json"))?)?;
    let depth_units = prepared.request["depth_units"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    validate_report(
        &report,
        jobs,
        &prepared.plans,
        &depth_units,
        &input.controls.compression,
    )?;
    let factor = if depth_units == "metres" { 0.3048 } else { 1.0 };
    let threshold = f64::from((input.controls.depth_cutoff_ft * factor) as f32);
    if report["effective_cutoff"].as_f64() != Some(threshold) {
        return Err(Error::engine(
            "Native grouped cutoff differs from physical feet request",
        ));
    }
    let output = PathBuf::from(s(&config["output"], "output")?);
    let lzw = input.controls.compression == "LZW";
    let delivered_jobs = report["jobs"]
        .as_array_mut()
        .ok_or_else(|| Error::engine("report jobs"))?;
    for (job, delivered) in jobs.iter().zip(delivered_jobs.iter_mut()) {
        delivered["reach_id"] = job["reach_id"].clone();
        let job_id = s(&job["job_id"], "job_id")?;
        let plan = &prepared.plans[job_id];
        let shape: Vec<u64> = serde_json::from_value(plan["shape"].clone())?;
        let t: Vec<f64> = serde_json::from_value(plan["transform"].clone())?;
        let destination = output.join(job_id);
        std::fs::create_dir_all(&destination)?;
        for group in delivered["groups"].as_array_mut().into_iter().flatten() {
            let source = native_out.join(s(&group["output"]["path"], "output.path")?);
            let h = tiff_header(&source)?;
            let gt = if h.scale.len() >= 2 && h.tie.len() >= 6 {
                [
                    h.tie[3] - h.tie[0] * h.scale[0],
                    h.scale[0],
                    0.0,
                    h.tie[4] + h.tie[1] * h.scale[1],
                    0.0,
                    -h.scale[1],
                ]
            } else {
                [f64::NAN; 6]
            };
            let nodata_nan = h
                .nodata
                .as_deref()
                .is_some_and(|v| v.eq_ignore_ascii_case("nan"));
            if [h.height, h.width] != [shape[0], shape[1]]
                || gt.to_vec() != t
                || h.epsg != plan["epsg"].as_u64()
                || h.samples != 1
                || h.bits != 32
                || h.format != 3
                || !nodata_nan
                || h.compression != if lzw { 5 } else { 1 }
            {
                return Err(Error::engine(
                    "Grouped GeoTIFF metadata differs from requested lattice",
                ));
            }
            let target = destination.join(source.file_name().unwrap_or_default());
            move_file(&source, &target)?;
            group["output"]["path"] = json!(target.to_string_lossy());
        }
    }
    for (p, id) in &input.sources {
        if file_identity(p)? != *id {
            return Err(Error::engine(format!(
                "Catalog source changed: {}",
                p.display()
            )));
        }
    }
    let mut record = prepared.preparation.clone();
    let engine_record = record
        .as_object_mut()
        .and_then(|m| m.remove("preparation_engine"));
    report["commander_preparation"] = record;
    report["preparation_engine"] = engine_record.unwrap_or(Value::Null);
    Ok(report)
}

#[cfg(target_os = "linux")]
fn set_affinity(_cpus: &[usize]) {
    // The supervisor's CPU partition is recorded in the request (`threads`);
    // the engine child inherits this process's affinity from the supervisor.
}
#[cfg(not(target_os = "linux"))]
fn set_affinity(_cpus: &[usize]) {}

/// `_worker(config_path)`.
pub fn run(config_path: &Path) -> Result<()> {
    let config: Value = serde_json::from_str(&std::fs::read_to_string(config_path)?)?;
    let control = PathBuf::from(s(&config["control"], "control")?);
    let cpus: Vec<usize> = config["cpus"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_u64().map(|v| v as usize))
        .collect();
    set_affinity(&cpus);
    let mut result = json!({"status": "in_progress", "cohorts": []});
    write_json(&control.join("result.json"), &result)?;
    let mut order: Vec<String> = Vec::new();
    let mut grouped: std::collections::HashMap<String, Vec<Value>> = Default::default();
    for job in config["jobs"].as_array().into_iter().flatten() {
        let key = job["key"].as_str().unwrap_or_default().to_string();
        if !grouped.contains_key(&key) {
            order.push(key.clone());
        }
        grouped.entry(key).or_default().push(job.clone());
    }
    for (ordinal, key) in order.iter().enumerate() {
        match run_cohort(&grouped[key], &config, &control, ordinal) {
            Ok(report) => {
                result["cohorts"].as_array_mut().unwrap().push(report);
                write_json(&control.join("result.json"), &result)?;
            }
            Err(e) => {
                result["status"] = json!("aborted");
                result["error"] = json!(format!("{}: {}", python_class(e.kind), e.message));
                write_json(&control.join("result.json"), &result)?;
                return Err(e);
            }
        }
    }
    result["status"] = json!("complete");
    write_json(&control.join("result.json"), &result)
}
