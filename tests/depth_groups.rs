//! Generated fixtures: ordinary renderer parity, independent AOIs, spill, strict receipts.
use ras_commander_fast_map::{
    depth_groups::{self, Request},
    native_output, sloping_session,
};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, Read},
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
};
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn table(dir: &Path, name: &str, rows: usize, bytes: Vec<u8>) -> Value {
    fs::write(dir.join(format!("{name}.bin")), bytes).unwrap();
    json!({"path":format!("{name}.bin"),"rows":rows})
}
fn ints(dir: &Path, name: &str, rows: usize, v: &[i32]) -> Value {
    table(
        dir,
        name,
        rows,
        v.iter().flat_map(|v| v.to_le_bytes()).collect(),
    )
}
fn doubles(dir: &Path, name: &str, rows: usize, v: &[f64]) -> Value {
    table(
        dir,
        name,
        rows,
        v.iter().flat_map(|v| v.to_le_bytes()).collect(),
    )
}
fn floats(dir: &Path, name: &str, rows: usize, v: &[f32]) -> Value {
    table(
        dir,
        name,
        rows,
        v.iter().flat_map(|v| v.to_le_bytes()).collect(),
    )
}
fn polygon(dir: &Path, name: &str, x0: f64, x1: f64) {
    let mut wkb = vec![1];
    for i in [3u32, 1, 5] {
        wkb.extend(i.to_le_bytes());
    }
    for point in [[x0, 0.0], [x0, 4.0], [x1, 4.0], [x1, 0.0], [x0, 0.0]] {
        for v in point {
            wkb.extend(v.to_le_bytes());
        }
    }
    let mut bytes = (wkb.len() as u32).to_le_bytes().to_vec();
    bytes.extend(wkb);
    fs::write(dir.join(name), bytes).unwrap();
}
fn fixture() -> (Fixture, Value) {
    let dir = std::env::temp_dir().join(format!(
        "fast-map-groups-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&dir).unwrap();
    use ras_commander_fast_map::effective_terrain::{self, Effective, Grid};
    effective_terrain::write_geotiff(
        &dir.join("terrain.tif"),
        Effective {
            grid: Grid {
                origin_x: 0.0,
                origin_y: 4.0,
                cell_size: 1.0,
                width: 4,
                height: 4,
            },
            values: vec![0.0; 16],
            report: Value::Null,
        },
        3857,
    )
    .unwrap();
    let arrays = json!({
        "centers":doubles(&dir,"centers",5,&[2.,2.,-1.,2.,2.,5.,5.,2.,2.,-1.]),
        "cell_info":ints(&dir,"cell_info",5,&[0,4,4,1,5,1,6,1,7,1]),
        "cell_faces":ints(&dir,"cell_faces",8,&[0,1,1,1,2,1,3,1,0,-1,1,-1,2,-1,3,-1]),
        "cell_minimum":floats(&dir,"cell_minimum",5,&[0.;5]),
        "face_vertices":ints(&dir,"face_vertices",4,&[0,1,1,2,2,3,3,0]),
        "face_cells":ints(&dir,"face_cells",4,&[0,1,0,2,0,3,0,4]),
        "face_info":ints(&dir,"face_info",4,&[0;8]),
        "face_points":doubles(&dir,"face_points",0,&[]),
        "face_minimum":floats(&dir,"face_minimum",4,&[0.;4]),
        "vertices":doubles(&dir,"vertices",4,&[0.,0.,0.,4.,4.,4.,4.,0.]),
        "vertex_info":ints(&dir,"vertex_info",4,&[0,2,2,2,4,2,6,2]),
        "vertex_faces":ints(&dir,"vertex_faces",8,&[3,1,0,1,0,1,1,1,1,1,2,1,2,1,3,1])
    });
    fs::write(
        dir.join("mesh.json"),
        json!({"schema":"ras-commander-sloping-mesh/1","arrays":arrays}).to_string(),
    )
    .unwrap();
    polygon(&dir, "cell.wkb", 0., 4.);
    polygon(&dir, "left.wkb", 0., 2.);
    polygon(&dir, "right.wkb", 2., 4.);
    for (name, value) in [("a", 0.5), ("b", 1.25), ("dry", 0.0), ("high", 10001.0)] {
        floats(&dir, name, 5, &[value, 0., 0., 0., 0.]);
    }
    let model = json!({"schema":sloping_session::SCHEMA,"terrain":"terrain.tif","epsg":3857,
        "crs_identity":{"kind":"epsg"},"terrain_transform":[0.,1.,0.,4.,0.,-1.],"window":[0,0,4,4],
        "geometry_file":"cell.wkb","mesh_files":["mesh.json"],"owner_cells":[[0,0]],
        "profiles":[{"id":"a","values_files":["a.bin"]},{"id":"b","values_files":["b.bin"]},
            {"id":"dry","values_files":["dry.bin"]},{"id":"high","values_files":["high.bin"]}],
        "threads":2,"max_working_bytes":256u64<<20,"window_cache_bytes":0});
    (Fixture(dir), model)
}
fn grouped(model: Value) -> Value {
    json!({"schema":depth_groups::SCHEMA,"model":model,"source_indices":[7,3,9,11],"depth_units":"feet",
        "max_accumulator_bytes":1024,"compression":"NONE","jobs":[
            {"job_id":"left","reach_id":"r1","window":[0,0,4,4],"aoi_file":"left.wkb","groups":[
                {"group_id":"max","timesteps":[3,7]},{"group_id":"dry","timesteps":[9]},
                {"group_id":"equal","timesteps":[7]},{"group_id":"high","timesteps":[11]}]},
            {"job_id":"right","reach_id":"r2","window":[2,0,2,4],"aoi_file":"right.wkb","groups":[
                {"group_id":"max","timesteps":[7,3]}]}]})
}
fn read_tiff(path: &Path) -> Vec<f32> {
    let mut raster = terrio::LazyRaster::open_tiff(path.to_str().unwrap())
        .unwrap()
        .unwrap();
    let win = terrio::Window {
        x: 0,
        y: 0,
        width: raster.width(),
        height: raster.height(),
    };
    let ds = raster
        .read_window(
            win,
            &terrio::WindowOptions {
                bands: vec![1],
                overview: None,
            },
        )
        .unwrap();
    let terrio::raster::RasterData::F32(values) = ds.raster.data else {
        panic!("not Float32")
    };
    values
}
fn bits(a: &[f32], b: &[f32]) {
    assert_eq!(
        a.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
        b.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
    );
}
fn compression(path: &Path) -> u16 {
    let bytes = fs::read(path).unwrap();
    assert_eq!(&bytes[..4], b"II*\0");
    let offset = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
    let n = u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap()) as usize;
    for i in 0..n {
        let p = offset + 2 + i * 12;
        if u16::from_le_bytes(bytes[p..p + 2].try_into().unwrap()) == 259 {
            return u16::from_le_bytes(bytes[p + 8..p + 10].try_into().unwrap());
        }
    }
    panic!("no compression tag")
}

#[test]
fn grouped_matches_ordinary_frames_masks_bits_and_spill() {
    let (f, model) = fixture();
    let ordinary: sloping_session::Request = serde_json::from_value(model.clone()).unwrap();
    let baseline = sloping_session::run(&ordinary, &f.0, &f.0.join("ordinary")).unwrap();
    assert_eq!(baseline["status"], "complete");
    let wire = grouped(model);
    for (name, cap, compression_name) in [("resident", 1024, "NONE"), ("spill", 0, "LZW")] {
        let mut v = wire.clone();
        v["max_accumulator_bytes"] = cap.into();
        v["compression"] = compression_name.into();
        v["scratch"] = json!({"path":f.0,"node_local":true,"max_bytes":1024});
        let req: Request = serde_json::from_value(v).unwrap();
        let report = depth_groups::run(&req, &f.0, &f.0.join(name)).unwrap();
        assert_eq!(report["status"], "complete", "{report}");
        assert_eq!(report["geometry_preparations"], 1);
        assert_eq!(report["terrain_preparations"], 1);
        assert_eq!(report["hydraulic_preparations"], 4);
        assert_eq!(
            report["jobs"][0]["groups"][0]["frames"][0]["source_index"],
            3
        );
        assert_eq!(
            report["jobs"][0]["groups"][1]["frames"][0]["status"],
            "no_flooded_pixels"
        );
        assert_eq!(report["jobs"][0]["groups"][1]["all_dry"], true);
        let maximum = read_tiff(&f.0.join(name).join("left/max.tif"));
        let b = fs::read(f.0.join("ordinary/b/depth.bin")).unwrap();
        let b: Vec<_> = b
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect();
        let expected: Vec<_> = b
            .iter()
            .enumerate()
            .map(|(i, &v)| if i % 4 < 2 { v } else { f32::NAN })
            .collect();
        bits(&maximum, &expected);
        let equal = read_tiff(&f.0.join(name).join("left/equal.tif"));
        assert_eq!(equal.iter().filter(|&&v| v == 0.5).count(), 8);
        assert!(
            read_tiff(&f.0.join(name).join("left/dry.tif"))
                .iter()
                .all(|v| v.is_nan())
        );
        assert_eq!(read_tiff(&f.0.join(name).join("left/high.tif"))[0], 10001.0);
        bits(
            &read_tiff(&f.0.join(name).join("right/max.tif")),
            &[1.25; 8],
        );
        assert_eq!(
            compression(&f.0.join(name).join("left/max.tif")),
            if compression_name == "NONE" { 1 } else { 5 }
        );
        assert!(!f.0.join(name).join("a").exists());
        assert!(!f.0.join(name).join("left/unit.json").exists());
    }
    assert!(!fs::read_dir(&f.0).unwrap().any(|e| {
        e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("fast-map-groups-")
    }));
}

#[test]
fn malformed_groups_and_admission_fail_before_payloads() {
    let (f, model) = fixture();
    let base = grouped(model);
    for variant in ["repeat", "missing", "extra", "duplicate", "mode", "unit"] {
        let mut v = base.clone();
        match variant {
            "repeat" => v["jobs"][0]["groups"][0]["timesteps"] = json!([7, 7]),
            "missing" => v["jobs"][0]["groups"][0]["timesteps"] = json!([999]),
            "extra" => {
                v["source_indices"].as_array_mut().unwrap().push(json!(13));
                v["model"]["profiles"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"id":"extra","values_files":["absent"]}));
            }
            "duplicate" => v["jobs"][1]["job_id"] = "LEFT".into(),
            "mode" => v["model"]["options"] = json!({"render_mode":"slopingPretty"}),
            _ => v["depth_units"] = "unknown".into(),
        }
        if let Ok(req) = serde_json::from_value::<Request>(v) {
            assert!(depth_groups::run(&req, &f.0, &f.0.join(variant)).is_err());
            assert!(!f.0.join(variant).exists());
        }
    }
    let mut v = base;
    v["max_accumulator_bytes"] = 0.into();
    let req: Request = serde_json::from_value(v.clone()).unwrap();
    assert!(
        depth_groups::run(&req, &f.0, &f.0.join("no-scratch"))
            .unwrap_err()
            .to_string()
            .contains("required")
    );
    v["max_accumulator_bytes"] = 1024.into();
    v["model"]["max_working_bytes"] = 1024.into();
    let req: Request = serde_json::from_value(v).unwrap();
    assert!(
        depth_groups::run(&req, &f.0, &f.0.join("no-ram"))
            .unwrap_err()
            .to_string()
            .contains("allowed")
    );
}

#[test]
fn aborted_group_preserves_accounting_and_removes_products() {
    let (f, model) = fixture();
    let mut v = grouped(model);
    v["model"]["profiles"][1]["values_files"] = json!(["missing.bin"]);
    let req: Request = serde_json::from_value(v).unwrap();
    let report = depth_groups::run(&req, &f.0, &f.0.join("failed")).unwrap();
    assert_eq!(report["status"], "aborted");
    assert_eq!(
        report["jobs"][0]["groups"][0]["frames"][1]["status"],
        "complete"
    );
    assert_eq!(
        report["jobs"][0]["groups"][0]["frames"][0]["status"],
        "pending"
    );
    assert!(!f.0.join("failed/left/max.tif").exists());
}

#[test]
fn cli_output_controls_and_binary_group_framing() {
    let (f, mut model) = fixture();
    model["stream_outputs"] = true.into();
    model["geotiff_outputs"] = true.into();
    for (name, control) in [("legacy", false), ("selected", true)] {
        let mut v = model.clone();
        if control {
            v["products"] = json!(["depth"]);
            v["compression"] = "NONE".into();
        }
        fs::write(f.0.join("request.json"), v.to_string()).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_ras2fim-batch"))
            .args(["sloping"])
            .arg(f.0.join("request.json"))
            .arg(f.0.join(name))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(f.0.join(name).join("a/wse.tif").exists(), !control);
        assert_eq!(
            compression(&f.0.join(name).join("a/depth.tif")),
            if control { 1 } else { 5 }
        );
    }
    model["stream_outputs"] = false.into();
    model["geotiff_outputs"] = false.into();
    let mut v = grouped(model);
    v["sink"] = "grouped-buffer".into();
    fs::write(f.0.join("request.json"), v.to_string()).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_ras2fim-batch"))
        .arg("depth-groups")
        .arg(f.0.join("request.json"))
        .arg(f.0.join("buffers"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut stream = std::io::Cursor::new(output.stdout);
    for expected in [64, 64, 64, 64, 32] {
        let mut line = String::new();
        stream.read_line(&mut line).unwrap();
        let header: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(header["group"]["byte_length"], expected);
        let mut bytes = vec![0; expected as usize];
        stream.read_exact(&mut bytes).unwrap();
    }
    let mut terminal = String::new();
    stream.read_line(&mut terminal).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&terminal).unwrap()["command_status"],
        "complete"
    );
    assert_eq!(stream.position() as usize, stream.get_ref().len());
    let mut invalid = v;
    invalid["extra"] = true.into();
    assert!(serde_json::from_value::<Request>(invalid).is_err());
}

/// The final delivery option changes only nodata, placement, tiling, compression
/// and overviews: window pixels keep the legacy LZW product bits.
#[test]
fn cli_final_format_places_legacy_values_on_the_full_grid() {
    let (f, mut model) = fixture();
    model["stream_outputs"] = true.into();
    model["geotiff_outputs"] = true.into();
    model["window"] = json!([1, 1, 3, 2]);
    model["profiles"] =
        json!([{"id":"a","values_files":["a.bin"]},{"id":"b","values_files":["b.bin"]}]);
    let run = |name: &str, request: &Value| {
        fs::write(f.0.join("request.json"), request.to_string()).unwrap();
        Command::new(env!("CARGO_BIN_EXE_ras2fim-batch"))
            .args(["sloping"])
            .arg(f.0.join("request.json"))
            .arg(f.0.join(name))
            .output()
            .unwrap()
    };
    let legacy = run("legacy", &model);
    assert!(legacy.status.success());
    let mut selected = model.clone();
    selected["compression"] = "DEFLATE".into();
    selected["final_format"] = json!({"nodata":-9999.0,"block_size":16,
        "full_grid":{"width":4,"height":4,"row_offset":1,"col_offset":1,
            "transform":[0.,1.,0.,4.,0.,-1.]},
        "overview_factors":[2],"overview_resampling":"average"});
    let output = run("final", &selected);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let header: Value = serde_json::from_str(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .next()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(header["products"], "native-geotiff/1");
    assert_eq!(header["compression"], "DEFLATE");
    assert_eq!(header["final_format"]["full_grid"]["row_offset"], 1);
    let mut streamed = selected.clone();
    streamed["row_stream_rows"] = 1.into();
    assert!(run("rows", &streamed).status.success());
    for profile in ["a", "b"] {
        for product in ["depth", "wse"] {
            let name = format!("{profile}/{product}.tif");
            let window = read_tiff(&f.0.join("legacy").join(&name));
            let path = f.0.join("final").join(&name);
            let full = read_tiff(&path);
            assert_eq!((window.len(), full.len()), (6, 16));
            let mut valid = 0;
            for (i, v) in full.iter().enumerate() {
                let (r, c) = (i / 4, i % 4);
                let source = ((1..3).contains(&r) && (1..4).contains(&c))
                    .then(|| window[(r - 1) * 3 + c - 1])
                    .filter(|w| !w.is_nan());
                valid += usize::from(source.is_some());
                assert_eq!(
                    v.to_bits(),
                    source.unwrap_or(-9999.0).to_bits(),
                    "{name} {i}"
                );
            }
            assert_eq!(
                header[format!("{product}_final")]["valid_pixels"]
                    .as_u64()
                    .filter(|_| profile == "a")
                    .unwrap_or(valid as u64),
                valid as u64
            );
            let bytes = fs::read(&path).unwrap();
            assert_eq!(bytes, fs::read(f.0.join("rows").join(&name)).unwrap());
            assert_eq!(compression(&path), 8);
            let raster = terrio::LazyRaster::open_tiff(path.to_str().unwrap())
                .unwrap()
                .unwrap();
            assert_eq!(raster.overviews(), vec![(2, 2)]);
            assert_eq!(raster.block_size(), Some((16, 16)));
            assert_eq!(raster.nodata(), Some(-9999.0));
            assert_eq!(raster.geo_transform(), [0., 1., 0., 4., 0., -1.]);
        }
    }
    // Refused from the request alone: wrong placement, or DEFLATE without the format.
    let mut misplaced = selected.clone();
    misplaced["final_format"]["full_grid"]["col_offset"] = 0.into();
    assert!(!run("misplaced", &misplaced).status.success());
    assert!(!f.0.join("misplaced/a/depth.tif").exists());
    let mut bare = model.clone();
    bare["compression"] = "DEFLATE".into();
    assert!(!run("bare", &bare).status.success());
    // Final delivery is not qualified with the stored tile grid contract.
    let mut gridded = selected.clone();
    gridded["schema"] = "ras-commander-sloping-request/2".into();
    gridded["grid_contract"] = json!({"id":"rasmapper-stored-tile-grid/1","source_width":4,
        "source_height":4,"tile_size":256,"source_geo_transform":[0.,1.,0.,4.,0.,-1.],
        "output_geo_transform":[0.,1.,0.,4.,0.,-1.]});
    let refused = run("gridded", &gridded);
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("stored tile grid contract"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(!f.0.join("gridded/a/depth.tif").exists());
}

#[test]
fn native_extensions_are_strict_and_do_not_change_base_request() {
    let (_f, model) = fixture();
    let mut wire = model.clone();
    wire["products"] = json!(["depth"]);
    assert!(native_output::parse_request(wire.clone()).is_err());
    wire["stream_outputs"] = true.into();
    wire["geotiff_outputs"] = true.into();
    assert!(native_output::parse_request(wire.clone()).is_ok());
    wire["products"] = json!(["depth", "depth"]);
    assert!(native_output::parse_request(wire.clone()).is_err());
    wire["products"] = json!(["depth"]);
    wire["unknown"] = 1.into();
    assert!(native_output::parse_request(wire).is_err());
    assert!(serde_json::from_value::<sloping_session::Request>(model).is_ok());
    for wire in [
        r#"{"threads":1,"threads":2}"#,
        r#"{"profiles":[{"id":"a","id":"b"}]}"#,
    ] {
        assert!(native_output::read_wire(wire.as_bytes()).is_err());
    }
}

#[test]
fn metres_owned_buffers_and_consumer_failure_are_explicit() {
    use depth_groups::{OwnedBufferSink, run_with_session};
    let (f, model) = fixture();
    let mut v = grouped(model);
    v["depth_units"] = "metres".into();
    v["sink"] = "grouped-buffer".into();
    let req: Request = serde_json::from_value(v).unwrap();
    assert_eq!(
        req.validate().unwrap().to_bits(),
        ((0.5f64 * 0.3048) as f32).to_bits()
    );
    let mut seen = 0;
    let mut sink = OwnedBufferSink::new(|metadata: &Value, values: Vec<f32>| {
        assert_eq!(metadata["units"], "metres");
        assert_eq!(
            metadata["byte_length"].as_u64().unwrap() as usize,
            values.len() * 4
        );
        seen += 1;
        Ok(())
    });
    let mut session = sloping_session::PreparedSession::default();
    let first = run_with_session(
        &req,
        &f.0,
        &f.0.join("owned"),
        &mut session,
        Some(&mut sink),
    )
    .unwrap();
    assert_eq!(first["status"], "complete");
    assert_eq!(seen, 5);
    let mut failed = OwnedBufferSink::new(|_: &Value, _: Vec<f32>| {
        Err(ras_commander_fast_map::MapError::Io(
            "consumer rejected output".into(),
        ))
    });
    let report = run_with_session(
        &req,
        &f.0,
        &f.0.join("consumer-failed"),
        &mut session,
        Some(&mut failed),
    )
    .unwrap();
    assert_eq!(report["status"], "aborted");
    assert_eq!(report["geometry_preparations"], 0);
    assert_eq!(report["terrain_preparations"], 0);
    assert!(
        report["error"]
            .as_str()
            .unwrap()
            .contains("consumer rejected")
    );
    assert_eq!(
        report["jobs"][0]["groups"][0]["frames"][0]["status"],
        "complete"
    );
}

#[test]
fn persistent_server_dispatches_grouped_requests_and_retains_parent() {
    use std::io::Write;
    use std::process::Stdio;
    let (f, model) = fixture();
    let req = f.0.join("groups.json");
    fs::write(&req, grouped(model).to_string()).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_ras2fim-batch"))
        .arg("sloping-server")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    for name in ["one", "two"] {
        writeln!(input, "{}", json!({"request":req,"output":f.0.join(name)})).unwrap();
    }
    writeln!(input, "{}", json!({"close":true})).unwrap();
    drop(input);
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lines: Vec<_> = output
        .stdout
        .split(|&b| b == b'\n')
        .filter(|v| !v.is_empty())
        .collect();
    assert_eq!(lines.len(), 2);
    for line in lines {
        assert_eq!(
            serde_json::from_slice::<Value>(line).unwrap()["command_status"],
            "complete"
        );
    }
    let report: Value =
        serde_json::from_slice(&fs::read(f.0.join("two/report.json")).unwrap()).unwrap();
    assert_eq!(report["geometry_preparations"], 0);
    assert_eq!(report["terrain_preparations"], 0);
}
