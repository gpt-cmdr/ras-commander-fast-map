//! Bounded file transport for the experimental coverage-first compositor.
use crate::{
    MapError,
    coupled::{Products, Storage, compose_into, compose_with_storage_into},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub const SCHEMA: &str = "fast-map-coupled-diagnostic/1";
pub const STORAGE_SCHEMA: &str = "fast-map-coupled-diagnostic/2";
pub const STORED_GRID_SCHEMA: &str = "fast-map-coupled-diagnostic/3";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Component {
    #[serde(default)]
    pub grid_contract: Option<crate::stored_grid::Contract>,
    pub profile_id: String,
    /// Exact caller-verified CRS identity, shared by both prepared components.
    pub crs: String,
    pub width: usize,
    pub height: usize,
    pub geo_transform: [f64; 6],
    pub depth_file: PathBuf,
    pub wse_file: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub schema: String,
    #[serde(default)]
    pub grid_contract: Option<crate::stored_grid::Contract>,
    #[serde(default)]
    pub window: Option<[usize; 4]>,
    pub xs: Component,
    pub two_d: Component,
    /// Produced by the same 2D polygon burn used for these component products.
    pub two_d_owners_file: PathBuf,
    pub two_d_cell_count: usize,
    /// Schema /2 requires explicit storage products and physical ownership.
    #[serde(default)]
    pub storage: Option<StorageComponent>,
    pub max_working_bytes: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageComponent {
    pub component: Component,
    pub owners_file: PathBuf,
    pub area_count: usize,
}

pub fn run(req: &Request, base: &Path, output: &Path) -> Result<Value, MapError> {
    if !matches!(
        (req.schema.as_str(), req.storage.is_some()),
        (SCHEMA, false) | (STORAGE_SCHEMA, true) | (STORED_GRID_SCHEMA, false)
    ) {
        return Err(MapError::Unsupported(
            "coupled schema /1 excludes storage; schema /2 requires storage".into(),
        ));
    }
    let xs = &req.xs;
    let two = &req.two_d;
    let gt = xs.geo_transform;
    let layout=match (req.schema.as_str(),req.grid_contract.as_ref(),req.window) {
        (STORED_GRID_SCHEMA,Some(contract),Some(window))=>{
            let layout=contract.validate(window)?;
            if !xs.grid_contract.as_ref().is_some_and(|c|c.exactly_matches(contract)) || !two.grid_contract.as_ref().is_some_and(|c|c.exactly_matches(contract))
                || xs.width!=window[2] || xs.height!=window[3]
                || !crate::stored_grid::same(gt,layout.window_transform(window)) || !crate::stored_grid::same(two.geo_transform,gt) {
                return Err(MapError::Invalid("coupled stored grid component framing mismatch".into()));
            }
            Some(layout)
        },
        (SCHEMA|STORAGE_SCHEMA,None,None) if xs.grid_contract.is_none() && two.grid_contract.is_none()
            && req.storage.as_ref().is_none_or(|s|s.component.grid_contract.is_none())=>None,
        _=>return Err(MapError::Unsupported("coupled stored grid schema requires matching explicit component/source/window contracts; legacy excludes them".into())),
    };
    if xs.profile_id.is_empty()
        || xs.profile_id.len() > 256
        || xs.profile_id != two.profile_id
        || xs.crs.is_empty()
        || xs.crs.len() > 16384
        || xs.crs != two.crs
        || xs.width == 0
        || xs.height == 0
        || xs.width != two.width
        || xs.height != two.height
        || gt != two.geo_transform
        || !gt.iter().all(|v| v.is_finite())
        || gt[1] <= 0.
        || gt[5] >= 0.
        || gt[2] != 0.
        || gt[4] != 0.
        || (req.two_d_cell_count == 0 && req.schema != STORAGE_SCHEMA)
        || req.two_d_cell_count >= u32::MAX as usize
        || !(gt[0] + xs.width as f64 * gt[1]).is_finite()
        || !(gt[3] + xs.height as f64 * gt[5]).is_finite()
    {
        return Err(MapError::Invalid(
            "coupled grids/profiles must match; counts and north-up transform must be valid".into(),
        ));
    }
    if let Some(storage) = &req.storage {
        let s = &storage.component;
        if storage.area_count == 0
            || storage.area_count >= u32::MAX as usize
            || s.profile_id != xs.profile_id
            || s.crs != xs.crs
            || s.width != xs.width
            || s.height != xs.height
            || s.geo_transform != gt
        {
            return Err(MapError::Invalid(
                "storage grid/profile/CRS must match; area count must be valid".into(),
            ));
        }
    }
    let n = xs
        .width
        .checked_mul(xs.height)
        .ok_or_else(|| MapError::Budget("coupled grid overflow".into()))?;
    let planned = n
        .checked_mul(if req.storage.is_some() { 40 } else { 28 })
        // Conversion buffer + BufWriter + fixed report overhead. Variable
        // metadata allows for clones and worst-case JSON escaping/capacity.
        .and_then(|v| v.checked_add(65536 + 8192 + 65536))
        .and_then(|v| v.checked_add(32 * (xs.profile_id.len() + xs.crs.len())))
        .ok_or_else(|| MapError::Budget("coupled buffer size overflow".into()))?;
    if planned > req.max_working_bytes {
        return Err(MapError::Budget(format!(
            "coupled planned {planned} bytes exceeds budget"
        )));
    }
    if output.exists() {
        return Err(MapError::Invalid(
            "coupled output directory already exists".into(),
        ));
    }
    let read = |p: &Path| crate::one_d_session::read_records(&base.join(p), n, f32::from_le_bytes);
    let xd = read(&xs.depth_file)?;
    let xw = read(&xs.wse_file)?;
    let md = read(&two.depth_file)?;
    let mw = read(&two.wse_file)?;
    let owners = crate::one_d_session::read_records(
        &base.join(&req.two_d_owners_file),
        n,
        u32::from_le_bytes,
    )?;
    let storage = req
        .storage
        .as_ref()
        .map(|s| {
            Ok::<_, MapError>((
                crate::one_d_session::read_records(
                    &base.join(&s.owners_file),
                    n,
                    u32::from_le_bytes,
                )?,
                read(&s.component.depth_file)?,
                read(&s.component.wse_file)?,
                s.area_count,
            ))
        })
        .transpose()?;
    let mut depth = Vec::new();
    let mut wse = Vec::new();
    depth
        .try_reserve_exact(n)
        .map_err(|e| MapError::Budget(e.to_string()))?;
    wse.try_reserve_exact(n)
        .map_err(|e| MapError::Budget(e.to_string()))?;
    depth.resize(n, f32::NAN);
    wse.resize(n, f32::NAN);
    let xs_products = Products {
        depth: &xd,
        wse: &xw,
    };
    let mesh_products = Products {
        depth: &md,
        wse: &mw,
    };
    let counts = if let Some((labels, sd, sw, count)) = &storage {
        serde_json::to_value(compose_with_storage_into(
            &owners,
            req.two_d_cell_count,
            xs_products,
            mesh_products,
            Storage {
                owners: labels,
                area_count: *count,
                products: Products { depth: sd, wse: sw },
            },
            &mut depth,
            &mut wse,
        )?)
    } else {
        serde_json::to_value(compose_into(
            &owners,
            req.two_d_cell_count,
            xs_products,
            mesh_products,
            &mut depth,
            &mut wse,
        )?)
    }
    .map_err(|e| MapError::Internal(e.to_string()))?;
    let method = if layout.is_some() {
        stored_grid_method_record()
    } else if storage.is_some() {
        crate::coupled::storage_method_record()
    } else {
        crate::coupled::method_record()
    };
    fs::create_dir(output)?;
    let mut report = json!({"schema": req.schema, "method": method,
        "status": "complete", "profile_id": xs.profile_id, "crs": xs.crs, "width": xs.width, "height": xs.height,
        "geo_transform": gt, "counts": counts, "planned_bytes": planned,
        "depth_file": "depth.bin", "wse_file": "wse.bin", "dtype": "little-endian float32", "nodata": "NaN"});
    if let Some(layout) = layout {
        report["grid_contract"] = json!(req.grid_contract);
        report["window"] = json!(req.window);
        report["source_geo_transform"] = json!(layout.source);
        report["output_geo_transform"] = json!(layout.output);
    }
    let result = (|| {
        crate::horizontal::write_values(&output.join("depth.bin"), &depth)?;
        crate::horizontal::write_values(&output.join("wse.bin"), &wse)?;
        Ok::<_, MapError>(())
    })();
    if let Err(e) = &result {
        report["status"] = json!("aborted");
        report["error"] = json!(e.to_string());
    }
    fs::write(
        output.join("report.json"),
        serde_json::to_vec_pretty(&report).map_err(|e| MapError::Internal(e.to_string()))?,
    )?;
    result?;
    Ok(report)
}

pub fn stored_grid_method_record() -> Value {
    let mut record = crate::coupled::method_record();
    record["method_id"] = json!("one-d-two-d-native-grid-coverage-first-diagnostic/1");
    record["grid_contracts"] = json!([crate::stored_grid::ID]);
    record
}
