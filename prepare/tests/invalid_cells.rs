//! `_invalid_cells.load_cells` + `require_cells` parity on ras-commander 0.124.0's
//! synthetic meshes (bowtie, split, consumed, cascade, hole, tie) under every policy.
//! Expectations come from ras-commander itself (`tests/fixtures/make_fixtures.py`).

use fast_map_prepare::{cells, geom, hdf::Hdf};
use serde_json::{Value, json};
use std::path::PathBuf;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

#[test]
fn invalid_cell_policies_match_ras_commander() {
    let expected: Value = serde_json::from_str(
        &std::fs::read_to_string(fixtures().join("expected-cells.json")).unwrap(),
    )
    .unwrap();
    let mut checked = 0;
    for (key, want) in expected.as_object().unwrap() {
        let (case, policy) = key.split_once('/').unwrap();
        let hdf = Hdf::open(&fixtures().join(format!("cells-{case}.hdf")), 1).unwrap();
        let names = hdf.mesh_names().unwrap();
        let got = match cells::load(&hdf, &names, policy, 1) {
            Ok(c) => {
                let rows: Vec<Value> = c
                    .rows
                    .iter()
                    .map(|r| {
                        let hex: String = geom::wkb(&r.geom)
                            .unwrap()
                            .iter()
                            .map(|b| format!("{b:02x}"))
                            .collect();
                        json!({"mesh": names[r.mesh], "cell_id": r.cell_id, "wkb": hex})
                    })
                    .collect();
                json!({"rows": rows, "invalid_cells": c.describe(None).unwrap()})
            }
            Err(e) => json!({"refused": e.message}),
        };
        if want.get("refused").is_some() {
            assert!(
                got.get("refused").is_some(),
                "{key}: Python refuses, Rust gave {got}"
            );
        } else {
            assert_eq!(&got, want, "{key}");
        }
        checked += 1;
    }
    assert_eq!(checked, 24);
}
