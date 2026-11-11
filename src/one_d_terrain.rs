//! Window terrain for the 1D request, read directly with terrio.
//!
//! Replaces caller-staged Float32 buffers: the source GeoTIFF must be the
//! request's single-band Float32 grid (same size and geotransform). Tile-row
//! stripes are decoded on the request pool, one reader per worker, one source
//! tile per terrio call (a single-chunk read never fans out to terrio's own
//! parallelism, so the request's thread budget holds). Values equal
//! to the declared nodata under GDAL's Float32 rule (`ARE_REAL_EQUAL`, as a
//! masked GDAL read) become NaN; every other value keeps its bits.
use crate::MapError;
use rayon::prelude::*;
use std::path::Path;
use terrio::{LazyRaster, Window, WindowOptions, raster::RasterData};

fn open(path: &Path) -> Result<LazyRaster, MapError> {
    let text = path
        .to_str()
        .ok_or_else(|| MapError::Invalid("terrain_tiff path is not UTF-8".into()))?;
    let r = LazyRaster::open_tiff(text)
        .map_err(|e| MapError::Terrain(format!("{text}: {e}")))?
        .ok_or_else(|| MapError::Unsupported(format!("{text}: not a TIFF")))?;
    if !r.is_lazy() {
        return Err(MapError::Unsupported(format!(
            "{text}: terrio did not open a lazy GeoTIFF"
        )));
    }
    Ok(r)
}

/// Validated source: tile shape and the f32 nodata used for masking.
pub struct Source {
    stripe_rows: usize,
    tile_cols: usize,
    nodata: Option<f32>,
}

/// Check the header against the request grid before any payload read.
pub fn inspect(path: &Path, width: usize, height: usize, gt: [f64; 6]) -> Result<Source, MapError> {
    let r = open(path)?;
    if r.band_count() != 1 || r.data_type() != terrio::DataType::F32 {
        return Err(MapError::Unsupported(
            "terrain_tiff must be single-band Float32".into(),
        ));
    }
    if (r.width(), r.height()) != (width, height) || r.geo_transform() != gt {
        return Err(MapError::Invalid(
            "terrain_tiff size or geotransform differs from the request grid".into(),
        ));
    }
    let block = r
        .block_size()
        .filter(|b| b.0 > 0 && b.1 > 0 && b.1 <= 4096)
        .ok_or_else(|| MapError::Unsupported("terrain_tiff has no usable block layout".into()))?;
    // A one-pixel read on the calling thread starts terrio's process-wide
    // thread pool before any parallel read; first use from several request
    // workers at once would otherwise race to start (and discard) pools.
    let mut r = r;
    r.read_window(
        Window {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        },
        &WindowOptions {
            bands: vec![1],
            overview: None,
        },
    )
    .map_err(|e| MapError::Terrain(format!("terrain_tiff first read: {e}")))?;
    Ok(Source {
        stripe_rows: block.1,
        tile_cols: block.0,
        nodata: r.nodata().filter(|v| !v.is_nan()).map(|v| v as f32),
    })
}

impl Source {
    /// Decode scratch per worker: one stripe of the window plus tile decode slack.
    pub fn planned_bytes(&self, window_width: usize, threads: usize) -> Option<usize> {
        window_width
            .checked_mul(self.stripe_rows)?
            .checked_mul(8)?
            .checked_add(4 << 20)?
            .checked_mul(threads.max(1))
    }

    /// Read the window into `out` (row-major, `width * height`).
    pub fn read(
        &self,
        path: &Path,
        window: [usize; 4],
        out: &mut [f32],
        pool: Option<&rayon::ThreadPool>,
    ) -> Result<(), MapError> {
        let [col, row, width, height] = window;
        if out.len() != width * height {
            return Err(MapError::Invalid("terrain window buffer mismatch".into()));
        }
        // Stripes follow source tile rows so each decode touches whole tiles once.
        let first = row / self.stripe_rows;
        let last = (row + height - 1) / self.stripe_rows;
        let mut stripes = Vec::with_capacity(last - first + 1);
        let mut rest = out;
        for s in first..=last {
            let top = (s * self.stripe_rows).max(row);
            let bottom = ((s + 1) * self.stripe_rows).min(row + height);
            let (head, tail) = std::mem::take(&mut rest).split_at_mut((bottom - top) * width);
            rest = tail;
            stripes.push((top, bottom - top, head));
        }
        let nodata = self.nodata;
        let decode = |reader: &mut Option<LazyRaster>,
                      (top, rows, chunk): (usize, usize, &mut [f32])|
         -> Result<(), MapError> {
            if reader.is_none() {
                *reader = Some(open(path)?);
            }
            // One source tile per terrio call: a single-chunk read decodes on the
            // calling thread, so all parallelism stays on the request pool.
            let reader = reader.as_mut().unwrap();
            let mut x = col;
            while x < col + width {
                let next = ((x / self.tile_cols + 1) * self.tile_cols).min(col + width);
                let tile_width = next - x;
                let ds = reader
                    .read_window(
                        Window {
                            x,
                            y: top,
                            width: tile_width,
                            height: rows,
                        },
                        &WindowOptions {
                            bands: vec![1],
                            overview: None,
                        },
                    )
                    .map_err(|e| {
                        MapError::Terrain(format!("terrain_tiff tile at {x},{top}: {e}"))
                    })?;
                let RasterData::F32(values) = ds.raster.data else {
                    return Err(MapError::Unsupported(
                        "terrio returned non-Float32 terrain".into(),
                    ));
                };
                if values.len() != tile_width * rows {
                    return Err(MapError::Terrain("terrain tile size mismatch".into()));
                }
                for (r, source) in values.chunks_exact(tile_width).enumerate() {
                    let start = r * width + (x - col);
                    for (dst, &v) in chunk[start..start + tile_width].iter_mut().zip(source) {
                        *dst = match nodata {
                            Some(nd) if crate::final_tiff::masked(v, nd) => f32::NAN,
                            _ => v,
                        };
                    }
                }
                x = next;
            }
            Ok(())
        };
        match pool {
            Some(pool) => pool.install(|| {
                stripes
                    .into_par_iter()
                    .try_for_each_init(|| None, |reader, stripe| decode(reader, stripe))
            }),
            None => {
                let mut reader = None;
                stripes
                    .into_iter()
                    .try_for_each(|stripe| decode(&mut reader, stripe))
            }
        }
    }
}
