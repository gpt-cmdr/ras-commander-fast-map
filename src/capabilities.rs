//! Shared method discovery for Rust callers and the command-line executable.

/// Describe the engine and all methods advertised by `ras2fim-batch method`.
///
/// Method records retain their individual numerical contracts and qualification
/// limits. An advertised method is not a promise of parity for arbitrary inputs.
/// The legacy [`crate::method_record`] describes only the original batch method.
pub fn engine_method_record() -> serde_json::Value {
    let mut registry = serde_json::json!({
        "engine": {
            "package": "ras-commander-fast-map",
            "version": crate::ENGINE_VERSION,
            "source_commit": crate::ENGINE_COMMIT
        },
        "storage": crate::storage_session::method_record(),
        "coupled_diagnostic": crate::coupled::method_record(),
        "coupled_native_grid_diagnostic":crate::coupled_session::stored_grid_method_record(),
        "coupled_storage_diagnostic": crate::coupled::storage_method_record(),
        "one_d_diagnostic": crate::one_d::method_record(),
        "one_d_native_diagnostic": crate::one_d_native::method_record(),
        "one_d_clipped_diagnostic": crate::one_d_native::clipped_method_record(),
        "one_d_clipped_native_grid_diagnostic":stored_xs_record(),
        "batch_session_r1": crate::method_record(),
        "step05_r2": crate::step05::method_record(),
        "horizontal": crate::horizontal::method_record(),
        "sloping": crate::sloping_session::method_record(),
        "sloping_native_grid":crate::sloping_session::native_grid_method_record(),
        "depth_groups": crate::depth_groups::method_record(),
        "boundary": crate::boundary_session::method_record(),
        "native_boundary": crate::boundary_session::native_method_record(),
        "whole_simulation": crate::whole_simulation::method_record(),
        "effective_terrain": crate::effective_terrain::method_record()
    });
    let specifications = [
        (
            "sloping_native_grid",
            crate::sloping_session::NATIVE_GRID_SCHEMA,
            "sloping",
            "diagnostic",
        ),
        (
            "one_d_clipped_native_grid_diagnostic",
            crate::one_d_session::STORED_GRID_SCHEMA,
            "one-d-diagnostic",
            "diagnostic",
        ),
        (
            "coupled_native_grid_diagnostic",
            crate::coupled_session::STORED_GRID_SCHEMA,
            "coupled-diagnostic",
            "diagnostic",
        ),
        (
            "storage",
            crate::storage_session::SCHEMA,
            "storage",
            "mapping",
        ),
        (
            "coupled_diagnostic",
            crate::coupled_session::SCHEMA,
            "coupled-diagnostic",
            "diagnostic",
        ),
        (
            "coupled_storage_diagnostic",
            crate::coupled_session::STORAGE_SCHEMA,
            "coupled-diagnostic",
            "diagnostic",
        ),
        (
            "one_d_diagnostic",
            crate::one_d_session::SCHEMA,
            "one-d-diagnostic",
            "diagnostic",
        ),
        (
            "one_d_native_diagnostic",
            crate::one_d_session::NATIVE_SCHEMA,
            "one-d-diagnostic",
            "diagnostic",
        ),
        (
            "one_d_clipped_diagnostic",
            crate::one_d_session::CLIPPED_SCHEMA,
            "one-d-diagnostic",
            "diagnostic",
        ),
        (
            "batch_session_r1",
            crate::model::SESSION_SCHEMA,
            "run",
            "diagnostic",
        ),
        (
            "step05_r2",
            crate::step05::REQUEST_SCHEMA,
            "step05",
            "mapping",
        ),
        (
            "horizontal",
            crate::horizontal::SCHEMA,
            "horizontal",
            "mapping",
        ),
        (
            "sloping",
            crate::sloping_session::SCHEMA,
            "sloping",
            "mapping",
        ),
        (
            "depth_groups",
            crate::depth_groups::SCHEMA,
            "depth-groups",
            "reduction",
        ),
        (
            "boundary",
            crate::boundary_session::SCHEMA,
            "boundary",
            "mapping",
        ),
        (
            "native_boundary",
            crate::boundary_session::NATIVE_SCHEMA,
            "boundary",
            "mapping",
        ),
        (
            "whole_simulation",
            crate::whole_simulation::SCHEMA,
            "whole-simulation",
            "mapping",
        ),
        (
            "effective_terrain",
            crate::effective_terrain::SCHEMA,
            "effective-terrain",
            "terrain",
        ),
    ];
    let capabilities: Vec<_> = specifications
        .into_iter()
        .map(|(key, schema, command, intended_use)| {
            capability(&registry[key], key, schema, command, intended_use)
        })
        .collect();
    registry["capabilities_schema"] = "ras-commander-fast-map-capabilities/1".into();
    registry["capabilities"] = capabilities.into();
    registry
}

fn stored_xs_record() -> serde_json::Value {
    let mut record = crate::one_d_native::clipped_method_record();
    record["grid_contracts"] = serde_json::json!([crate::stored_grid::ID]);
    // Inline storage composition is not offered with the stored grid contract.
    if let Some(map) = record.as_object_mut() {
        map.remove("inline_storage");
    }
    record
}

fn capability(
    record: &serde_json::Value,
    key: &str,
    schema: &str,
    command: &str,
    intended_use: &str,
) -> serde_json::Value {
    let status = if record["native_parity_qualified"] == false {
        "unqualified"
    } else if record.get("native_version_qualified").is_some()
        || record.get("qualification_scope").is_some()
    {
        "scoped"
    } else {
        "not_declared"
    };
    let qualification_flags: serde_json::Map<String, serde_json::Value> = [
        "experimental",
        "native_parity_qualified",
        "native_vector_parity",
        "native_version_qualified",
        "qualification_scope",
        "qualified",
        "hydraulic_qualification",
        "known_differences",
    ]
    .into_iter()
    .filter_map(|field| record.get(field).map(|value| (field.into(), value.clone())))
    .collect();
    let mut entry = serde_json::json!({
        "key": key,
        "method_id": record.get("method_id").or_else(|| record.get("method_version")),
        "request_schemas": [schema],
        "cli_command": command,
        "api_contract": "stable",
        "intended_use": intended_use,
        "flags": record,
        "numerical_qualification": {
            "status": status,
            "native_version": record.get("native_version_qualified"),
            "scope": record.get("qualification_scope").or_else(|| record.get("scope")),
            "flags": qualification_flags
        }
    });
    if key == "effective_terrain" {
        entry["request_schemas"] = serde_json::json!([
            crate::effective_terrain::SCHEMA_V1,
            crate::effective_terrain::SCHEMA
        ]);
    }
    if let Some(version) = record.get("method_version") {
        entry["method_version"] = version.clone();
    }
    entry
}
