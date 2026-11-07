//! Mapping on the top-priority source of a multi-source terrain, without a
//! RAS Mapper terrain export (`top-priority-source-footprint/1`).
//!
//! RAS Mapper writes stored maps per terrain source, on that source's own grid.
//! Inside the highest-priority source's footprint (bit 0 of its level-0 tile
//! `Mask` in the terrain HDF), the effective terrain is that source's original
//! pixels: on BaldEagleCrkMulti2D p07 (two sources), the RAS Mapper 1x export
//! equals the priority-1 TIFF at every footprint pixel, and mapping directly on
//! that TIFF matches native HEC-RAS 7.0.1 stored maps in every valid pixel and
//! float32 bit (Max and one output time) without HEC-RAS.
//!
//! What is not reproduced, and therefore refused rather than approximated:
//! pixels outside the footprint. RAS Mapper fills them from lower-priority
//! sources (its export resamples them with a rule close to, but not,
//! pixel-centre bilinear) and stitches; the footprint also drops valid source
//! pixels (23,329 on p07, including 229 single-pixel slivers that the export
//! fills from the coarser source).
//!
//! Foreign pixels are therefore allowed only where the result cannot depend on
//! their terrain: in sloping rendering, pixels of cells that fail the native
//! cell wet gate are never drawn. Any foreign pixel of a wet cell and any
//! foreign pixel in horizontal rendering (no cell minimum is supplied) is
//! refused.
use crate::{MapError, model, terrain::NativeTerrain};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

pub const METHOD_ID: &str = "top-priority-source-footprint/1";
const TILE: usize = 256;

/// Level-0 tile mask of the source (terrain HDF `Terrain/<source>/0/Mask`).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaskArray {
    pub path: String,
    pub dtype: String,
    /// `[tiles_y * tiles_x, 65536]`, tiles row-major over the source raster.
    pub shape: [usize; 2],
}

/// Declares how the supplied `terrain` relates to the registered terrain.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerrainScope {
    /// `top_priority_source`: `terrain` is the original TIFF of the
    /// highest-priority source of a registered terrain with `source_count` sources.
    pub kind: String,
    pub source_count: usize,
    pub mask: MaskArray,
}

impl TerrainScope {
    pub fn validate(&self) -> Result<(), MapError> {
        if self.kind != "top_priority_source"
            || self.source_count == 0
            || self.mask.dtype != "|u1"
            || self.mask.shape[1] != TILE * TILE
            || self.mask.shape[0] == 0
        {
            return Err(MapError::Invalid(
                "terrain_scope needs kind top_priority_source, source_count >= 1 and a |u1 [tiles, 65536] mask".into(),
            ));
        }
        Ok(())
    }
}

pub fn method_record() -> Value {
    json!({
        "method_id": METHOD_ID,
        "terrain": "original float32 TIFF of the highest-priority registered source, 256 x 256 tiles; no export, resampling or mosaic",
        "footprint": "bit 0 of the source's level-0 terrain-HDF tile mask, with valid source data",
        "foreign_pixels": "owned pixels outside the footprint; allowed only in sloping cells that fail the native wet gate",
        "refused": "foreign pixels of wet cells, foreign pixels in horizontal rendering, foreign storage-area pixels, cell-face sloping",
        "qualified": "BaldEagleCrkMulti2D p07 (2 sources), native HEC-RAS 7.0.1 plain sloping Max and one output time"
    })
}

/// Footprint of the window: mask bit 0 and valid source data, per window pixel.
fn footprint(
    scope: &TerrainScope,
    base: &Path,
    terrain: &NativeTerrain,
    window: [usize; 4],
) -> Result<Vec<bool>, MapError> {
    let [col, row, width, height] = window;
    let (tiles_x, tiles_y) = (terrain.width.div_ceil(TILE), terrain.height.div_ceil(TILE));
    if terrain.block != (TILE, TILE) || scope.mask.shape[0] != tiles_x * tiles_y {
        return Err(MapError::Unsupported(
            "terrain_scope needs the original 256 x 256 tiled source and its full level-0 mask"
                .into(),
        ));
    }
    let path = model::resolve(base, &scope.mask.path);
    let expected = (scope.mask.shape[0] * TILE * TILE) as u64;
    if std::fs::metadata(&path)?.len() != expected {
        return Err(MapError::Invalid(
            "terrain_scope mask length mismatch".into(),
        ));
    }
    let mut file = File::open(&path)?;
    let mut inside = vec![false; width * height];
    let mut tile = vec![0_u8; TILE * TILE];
    for ty in row / TILE..=(row + height - 1) / TILE {
        for tx in col / TILE..=(col + width - 1) / TILE {
            file.seek(SeekFrom::Start(((ty * tiles_x + tx) * TILE * TILE) as u64))?;
            file.read_exact(&mut tile)?;
            let block = terrain.get_block(tx, ty)?;
            let win = terrain.block_window(tx, ty);
            for r in row.max(win.y)..(row + height).min(win.y + win.height) {
                for c in col.max(win.x)..(col + width).min(win.x + win.width) {
                    let (lr, lc) = (r - win.y, c - win.x);
                    let valid = terrain
                        .classify(block[lr * win.width + lc])
                        .is_some_and(f64::is_finite);
                    inside[(r - row) * width + c - col] = valid && tile[lr * TILE + lc] & 1 != 0;
                }
            }
        }
    }
    Ok(inside)
}

/// Result of the terrain-scope guard on one window.
pub struct Scoped {
    pub record: Value,
    /// 2D-owned window pixels outside the footprint (window order). They may
    /// be mapped only while their cell fails the wet gate; empty for one source.
    pub foreign: Vec<usize>,
}

/// Classifies owned pixels against the top-priority source's footprint.
///
/// `storage` lists storage-area (level-pool) window pixels. A level pool has no
/// cell minimum that could bound a pixel's visibility without its terrain, so
/// with more than one source any storage-area pixel outside the footprint is
/// refused.
pub fn check(
    scope: &TerrainScope,
    base: &Path,
    terrain: &NativeTerrain,
    window: [usize; 4],
    owner: &[u32],
    storage: &[usize],
) -> Result<Scoped, MapError> {
    scope.validate()?;
    let [_, _, width, height] = window;
    if owner.len() != width * height || storage.iter().any(|&p| p >= width * height) {
        return Err(MapError::Internal("ownership/window size mismatch".into()));
    }
    let inside = footprint(scope, base, terrain, window)?;
    let (owned, foreign) = classify(&inside, owner);
    let foreign_storage = foreign_storage(&inside, storage);
    let mut record = json!({"method": method_record(), "source_count": scope.source_count,
        "owned_pixels": owned, "foreign_2d_pixels": foreign.len()});
    if !storage.is_empty() {
        record["storage_area_pixels"] = storage.len().into();
        record["foreign_storage_area_pixels"] = foreign_storage.into();
    }
    if scope.source_count == 1 {
        return Ok(Scoped {
            record,
            foreign: Vec::new(),
        });
    }
    if foreign_storage > 0 {
        return Err(MapError::Unsupported(format!(
            "{foreign_storage} storage-area pixels lie outside the top-priority source's footprint; lower-priority sources and stitches are not qualified"
        )));
    }
    Ok(Scoped { record, foreign })
}

/// Uses already classified source validity. The callback must describe the
/// original source (not rendered depth or the delivery nodata sentinel).
/// Only owned/storage pixels affect the guard; no terrain block is decoded.
pub(crate) fn check_with_validity(
    scope: &TerrainScope,
    base: &Path,
    terrain: &NativeTerrain,
    window: [usize; 4],
    owner: &[u32],
    storage: &[usize],
    valid: impl Fn(usize) -> bool,
) -> Result<Scoped, MapError> {
    scope.validate()?;
    let [col, row, width, height] = window;
    if owner.len() != width * height || storage.iter().any(|&p| p >= owner.len()) {
        return Err(MapError::Internal("ownership/window size mismatch".into()));
    }
    let tiles_x = terrain.width.div_ceil(TILE);
    if terrain.block != (TILE, TILE)
        || scope.mask.shape[0] != tiles_x * terrain.height.div_ceil(TILE)
    {
        return Err(MapError::Unsupported(
            "terrain_scope needs the original 256 x 256 tiled source and its full level-0 mask"
                .into(),
        ));
    }
    let path = model::resolve(base, &scope.mask.path);
    if std::fs::metadata(&path)?.len() != (scope.mask.shape[0] * TILE * TILE) as u64 {
        return Err(MapError::Invalid(
            "terrain_scope mask length mismatch".into(),
        ));
    }
    let mut file = File::open(path)?;
    let mut tile = vec![0_u8; TILE * TILE];
    let mut current = None;
    let mut inside = |p: usize| -> Result<bool, MapError> {
        let (r, c) = (row + p / width, col + p % width);
        let key = (r / TILE) * tiles_x + c / TILE;
        if current != Some(key) {
            file.seek(SeekFrom::Start((key * TILE * TILE) as u64))?;
            file.read_exact(&mut tile)?;
            current = Some(key);
        }
        Ok(valid(p) && tile[(r % TILE) * TILE + c % TILE] & 1 != 0)
    };
    let mut owned = 0_u64;
    let mut foreign_count = 0_u64;
    let mut foreign = Vec::new();
    // Tile order limits mask reads; sort the sparse foreign indices afterwards
    // to retain the legacy window-order denial/report behavior.
    for ty in row / TILE..=(row + height - 1) / TILE {
        for tx in col / TILE..=(col + width - 1) / TILE {
            for r in row.max(ty * TILE)..(row + height).min((ty + 1) * TILE) {
                for c in col.max(tx * TILE)..(col + width).min((tx + 1) * TILE) {
                    let p = (r - row) * width + c - col;
                    if owner[p] != 0 {
                        owned += 1;
                        if !inside(p)? {
                            foreign_count += 1;
                            if scope.source_count > 1 {
                                foreign.push(p);
                            }
                        }
                    }
                }
            }
        }
    }
    foreign.sort_unstable();
    let mut foreign_storage = 0;
    for &p in storage {
        foreign_storage += usize::from(!inside(p)?);
    }
    let mut record = json!({"method":method_record(),"source_count":scope.source_count,
        "owned_pixels":owned,"foreign_2d_pixels":foreign_count});
    if !storage.is_empty() {
        record["storage_area_pixels"] = storage.len().into();
        record["foreign_storage_area_pixels"] = foreign_storage.into();
    }
    if scope.source_count > 1 && foreign_storage > 0 {
        return Err(MapError::Unsupported(format!(
            "{foreign_storage} storage-area pixels lie outside the top-priority source's footprint; lower-priority sources and stitches are not qualified"
        )));
    }
    Ok(Scoped { record, foreign })
}

/// Fails when a 2D cell that owns a foreign pixel is wet: the pixel's terrain
/// would come from a lower-priority source or stitch, which is unqualified.
pub fn deny_wet_foreign(
    foreign: &[usize],
    owner: &[u32],
    owner_cells: &[[usize; 2]],
    wet: impl Fn(usize, usize) -> bool,
) -> Result<(), MapError> {
    let wet_pixels = foreign
        .iter()
        .filter(|&&p| {
            let [mesh, cell] = owner_cells[owner[p] as usize - 1];
            wet(mesh, cell)
        })
        .count();
    if wet_pixels > 0 {
        return Err(MapError::Unsupported(format!(
            "{wet_pixels} pixels of wet cells lie outside the top-priority source's footprint; lower-priority sources and stitches are not qualified"
        )));
    }
    Ok(())
}

/// Storage-area pixels outside the footprint.
fn foreign_storage(inside: &[bool], storage: &[usize]) -> usize {
    storage.iter().filter(|&&p| !inside[p]).count()
}

/// Owned pixel count and 2D-owned pixels outside the footprint (window order).
fn classify(inside: &[bool], owner: &[u32]) -> (u64, Vec<usize>) {
    let mut owned = 0_u64;
    let mut foreign = Vec::new();
    for (p, _) in owner.iter().enumerate().filter(|(_, label)| **label != 0) {
        owned += 1;
        if !inside[p] {
            foreign.push(p);
        }
    }
    (owned, foreign)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(kind: &str, count: usize, dtype: &str, tiles: usize) -> TerrainScope {
        TerrainScope {
            kind: kind.into(),
            source_count: count,
            mask: MaskArray {
                path: "mask.u1".into(),
                dtype: dtype.into(),
                shape: [tiles, 65536],
            },
        }
    }

    #[test]
    fn cached_validity_preserves_scope_counts_and_refusals_without_decoding() {
        use crate::effective_terrain::{self, Effective, Grid};
        use crate::model::{CrsIdentity, NodataPolicy, QueryConvention};
        let dir = std::env::temp_dir().join(format!("scope-validity-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("source.tif");
        effective_terrain::write_geotiff(
            &path,
            Effective {
                grid: Grid {
                    origin_x: 0.0,
                    origin_y: 256.0,
                    cell_size: 1.0,
                    width: 256,
                    height: 256,
                },
                values: vec![-9999.0; 256 * 256],
                report: Value::Null,
            },
            3857,
        )
        .unwrap();
        let terrain = NativeTerrain::open(
            path.to_str().unwrap(),
            3857,
            &CrsIdentity::Epsg,
            QueryConvention::PixelCenter,
            NodataPolicy::RawValue,
            1 << 20,
        )
        .unwrap();
        let mut mask = vec![1; 65536];
        mask[1] = 0;
        std::fs::write(dir.join("mask.u1"), &mask).unwrap();
        let owner = [1, 2, 0, 0];
        let validity = [true, true, true, false]; // valid -9999 is NOT inferred from rendered sentinel
        let scope = scope("top_priority_source", 1, "|u1", 1);
        let result =
            check_with_validity(&scope, &dir, &terrain, [0, 0, 4, 1], &owner, &[2, 3], |p| {
                validity[p]
            })
            .unwrap();
        let reference = check(&scope, &dir, &terrain, [0, 0, 4, 1], &owner, &[2]).unwrap();
        assert_eq!(
            result.record["owned_pixels"],
            reference.record["owned_pixels"]
        );
        assert_eq!(
            result.record["foreign_2d_pixels"],
            reference.record["foreign_2d_pixels"]
        );
        assert_eq!(result.record["foreign_storage_area_pixels"], 1);
        assert!(result.foreign.is_empty());
        let scope = TerrainScope {
            source_count: 2,
            ..scope
        };
        let result = check_with_validity(&scope, &dir, &terrain, [0, 0, 4, 1], &owner, &[], |p| {
            validity[p]
        })
        .unwrap();
        assert_eq!(result.foreign, vec![1]);
        assert!(
            check_with_validity(&scope, &dir, &terrain, [0, 0, 4, 1], &owner, &[3], |p| {
                validity[p]
            })
            .is_err()
        );
        // Compare full legacy classifier for partial windows touching the source edge.
        let before = serde_json::to_value(terrain.stats()).unwrap();
        let cached = check_with_validity(
            &scope,
            &dir,
            &terrain,
            [254, 255, 2, 1],
            &[1, 1],
            &[],
            |_| true,
        )
        .unwrap();
        let after = serde_json::to_value(terrain.stats()).unwrap();
        assert_eq!(before["cache"]["tile_reads"], after["cache"]["tile_reads"]);
        let reference = check(&scope, &dir, &terrain, [254, 255, 2, 1], &[1, 1], &[]).unwrap();
        assert_eq!(cached.record, reference.record);
        assert_eq!(cached.foreign, reference.foreign);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn footprint_classification_of_2d_pixels() {
        let inside = [true, false, true, false, true, true];
        let owner = [1, 1, 0, 2, 0, 0];
        assert_eq!(classify(&inside, &owner), (3, vec![1, 3]));
        assert_eq!(classify(&inside, &[0; 6]), (0, vec![]));
        // Storage-area pixels are classified on the same footprint.
        assert_eq!(foreign_storage(&inside, &[2, 3, 5]), 1);
        assert_eq!(foreign_storage(&inside, &[]), 0);
    }

    #[test]
    fn foreign_pixels_are_allowed_only_in_dry_cells() {
        let owner = [0, 1, 2, 2];
        let cells = [[0, 7], [1, 3]];
        // Pixel 1 belongs to mesh 0 cell 7; pixels 2-3 to mesh 1 cell 3.
        assert!(deny_wet_foreign(&[1, 3], &owner, &cells, |_, _| false).is_ok());
        let err = deny_wet_foreign(&[1, 3], &owner, &cells, |m, c| (m, c) == (1, 3)).unwrap_err();
        assert_eq!(err.kind(), "unsupported");
        assert!(err.to_string().starts_with("unsupported: 1 pixels"));
        assert!(deny_wet_foreign(&[], &owner, &cells, |_, _| true).is_ok());
    }

    #[test]
    fn scope_fields_are_validated() {
        assert!(scope("top_priority_source", 2, "|u1", 4).validate().is_ok());
        for bad in [
            scope("mosaic", 2, "|u1", 4),
            scope("top_priority_source", 0, "|u1", 4),
            scope("top_priority_source", 2, "<f4", 4),
            scope("top_priority_source", 2, "|u1", 0),
        ] {
            assert_eq!(bad.validate().unwrap_err().kind(), "invalid");
        }
    }
}
