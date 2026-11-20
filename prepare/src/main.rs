//! `fast-map-prepare prepare <request.json> <control-dir>`
//! `fast-map-prepare worker <config.json>` (drop-in for fim-commander's grouped worker)
//!
//! Writes `<control-dir>/cohort-0/request.json` (ras-commander-depth-groups-request/1),
//! its binary inputs, and `<control-dir>/cohort-0/preparation.json`.
use fast_map_prepare::{cohort, request};
use std::path::PathBuf;
use std::process::ExitCode;

fn run() -> fast_map_prepare::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("prepare") if args.len() == 4 => {
            let text = std::fs::read_to_string(&args[2])?;
            let input: request::PrepareIn = serde_json::from_str(&text)?;
            let (req, engine_method) = input.validate()?;
            let control = PathBuf::from(&args[3]);
            std::fs::create_dir_all(&control)?;
            let prepared = cohort::prepare(&req, &control, 0)?;
            let record = serde_json::json!({
                "schema": "fast-map-depth-groups-preparation-result/1",
                "request_path": prepared.request_path.to_string_lossy(),
                "plans": prepared.plans,
                "preparation": prepared.preparation,
                "prepared_info": prepared.prepared_info,
                "engine_method": engine_method,
            });
            request::write_json(&prepared.root.join("preparation.json"), &record)?;
            println!("{}", prepared.request_path.display());
            Ok(())
        }
        Some("cells") if args.len() == 4 => {
            // Parity probe: `_invalid_cells.load_cells` + `require_cells` rows and records.
            let hdf = fast_map_prepare::hdf::Hdf::open(std::path::Path::new(&args[2]), 1)?;
            let names = hdf.mesh_names()?;
            let cells = fast_map_prepare::cells::load(&hdf, &names, &args[3], 1)?;
            let rows: Vec<serde_json::Value> = cells
                .rows
                .iter()
                .map(|r| {
                    let wkb = fast_map_prepare::geom::wkb(&r.geom)?;
                    let hex: String = wkb.iter().map(|b| format!("{b:02x}")).collect();
                    Ok(serde_json::json!({"mesh": names[r.mesh], "cell_id": r.cell_id, "wkb": hex}))
                })
                .collect::<fast_map_prepare::Result<_>>()?;
            println!(
                "{}",
                serde_json::json!({"rows": rows, "invalid_cells": cells.describe(None)?})
            );
            Ok(())
        }
        Some("version") if args.len() == 2 => {
            println!(
                "{}",
                serde_json::json!({
                    "name": "fast-map-prepare",
                    "version": env!("CARGO_PKG_VERSION"),
                    "commit": option_env!("FAST_MAP_BUILD_COMMIT").unwrap_or("unknown"),
                    "oracle": "ras-commander 0.124.0 (_depth_groups._cohort), GEOS 3.13.1",
                })
            );
            Ok(())
        }
        Some("worker") if args.len() == 3 => {
            fast_map_prepare::worker::run(&PathBuf::from(&args[2]))
        }
        _ => Err(fast_map_prepare::Error::invalid(
            "usage: fast-map-prepare prepare <request.json> <control-dir> | worker <config.json> | cells <plan.hdf> <policy> | version",
        )),
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            let record = serde_json::json!({"status": "refused", "kind": e.kind.as_str(), "message": e.message});
            eprintln!("{record}");
            ExitCode::from(2)
        }
    }
}
