//! Diagnostic composition of globally prepared, aligned 1D and 2D products.
//!
//! Ownership precedes visibility: a dry 2D cell suppresses an underlying wet
//! XS surface. This module does not infer interfaces, stage HDF, synchronize
//! hydraulic runs, or qualify arbitrary mixed models.
use crate::MapError;

pub const METHOD_ID: &str = "one-d-two-d-coverage-first-diagnostic/1";
pub const STORAGE_METHOD_ID: &str = "one-d-two-d-storage-coverage-first-diagnostic/1";

pub fn storage_method_record() -> serde_json::Value {
    serde_json::json!({"native_geotiff_outputs": true, "native_final_format": crate::one_d_delivery::capability(),
        "method_id": STORAGE_METHOD_ID, "experimental": true,
        "native_parity_qualified": false,
        "ownership": "caller-supplied global physical 2D and storage labels; 0 unowned, row index + 1 owned",
        "precedence": "disjoint 2D and storage coverage override XS before wet/dry filtering",
        "arithmetic": "copy selected Depth/WSE Float32 bits; no interpolation, maximum or wet-only fallback",
        "scope": "aligned single-profile XS, 2D and storage products; zero 2D count requires entirely unowned dry mesh; overlapping 2D/storage coverage rejected; no structure overrides",
        "preparation": "caller preserves global topology, storage polygon source ordering, hydraulic dependencies, terrain layout and CRS"})
}

pub fn method_record() -> serde_json::Value {
    serde_json::json!({"native_geotiff_outputs": true, "native_final_format": crate::one_d_delivery::capability(),
        "method_id": METHOD_ID, "experimental": true,
        "native_parity_qualified": false,
        "ownership": "caller-supplied global physical 2D cell labels; 0 unowned, row index + 1 owned",
        "precedence": "2D coverage wins before wet/dry filtering; otherwise use XS product",
        "arithmetic": "copy selected Depth/WSE Float32 bits; no interpolation, maximum or wet-only fallback",
        "scope": "aligned single-profile 1D XS and 2D components only; no storage or structure overrides",
        "preparation": "caller preserves global topology, hydraulic dependencies, source terrain layout and CRS; no per-crop retriangulation"})
}

#[derive(Clone, Copy)]
pub struct Products<'a> {
    pub depth: &'a [f32],
    pub wse: &'a [f32],
}

#[derive(Debug, serde::Serialize)]
pub struct Counts {
    pub valid_pixels: usize,
    pub two_d_owned_pixels: usize,
    pub dry_two_d_suppressed_wet_xs: usize,
}

fn valid_pair(depth: f32, wse: f32) -> bool {
    (depth.is_nan() && wse.is_nan()) || (depth.is_finite() && depth >= 0. && wse.is_finite())
}

#[derive(Clone, Copy)]
pub struct Storage<'a> {
    pub owners: &'a [u32],
    pub area_count: usize,
    pub products: Products<'a>,
}

#[derive(Debug, serde::Serialize)]
pub struct StorageCounts {
    #[serde(flatten)]
    pub base: Counts,
    pub storage_owned_pixels: usize,
    pub dry_storage_suppressed_wet_xs: usize,
}

/// Explicit storage extension. Physical storage coverage suppresses XS even
/// when its product is dry. Overlap with physical 2D coverage is unqualified
/// and rejected. Validate all inputs before changing either output buffer.
pub fn compose_with_storage_into(
    owners: &[u32],
    cell_count: usize,
    xs: Products<'_>,
    mesh: Products<'_>,
    storage: Storage<'_>,
    depth: &mut [f32],
    wse: &mut [f32],
) -> Result<StorageCounts, MapError> {
    let n = owners.len();
    if storage.area_count == 0
        || storage.area_count >= u32::MAX as usize
        || [
            storage.owners.len(),
            storage.products.depth.len(),
            storage.products.wse.len(),
        ]
        .iter()
        .any(|&len| len != n)
    {
        return Err(MapError::Invalid(
            "coupled storage lengths or area count are invalid".into(),
        ));
    }
    for (i, &owner) in storage.owners.iter().enumerate() {
        if owner as usize > storage.area_count
            || !valid_pair(storage.products.depth[i], storage.products.wse[i])
            || (owner == 0 && storage.products.depth[i].is_finite())
            || (owner != 0 && owners[i] != 0)
        {
            return Err(MapError::Invalid(format!(
                "coupled storage pixel {i}: invalid products, ownership or overlapping 2D coverage"
            )));
        }
    }
    let base = compose_into_impl(owners, cell_count, xs, mesh, depth, wse, true)?;
    let mut counts = StorageCounts {
        base,
        storage_owned_pixels: 0,
        dry_storage_suppressed_wet_xs: 0,
    };
    for (i, &owner) in storage.owners.iter().enumerate() {
        if owner != 0 {
            counts.storage_owned_pixels += 1;
            counts.dry_storage_suppressed_wet_xs +=
                usize::from(storage.products.depth[i].is_nan() && xs.depth[i].is_finite());
            counts.base.valid_pixels -= usize::from(depth[i].is_finite());
            depth[i] = storage.products.depth[i];
            wse[i] = storage.products.wse[i];
            counts.base.valid_pixels += usize::from(depth[i].is_finite());
        }
    }
    Ok(counts)
}

/// Validate every input before changing output buffers. Caller supplies aligned
/// grids and the same hydraulic profile, and owns all memory/worker budgets.
pub fn compose_into(
    owners: &[u32],
    cell_count: usize,
    xs: Products<'_>,
    mesh: Products<'_>,
    depth: &mut [f32],
    wse: &mut [f32],
) -> Result<Counts, MapError> {
    compose_into_impl(owners, cell_count, xs, mesh, depth, wse, false)
}

#[allow(clippy::too_many_arguments)]
fn compose_into_impl(
    owners: &[u32],
    cell_count: usize,
    xs: Products<'_>,
    mesh: Products<'_>,
    depth: &mut [f32],
    wse: &mut [f32],
    allow_zero_cells: bool,
) -> Result<Counts, MapError> {
    let n = owners.len();
    if n == 0
        || (cell_count == 0 && !allow_zero_cells)
        || cell_count >= u32::MAX as usize
        || [
            xs.depth.len(),
            xs.wse.len(),
            mesh.depth.len(),
            mesh.wse.len(),
            depth.len(),
            wse.len(),
        ]
        .iter()
        .any(|len| *len != n)
    {
        return Err(MapError::Invalid(
            "coupled buffer lengths or cell count are invalid".into(),
        ));
    }
    for (i, &owner) in owners.iter().enumerate() {
        if owner as usize > cell_count
            || !valid_pair(xs.depth[i], xs.wse[i])
            || !valid_pair(mesh.depth[i], mesh.wse[i])
            || (owner == 0 && mesh.depth[i].is_finite())
        {
            return Err(MapError::Invalid(format!(
                "coupled pixel {i}: invalid ownership, product mask or value"
            )));
        }
    }
    let mut counts = Counts {
        valid_pixels: 0,
        two_d_owned_pixels: 0,
        dry_two_d_suppressed_wet_xs: 0,
    };
    for (i, (&owner, (out_depth, out_wse))) in owners
        .iter()
        .zip(depth.iter_mut().zip(wse.iter_mut()))
        .enumerate()
    {
        let selected = if owner != 0 {
            counts.two_d_owned_pixels += 1;
            counts.dry_two_d_suppressed_wet_xs +=
                usize::from(mesh.depth[i].is_nan() && xs.depth[i].is_finite());
            mesh
        } else {
            xs
        };
        *out_depth = selected.depth[i];
        *out_wse = selected.wse[i];
        counts.valid_pixels += usize::from(out_depth.is_finite());
    }
    Ok(counts)
}
