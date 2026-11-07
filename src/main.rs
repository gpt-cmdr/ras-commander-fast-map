//! CLI: `ras2fim-batch run <session.json> <new-output-dir>`, `method`, `terrain-info <tif>`.
//!
//! Exit codes: 0 all units complete/empty; 2 session ran but a profile failed/aborted;
//! 1 invalid request or session error. When this run created the output directory, a session
//! error still leaves `report.json` with `status: "aborted"` (best effort, see `session::run`);
//! no report exists when the request was rejected before the directory was created.
use ras_commander_fast_map::{model::SessionRequest, session, terrain};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const USAGE: &str = "usage:\n  ras2fim-batch run <session.json> <new-output-dir>\n  ras2fim-batch boundary <request.json> <new-output-dir>\n  ras2fim-batch horizontal <request.json> <new-output-dir>\n  ras2fim-batch whole-simulation <request.json> <new-output-dir>\n  ras2fim-batch sloping <request.json> <new-output-dir>\n  ras2fim-batch depth-groups <request.json> <new-output-dir>\n  ras2fim-batch step05 <request.json> <new-output-dir>\n  ras2fim-batch effective-terrain <request.json> <new-output-dir>\n  ras2fim-batch one-d-diagnostic <request.json> <new-output-dir>\n  ras2fim-batch coupled-diagnostic <request.json> <new-output-dir>\n  ras2fim-batch method\n  ras2fim-batch terrain-info <terrain.tif>";

/// Samples this process's OS thread count (Linux /proc) to evidence no nested decode threads.
struct ThreadMonitor {
    stop: Arc<AtomicBool>,
    max: Arc<AtomicUsize>,
    samples: Arc<AtomicUsize>,
    handle: Option<std::thread::JoinHandle<()>>,
}

fn os_threads() -> Option<usize> {
    std::fs::read_dir("/proc/self/task").ok().map(|d| d.count())
}

impl ThreadMonitor {
    fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let max = Arc::new(AtomicUsize::new(0));
        let samples = Arc::new(AtomicUsize::new(0));
        let (s, m, n) = (stop.clone(), max.clone(), samples.clone());
        let handle = os_threads().map(|_| {
            std::thread::spawn(move || {
                while !s.load(Ordering::Relaxed) {
                    if let Some(c) = os_threads() {
                        m.fetch_max(c, Ordering::Relaxed);
                        n.fetch_add(1, Ordering::Relaxed);
                    }
                    std::thread::sleep(std::time::Duration::from_micros(200));
                }
            })
        });
        Self {
            stop,
            max,
            samples,
            handle,
        }
    }
    fn finish(mut self) -> serde_json::Value {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
            serde_json::json!({"max_observed": self.max.load(Ordering::Relaxed),
                               "samples": self.samples.load(Ordering::Relaxed),
                               "method": "count /proc/self/task every ~200us; expected max = pool threads + main + monitor"})
        } else {
            serde_json::json!(null)
        }
    }
}

/// Diagnostic requests contain metadata only; numeric payloads are binary.
/// Bound JSON before deserialization, including unknown-field arrays/strings.
fn read_diagnostic_wire(
    path: &std::path::Path,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    use std::io::Read;
    const LIMIT: u64 = 65536;
    let file = std::fs::File::open(path)?;
    if file.metadata()?.len() > LIMIT {
        return Err("diagnostic JSON request exceeds 65536 bytes".into());
    }
    Ok(ras_commander_fast_map::native_output::read_wire(
        file.take(LIMIT + 1),
    )?)
}

fn run() -> Result<i32, Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let cmd = args.first().and_then(|a| a.to_str()).unwrap_or("");
    match (cmd, args.len()) {
        ("coupled-diagnostic", 3) => {
            let path = PathBuf::from(&args[1]);
            let out = PathBuf::from(&args[2]);
            let wire = read_diagnostic_wire(&path)?;
            let req = serde_json::from_value(wire)?;
            let absolute = path.canonicalize()?;
            let base = absolute.parent().ok_or("request has no parent")?;
            let report = ras_commander_fast_map::coupled_session::run(&req, base, &out)?;
            println!("{}", serde_json::to_string(&report)?);
            Ok(0)
        }
        ("one-d-diagnostic", 3) => {
            let path = PathBuf::from(&args[1]);
            let out = PathBuf::from(&args[2]);
            if out.exists() {
                return Err("output directory exists; choose a new path".into());
            }
            let wire = read_diagnostic_wire(&path)?;
            let req = serde_json::from_value(wire)?;
            let absolute = path.canonicalize()?;
            let base = absolute.parent().ok_or("request has no parent")?;
            let report = ras_commander_fast_map::one_d_session::run(&req, base, &out)?;
            println!("{}", serde_json::to_string(&report)?);
            Ok(0)
        }
        ("sloping-server", 1) => {
            use std::io::{BufRead, Read, Write};
            let mut session = ras_commander_fast_map::sloping_session::PreparedSession::default();
            let mut input = std::io::BufReader::new(std::io::stdin().lock());
            loop {
                let mut line = String::new();
                if input.by_ref().take(65537).read_line(&mut line)? == 0 {
                    break;
                }
                if line.len() > 65536 || !line.ends_with('\n') {
                    return Err("invalid session command".into());
                }
                let command: serde_json::Value = serde_json::from_str(&line)?;
                if command["close"] == true {
                    break;
                }
                let path = PathBuf::from(command["request"].as_str().ok_or("missing request")?);
                let out = PathBuf::from(command["output"].as_str().ok_or("missing output")?);
                let wire =
                    ras_commander_fast_map::native_output::read_wire(std::fs::File::open(&path)?)?;
                let base = path
                    .canonicalize()?
                    .parent()
                    .ok_or("missing base")?
                    .to_path_buf();
                let result = if wire["schema"] == ras_commander_fast_map::depth_groups::SCHEMA {
                    let req = serde_json::from_value(wire)?;
                    run_groups(&req, &base, &out, &mut session)?
                } else {
                    let (req, output_options) =
                        ras_commander_fast_map::native_output::parse_request(wire)?;
                    ras_commander_fast_map::sloping_session::run_with_output_options(
                        &req,
                        &base,
                        &out,
                        Some(&mut session),
                        output_options.as_ref(),
                    )?
                };
                println!("{}", serde_json::json!({"command_status":result["status"]}));
                std::io::stdout().flush()?;
                if result["status"] != "complete" {
                    return Ok(2);
                }
            }
            Ok(0)
        }
        ("method", 1) => {
            let m = ras_commander_fast_map::engine_method_record();
            println!("{}", serde_json::to_string_pretty(&m)?);
            Ok(0)
        }
        ("effective-terrain", 3) => {
            let path = PathBuf::from(&args[1]);
            let out = PathBuf::from(&args[2]);
            if out.exists() {
                return Err("output directory exists; choose a new path".into());
            }
            let req: ras_commander_fast_map::effective_terrain::Request =
                serde_json::from_reader(std::io::BufReader::new(std::fs::File::open(&path)?))?;
            let base = path
                .canonicalize()?
                .parent()
                .map(PathBuf::from)
                .ok_or("request has no parent")?;
            let report = ras_commander_fast_map::effective_terrain::run(&req, &base, &out)?;
            println!(
                "{}",
                serde_json::json!({"status": report["status"], "valid_pixels": report["valid_pixels"], "seconds": report["seconds"]})
            );
            Ok(0)
        }
        ("terrain-info", 2) => {
            let p = args[1].to_str().ok_or("terrain path is not UTF-8")?;
            println!("{}", serde_json::to_string_pretty(&terrain::inspect(p)?)?);
            Ok(0)
        }
        ("run", 3) => {
            let session_path = PathBuf::from(&args[1]);
            let out = PathBuf::from(&args[2]);
            if out.exists() {
                return Err("output directory exists; choose a new path".into());
            }
            let req = SessionRequest::load(&session_path)?;
            let base = session_path
                .canonicalize()?
                .parent()
                .map(PathBuf::from)
                .ok_or("session path has no parent")?;
            let monitor = ThreadMonitor::start();
            let result = session::run(&req, &base, &out);
            let threads = monitor.finish();
            let mut report = result?;
            report["os_threads"] = threads.clone();
            report["os_threads"]["expected_max"] = (req.budgets.threads + 2).into();
            ras_commander_fast_map::sink::write_json(&out.join("report.json"), &report)?;
            let summary = serde_json::json!({
                "status": report["status"],
                "counters": report["counters"],
                "cache": report["terrain"]["stats"]["cache"],
                "os_threads": threads,
                "total_wall_seconds": report["timings"]["total_wall_seconds"],
            });
            println!("{summary}");
            Ok(if report["status"] == "complete" { 0 } else { 2 })
        }
        ("depth-groups", 3) => {
            let path = PathBuf::from(&args[1]);
            let out = PathBuf::from(&args[2]);
            let req = ras_commander_fast_map::model::read_json(&path)?;
            let base = path
                .canonicalize()?
                .parent()
                .ok_or("missing request base")?
                .to_path_buf();
            let mut session = ras_commander_fast_map::sloping_session::PreparedSession::default();
            let report = run_groups(&req, &base, &out, &mut session)?;
            println!("{}", serde_json::json!({"command_status":report["status"]}));
            Ok(if report["status"] == "complete" { 0 } else { 2 })
        }
        ("sloping", 3) => {
            let path = PathBuf::from(&args[1]);
            let out = PathBuf::from(&args[2]);
            if out.exists() {
                return Err("output directory exists; choose a new path".into());
            }
            let wire = ras_commander_fast_map::native_output::read_wire(std::io::BufReader::new(
                std::fs::File::open(&path)?,
            ))?;
            let (req, output_options) = ras_commander_fast_map::native_output::parse_request(wire)?;
            let base = path
                .canonicalize()?
                .parent()
                .map(PathBuf::from)
                .ok_or("request has no parent")?;
            let report = ras_commander_fast_map::sloping_session::run_with_output_options(
                &req,
                &base,
                &out,
                None,
                output_options.as_ref(),
            )?;
            if !req.stream_outputs {
                println!("{}", serde_json::to_string(&report)?);
            }
            Ok(if report["status"] == "complete" { 0 } else { 2 })
        }
        ("boundary", 3) => {
            let path = PathBuf::from(&args[1]);
            let out = PathBuf::from(&args[2]);
            if out.exists() {
                return Err("output directory exists; choose a new path".into());
            }
            let req =
                serde_json::from_reader(std::io::BufReader::new(std::fs::File::open(&path)?))?;
            let base = path
                .canonicalize()?
                .parent()
                .ok_or("request has no parent")?
                .to_path_buf();
            let report = ras_commander_fast_map::boundary_session::run(&req, &base, &out)?;
            println!("{}", serde_json::to_string(&report)?);
            Ok(if report["status"] == "complete" { 0 } else { 2 })
        }
        ("whole-simulation", 3) => {
            let request_path = PathBuf::from(&args[1]);
            let out = PathBuf::from(&args[2]);
            if out.exists() {
                return Err("output directory exists; choose a new path".into());
            }
            let req: ras_commander_fast_map::whole_simulation::Request =
                ras_commander_fast_map::model::read_json(&request_path)?;
            let base = request_path
                .canonicalize()?
                .parent()
                .ok_or("request has no parent")?
                .to_path_buf();
            let report = ras_commander_fast_map::whole_simulation::run(&req, &base, &out)?;
            println!(
                "{}",
                serde_json::json!({"status":report["status"],"total_seconds":report["total_seconds"]})
            );
            Ok(if report["status"] == "complete" { 0 } else { 2 })
        }
        ("horizontal", 3) => {
            let request_path = PathBuf::from(&args[1]);
            let out = PathBuf::from(&args[2]);
            if out.exists() {
                return Err("output directory exists; choose a new path".into());
            }
            let req: ras_commander_fast_map::horizontal::Request =
                ras_commander_fast_map::model::read_json(&request_path)?;
            let base = request_path
                .canonicalize()?
                .parent()
                .ok_or("request has no parent")?
                .to_path_buf();
            let report = ras_commander_fast_map::horizontal::run(&req, &base, &out)?;
            if !req.stream_outputs {
                println!(
                    "{}",
                    serde_json::json!({"status":report["status"],"total_seconds":report["total_seconds"]})
                );
            }
            Ok(if report["status"] == "complete" { 0 } else { 2 })
        }
        ("step05", 3) => {
            let request_path = PathBuf::from(&args[1]);
            let out = PathBuf::from(&args[2]);
            if out.exists() {
                return Err("output directory exists; choose a new path".into());
            }
            let req = ras_commander_fast_map::step05::Request::load(&request_path)?;
            let base = request_path
                .canonicalize()?
                .parent()
                .map(PathBuf::from)
                .ok_or("request path has no parent")?;
            let monitor = ThreadMonitor::start();
            let result = ras_commander_fast_map::step05::run(&req, &base, &out);
            let threads = monitor.finish();
            let mut report = match result {
                Ok(r) => r,
                Err(e) => {
                    // The output directory may exist: record the session error durably.
                    if out.is_dir() {
                        let failed = serde_json::json!({"schema": "ras2fim-step05-report/1", "status": "error",
                            "error_kind": e.kind(), "error": e.to_string()});
                        let _ = ras_commander_fast_map::sink::write_json(
                            &out.join("report.json"),
                            &failed,
                        );
                    }
                    return Err(e.into());
                }
            };
            report["os_threads"] = threads;
            report["os_threads"]["expected_max"] = (req.budgets.threads + 2).into();
            ras_commander_fast_map::sink::write_json(&out.join("report.json"), &report)?;
            let summary = serde_json::json!({"status": report["status"], "lookup_cache": report["lookup_cache"],
                "total_wall_seconds_diagnostic": report["total_wall_seconds_diagnostic"]});
            println!("{summary}");
            Ok(if report["status"] == "complete" { 0 } else { 2 })
        }
        _ => Err(USAGE.into()),
    }
}

fn run_groups(
    req: &ras_commander_fast_map::depth_groups::Request,
    base: &std::path::Path,
    out: &std::path::Path,
    session: &mut ras_commander_fast_map::sloping_session::PreparedSession,
) -> Result<serde_json::Value, ras_commander_fast_map::MapError> {
    use ras_commander_fast_map::depth_groups::{self, BinarySink, Sink};
    if req.sink == Sink::Buffer {
        // Stdout itself is Send; its locked guard is not. The writer owns buffering.
        let mut sink = BinarySink(std::io::BufWriter::new(std::io::stdout()));
        depth_groups::run_with_session(req, base, out, session, Some(&mut sink))
    } else {
        depth_groups::run_with_session(req, base, out, session, None)
    }
}

fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
