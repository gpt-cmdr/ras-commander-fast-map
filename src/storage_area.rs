//! Standalone storage-area level-pool surfaces (`storage-area-level-pool/1`).
//!
//! RAS Mapper draws a storage area as a horizontal pool: the storage area's
//! water surface (Max or one output time) applied to every terrain pixel whose
//! centre lies inside the storage-area polygon. Qualified against native
//! HEC-RAS 7.0.1 stored maps on BaldEagleCrkMulti2D p07 and p04 in every render
//! mode and the whole-simulation products: every storage-area pixel, valid mask and float32 bits.
//!
//! The pixel rule is the native float32 plot test shared with the 2D cell
//! renderers: visible when `wse >= terrain + 0.001_f32`, depth `wse - terrain`.
//! Pixel ownership uses the same terrio pixel-centre rasterization and output
//! window as the 2D cells. Layouts whose native ordering is not qualified are
//! refused rather than guessed: a storage-area pixel that is also owned by a
//! 2D cell, or a pixel inside two storage areas.
use crate::{MapError, terrain::NativeTerrain};
use std::path::Path;
use terrio::ops::rasterize::Grid;

/// Method identifier of the storage-area surface. Change it when semantics change.
pub const METHOD_ID: &str = "storage-area-level-pool/1";

/// Window pixels owned by storage areas, in ascending window order.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct StorageAreaPixels {
    /// Row-major window pixel index.
    pub pixels: Vec<usize>,
    /// Zero-based storage-area position in the request's polygon file.
    pub labels: Vec<u32>,
    /// Terrain at the pixel; NaN where the terrain is nodata (always dry).
    pub ground: Vec<f32>,
    /// Number of storage areas in the request.
    pub count: usize,
}

pub fn method_record() -> serde_json::Value {
    serde_json::json!({
        "method_id": METHOD_ID,
        "surface": "level pool: one storage-area WSE (Max or output time) per polygon; no interpolation",
        "ownership": "terrio pixel-centre polygons on the 2D output window",
        "arithmetic": "native float32 WSE >= terrain + 0.001 plot tolerance; float32 subtraction",
        "refused": "storage-area pixels also owned by a 2D cell; pixels inside two storage areas; nonfinite storage-area WSE",
        "qualified": "native HEC-RAS 7.0.1 stored maps, BaldEagleCrkMulti2D p07 and p04: Max and one output time in horizontal, sloping and slopingPretty; whole-simulation arrival time and percent inundated",
        "not_qualified": "inundation boundaries; velocity; overlap ordering against 2D cells or other storage areas; multiple storage areas and storage-area-to-storage-area connections are untested"
    })
}

/// Burns the storage-area polygons on the output window and validates the layout.
/// `owner` is the 2D cell ownership of the same window, before or after AOI masking;
/// any overlap is refused. `aoi` (the window's `global-terrain-lattice/1` mask, when
/// present) removes storage-area pixels outside it, exactly as it masks 2D pixels.
pub fn prepare(
    polygons: &Path,
    count: usize,
    grid: &Grid,
    owner: &[u32],
    aoi: Option<&[u32]>,
    pool: &rayon::ThreadPool,
) -> Result<StorageAreaPixels, MapError> {
    if count == 0 || count >= u32::MAX as usize {
        return Err(MapError::Invalid(
            "storage_area_count must be between 1 and u32::MAX - 1".into(),
        ));
    }
    let window = grid.cols * grid.rows;
    if owner.len() != window || aoi.is_some_and(|m| m.len() != window) {
        return Err(MapError::Internal("ownership/window size mismatch".into()));
    }
    // terrio overwrites in feature order. Burning forwards and reversed gives
    // identical labels exactly when no pixel centre lies inside two polygons.
    let forward = crate::ownership::load(polygons, count, true)?;
    let mut reverse = crate::ownership::load(polygons, count, true)?;
    reverse.features.reverse();
    let labels = pool.install(|| crate::ownership::burn(forward, grid.clone()))?;
    let reversed = pool.install(|| crate::ownership::burn(reverse, grid.clone()))?;
    if labels != reversed {
        return Err(MapError::Unsupported(
            "storage-area polygons overlap; native ordering is not qualified".into(),
        ));
    }
    drop(reversed);
    select(&labels, owner, aoi, count)
}

fn select(
    labels: &[u32],
    owner: &[u32],
    aoi: Option<&[u32]>,
    count: usize,
) -> Result<StorageAreaPixels, MapError> {
    let mut out = StorageAreaPixels {
        count,
        ..Default::default()
    };
    for (i, &label) in labels.iter().enumerate() {
        if label == 0 {
            continue;
        }
        if label as usize > count {
            return Err(MapError::Internal("storage-area label out of range".into()));
        }
        if owner[i] != 0 {
            return Err(MapError::Unsupported(
                "storage-area polygon overlaps 2D cell ownership; native ordering is not qualified"
                    .into(),
            ));
        }
        if aoi.is_some_and(|m| m[i] == 0) {
            continue;
        }
        out.pixels.push(i);
        out.labels.push(label - 1);
    }
    out.ground = vec![f32::NAN; out.pixels.len()];
    Ok(out)
}

type BlockKey = (usize, usize);

/// Reads terrain for every storage-area pixel; nodata stays NaN (dry).
pub fn read_ground(
    pixels: &mut StorageAreaPixels,
    terrain: &NativeTerrain,
    window: [usize; 4],
) -> Result<(), MapError> {
    let [col, row, width, _] = window;
    let (bw, bh) = terrain.block;
    let mut order: Vec<usize> = (0..pixels.pixels.len()).collect();
    let block_of = |p: usize| {
        let (r, c) = (row + p / width, col + p % width);
        (r / bh, c / bw)
    };
    order.sort_by_key(|&k| (block_of(pixels.pixels[k]), pixels.pixels[k]));
    let mut current: Option<(BlockKey, std::sync::Arc<[f32]>)> = None;
    for k in order {
        let p = pixels.pixels[k];
        let key = block_of(p);
        if current.as_ref().is_none_or(|(b, _)| *b != key) {
            current = Some((key, terrain.get_block(key.1, key.0)?));
        }
        let (_, block) = current.as_ref().unwrap();
        let win = terrain.block_window(key.1, key.0);
        let (r, c) = (row + p / width, col + p % width);
        if let Some(value) = terrain.classify(block[(r - win.y) * win.width + c - win.x]) {
            if !value.is_finite() {
                return Err(MapError::Terrain("nonfinite storage-area terrain".into()));
            }
            pixels.ground[k] = value as f32;
        }
    }
    Ok(())
}

/// Reads one profile's storage-area WSE values (little-endian f32, one per area).
pub fn values(path: &Path, count: usize) -> Result<Vec<f32>, MapError> {
    let expected = count
        .checked_mul(4)
        .ok_or_else(|| MapError::Budget("storage-area values size overflow".into()))?;
    if std::fs::metadata(path)?.len() != expected as u64 {
        return Err(MapError::Invalid(
            "storage-area values length mismatch".into(),
        ));
    }
    let raw = std::fs::read(path)?;
    if raw.len() != expected {
        return Err(MapError::Invalid("storage-area values changed size".into()));
    }
    let values: Vec<f32> = raw
        .as_chunks::<4>()
        .0
        .iter()
        .map(|v| f32::from_le_bytes(*v))
        .collect();
    if values.iter().any(|v| !v.is_finite()) {
        return Err(MapError::Invalid(
            "storage-area WSE values must be finite".into(),
        ));
    }
    Ok(values)
}

/// Writes the level-pool surface into rendered 2D outputs (NaN = nodata).
/// Storage-area pixels are never owned by 2D cells, so nothing is overwritten.
/// `threshold` is a horizontal request's optional strict positive depth cutoff
/// (zero: none), applied exactly as for 2D pixels.
/// Returns the number of visible storage-area pixels.
pub fn apply(
    pixels: &StorageAreaPixels,
    values: &[f32],
    threshold: f32,
    depth: &mut [f32],
    wse: &mut [f32],
) -> Result<usize, MapError> {
    if values.len() != pixels.count {
        return Err(MapError::Invalid(
            "storage-area values count mismatch".into(),
        ));
    }
    let mut visible = 0;
    for ((&p, &label), &ground) in pixels.pixels.iter().zip(&pixels.labels).zip(&pixels.ground) {
        let value = values[label as usize];
        if !ground.is_finite() {
            continue;
        }
        // Same binary32 order as the 2D renderers: compare, then subtract.
        let delta = value - ground;
        if delta.is_infinite() {
            return Err(MapError::Invalid("float32 depth overflow".into()));
        }
        if value >= ground + 0.001_f32 && (threshold == 0.0 || delta > threshold) {
            if !depth[p].is_nan() || !wse[p].is_nan() {
                return Err(MapError::Internal(
                    "storage-area pixel already rendered by a 2D cell".into(),
                ));
            }
            depth[p] = delta;
            wse[p] = value;
            visible += 1;
        }
    }
    Ok(visible)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn polygon_wkb(rings: &[[[f64; 2]; 5]]) -> Vec<u8> {
        let mut out = Vec::new();
        for ring in rings {
            let mut wkb = vec![1];
            wkb.extend(3_u32.to_le_bytes());
            wkb.extend(1_u32.to_le_bytes());
            wkb.extend(5_u32.to_le_bytes());
            for p in ring {
                wkb.extend(p[0].to_le_bytes());
                wkb.extend(p[1].to_le_bytes());
            }
            out.extend((wkb.len() as u32).to_le_bytes());
            out.extend(wkb);
        }
        out
    }

    fn square(x0: f64, y0: f64, x1: f64, y1: f64) -> [[f64; 2]; 5] {
        [[x0, y0], [x1, y0], [x1, y1], [x0, y1], [x0, y0]]
    }

    fn grid() -> Grid {
        // 6 x 4 window, 1-unit pixels, origin (0, 4).
        Grid {
            geo_transform: [0., 1., 0., 4., 0., -1.],
            cols: 6,
            rows: 4,
            crs: Default::default(),
        }
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("fast-map-sa-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn pool() -> rayon::ThreadPool {
        rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap()
    }

    #[test]
    fn pixel_centre_ownership_and_aoi() {
        let d = scratch("own");
        let file = d.join("sa.wkb");
        // Two disjoint areas: columns 0-1 and 4-5 (pixel centres x = 0.5, 1.5 / 4.5, 5.5).
        fs::write(
            &file,
            polygon_wkb(&[square(0., 0., 2., 4.), square(4.2, 0., 6., 4.)]),
        )
        .unwrap();
        let owner = vec![0; 24];
        let sa = prepare(&file, 2, &grid(), &owner, None, &pool()).unwrap();
        assert_eq!(sa.pixels.len(), 16);
        assert!(sa.pixels.iter().all(|&p| matches!(p % 6, 0 | 1 | 4 | 5)));
        assert_eq!(sa.labels[0], 0);
        assert_eq!(*sa.labels.last().unwrap(), 1);
        // The AOI keeps only the top two rows.
        let aoi = d.join("aoi.wkb");
        fs::write(&aoi, polygon_wkb(&[square(0., 2., 6., 4.)])).unwrap();
        let mask = crate::aoi::mask(&aoi, grid().geo_transform, [0, 0, 6, 4]).unwrap();
        let clipped = prepare(&file, 2, &grid(), &owner, Some(&mask), &pool()).unwrap();
        assert_eq!(clipped.pixels.len(), 8);
        assert!(clipped.pixels.iter().all(|&p| p < 12));
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn overlaps_are_refused_not_ordered() {
        let d = scratch("overlap");
        let file = d.join("sa.wkb");
        fs::write(
            &file,
            polygon_wkb(&[square(0., 0., 3., 4.), square(2., 0., 6., 4.)]),
        )
        .unwrap();
        let err = prepare(&file, 2, &grid(), &[0; 24], None, &pool()).unwrap_err();
        assert_eq!(err.kind(), "unsupported");
        // Touching edges share no pixel centre and are accepted.
        fs::write(
            &file,
            polygon_wkb(&[square(0., 0., 3., 4.), square(3., 0., 6., 4.)]),
        )
        .unwrap();
        assert_eq!(
            prepare(&file, 2, &grid(), &[0; 24], None, &pool())
                .unwrap()
                .pixels
                .len(),
            24
        );
        // A storage-area pixel owned by a 2D cell is refused.
        let mut owner = vec![0; 24];
        owner[7] = 1;
        let err = prepare(&file, 2, &grid(), &owner, None, &pool()).unwrap_err();
        assert_eq!(err.kind(), "unsupported");
        // Count must match the file exactly.
        assert!(prepare(&file, 1, &grid(), &[0; 24], None, &pool()).is_err());
        assert!(prepare(&file, 0, &grid(), &[0; 24], None, &pool()).is_err());
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn level_pool_uses_native_binary32_plot_order() {
        let sa = StorageAreaPixels {
            pixels: vec![0, 1, 2, 3, 4],
            labels: vec![0, 0, 0, 1, 1],
            ground: vec![1000.0, 1000.0, f32::NAN, 10.0, 100000.0],
            count: 2,
        };
        let pool_a = f32::from_bits(0x447a0010); // 1000.0 + 2 ulp: >= 1000 + 0.001 in f32
        let mut depth = vec![f32::NAN; 6];
        let mut wse = depth.clone();
        let visible = apply(&sa, &[pool_a, 100000.0], 0.0, &mut depth, &mut wse).unwrap();
        assert_eq!(visible, 4);
        assert_eq!(depth[0].to_bits(), (pool_a - 1000.0).to_bits());
        assert!(depth[0] < 0.001); // no post-subtraction cutoff
        assert_eq!(wse[1], pool_a);
        assert!(depth[2].is_nan()); // nodata terrain stays dry
        assert_eq!(depth[3], 99990.0);
        assert_eq!(depth[4], 0.0); // terrain + tolerance rounds back to terrain
        assert!(depth[5].is_nan()); // not a storage-area pixel
        // Below the tolerance stays dry.
        let mut depth = vec![f32::NAN; 6];
        let mut wse = depth.clone();
        let below = f32::from_bits(0x447a000f);
        apply(&sa, &[below, 0.0], 0.0, &mut depth, &mut wse).unwrap();
        assert!(depth[0].is_nan() && depth[3].is_nan());
        // A horizontal strict depth cutoff applies to pools as to 2D pixels.
        let mut depth = vec![f32::NAN; 6];
        let mut wse = depth.clone();
        let visible = apply(&sa, &[pool_a, 100000.0], 1.0, &mut depth, &mut wse).unwrap();
        assert_eq!(visible, 1);
        assert!(depth[0].is_nan() && depth[4].is_nan());
        assert_eq!(depth[3], 99990.0);
        // Already-rendered pixels and wrong value counts are internal/invalid errors.
        let mut depth = vec![1.0; 6];
        assert!(apply(&sa, &[pool_a, 0.0], 0.0, &mut depth, &mut wse.clone()).is_err());
        assert!(apply(&sa, &[pool_a], 0.0, &mut [f32::NAN; 6], &mut [f32::NAN; 6]).is_err());
    }

    #[test]
    fn values_are_length_checked_and_finite() {
        let d = scratch("values");
        let file = d.join("v.bin");
        fs::write(
            &file,
            [1.0_f32.to_le_bytes(), 2.0_f32.to_le_bytes()].concat(),
        )
        .unwrap();
        assert_eq!(values(&file, 2).unwrap(), vec![1.0, 2.0]);
        assert!(values(&file, 3).is_err());
        fs::write(&file, f32::NAN.to_le_bytes()).unwrap();
        assert!(values(&file, 1).is_err());
        fs::remove_dir_all(d).unwrap();
    }
}
