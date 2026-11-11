use ras_commander_fast_map::{
    CONTRACT_VERSION, METHOD_VERSION, engine_method_record, method_record,
};
use serde_json::Value;
use std::{collections::BTreeSet, process::Command};

#[test]
fn rust_and_cli_discovery_are_identical() {
    let output = Command::new(env!("CARGO_BIN_EXE_ras2fim-batch"))
        .arg("method")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cli: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(cli, engine_method_record());
}

#[test]
fn discovery_preserves_existing_registry_keys_and_method_ids() {
    let record = engine_method_record();
    let expected_keys = [
        "engine",
        "coupled_diagnostic",
        "coupled_storage_diagnostic",
        "one_d_diagnostic",
        "one_d_native_diagnostic",
        "one_d_clipped_diagnostic",
        "batch_session_r1",
        "step05_r2",
        "horizontal",
        "sloping",
        "depth_groups",
        "boundary",
        "whole_simulation",
        "effective_terrain",
    ];
    assert!(
        expected_keys
            .into_iter()
            .collect::<BTreeSet<_>>()
            .is_subset(
                &record
                    .as_object()
                    .unwrap()
                    .keys()
                    .map(String::as_str)
                    .collect::<BTreeSet<_>>()
            )
    );
    for (key, method_id) in [
        (
            "coupled_diagnostic",
            "one-d-two-d-coverage-first-diagnostic/1",
        ),
        (
            "coupled_storage_diagnostic",
            "one-d-two-d-storage-coverage-first-diagnostic/1",
        ),
        ("one_d_diagnostic", "one-d-xs-surface-diagnostic/3"),
        ("one_d_native_diagnostic", "one-d-xs-surface-diagnostic/4"),
        ("one_d_clipped_diagnostic", "one-d-xs-surface-diagnostic/5"),
        ("horizontal", "horizontal-cell-constant/2"),
        ("sloping", "rasmapper-plain-sloping/1"),
        ("depth_groups", "masked-float32-depth-group-maximum/1"),
        ("whole_simulation", "native-whole-simulation-horizontal/1"),
        ("effective_terrain", "rasmapper-effective-terrain/2"),
    ] {
        assert_eq!(record[key]["method_id"], method_id, "{key}");
    }
}

#[test]
fn capabilities_preserve_dispatch_schemas_flags_and_qualification_limits() {
    let registry = engine_method_record();
    assert_eq!(
        registry["capabilities_schema"],
        "ras-commander-fast-map-capabilities/1"
    );
    let entries = registry["capabilities"].as_array().unwrap();
    let dispatch = [
        (
            "sloping_native_grid",
            "ras-commander-sloping-request/2",
            "sloping",
            "diagnostic",
            "unqualified",
        ),
        (
            "one_d_clipped_native_grid_diagnostic",
            "fast-map-one-d-diagnostic/4",
            "one-d-diagnostic",
            "diagnostic",
            "unqualified",
        ),
        (
            "coupled_native_grid_diagnostic",
            "fast-map-coupled-diagnostic/3",
            "coupled-diagnostic",
            "diagnostic",
            "unqualified",
        ),
        (
            "storage",
            "fast-map-storage/1",
            "storage",
            "mapping",
            "unqualified",
        ),
        (
            "coupled_diagnostic",
            "fast-map-coupled-diagnostic/1",
            "coupled-diagnostic",
            "diagnostic",
            "unqualified",
        ),
        (
            "coupled_storage_diagnostic",
            "fast-map-coupled-diagnostic/2",
            "coupled-diagnostic",
            "diagnostic",
            "unqualified",
        ),
        (
            "one_d_diagnostic",
            "fast-map-one-d-diagnostic/1",
            "one-d-diagnostic",
            "diagnostic",
            "unqualified",
        ),
        (
            "one_d_native_diagnostic",
            "fast-map-one-d-diagnostic/2",
            "one-d-diagnostic",
            "diagnostic",
            "unqualified",
        ),
        (
            "one_d_clipped_diagnostic",
            "fast-map-one-d-diagnostic/3",
            "one-d-diagnostic",
            "diagnostic",
            "unqualified",
        ),
        (
            "batch_session_r1",
            "ras2fim-batch-session/1",
            "run",
            "diagnostic",
            "not_declared",
        ),
        (
            "step05_r2",
            "ras2fim-step05-request/1",
            "step05",
            "mapping",
            "not_declared",
        ),
        (
            "horizontal",
            "ras2fim-horizontal-request/1",
            "horizontal",
            "mapping",
            "not_declared",
        ),
        (
            "sloping",
            "ras-commander-sloping-request/1",
            "sloping",
            "mapping",
            "scoped",
        ),
        (
            "depth_groups",
            "ras-commander-depth-groups-request/1",
            "depth-groups",
            "reduction",
            "scoped",
        ),
        (
            "boundary",
            "ras-commander-boundary-request/1",
            "boundary",
            "mapping",
            "not_declared",
        ),
        (
            "native_boundary",
            "ras-commander-boundary-request/2",
            "boundary",
            "mapping",
            "unqualified",
        ),
        (
            "whole_simulation",
            "ras-commander-whole-simulation-request/1",
            "whole-simulation",
            "mapping",
            "not_declared",
        ),
        (
            "effective_terrain",
            "ras-commander-effective-terrain-request/2",
            "effective-terrain",
            "terrain",
            "not_declared",
        ),
    ];
    assert_eq!(entries.len(), dispatch.len());
    let mut seen = BTreeSet::new();
    for (key, schema, command, intended_use, status) in dispatch {
        let entry = entries.iter().find(|entry| entry["key"] == key).unwrap();
        assert!(seen.insert(key));
        let schemas = if key == "effective_terrain" {
            serde_json::json!(["ras-commander-effective-terrain-request/1", schema])
        } else {
            serde_json::json!([schema])
        };
        assert_eq!(entry["request_schemas"], schemas, "{key}");
        assert_eq!(entry["cli_command"], command, "{key}");
        assert_eq!(entry["api_contract"], "stable", "{key}");
        assert_eq!(entry["intended_use"], intended_use, "{key}");
        assert_eq!(entry["numerical_qualification"]["status"], status, "{key}");
        assert_eq!(entry["flags"], registry[key], "{key}");
        assert_eq!(
            entry["method_id"],
            registry[key]
                .get("method_id")
                .or_else(|| registry[key].get("method_version"))
                .unwrap()
                .clone(),
            "{key}"
        );
        if status == "unqualified" {
            assert_eq!(
                entry["numerical_qualification"]["flags"]["native_parity_qualified"],
                false
            );
        }
    }
    let sloping = entries
        .iter()
        .find(|entry| entry["key"] == "sloping")
        .unwrap();
    assert_eq!(
        sloping["numerical_qualification"]["native_version"],
        "7.0.1"
    );
    assert_eq!(sloping["flags"]["experimental"], true);
    let boundary = entries
        .iter()
        .find(|entry| entry["key"] == "boundary")
        .unwrap();
    assert_eq!(
        boundary["numerical_qualification"]["flags"]["native_vector_parity"],
        false
    );
}

#[test]
fn legacy_batch_method_record_retains_its_scope() {
    let legacy = method_record();
    assert_eq!(legacy["method_version"], METHOD_VERSION);
    assert_eq!(
        legacy["output_contract"]["contract_version"],
        CONTRACT_VERSION
    );
    assert!(legacy["interpolation"].is_object());
    assert!(legacy.get("engine").is_none());
    assert!(legacy.get("sloping").is_none());
    assert_eq!(engine_method_record()["batch_session_r1"], legacy);
}
