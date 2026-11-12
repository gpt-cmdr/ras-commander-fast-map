//! Opt-in final delivery for 1D and coupled composition products.
//!
//! When a request carries `final_format`, the engine applies the caller's AOI
//! pixel mask, counts finite and flooded Depth pixels and writes `depth.tif` and
//! `wse.tif` once with [`crate::final_tiff`] (tiled DEFLATE, numeric nodata,
//! optional full-grid placement and GDAL-average overviews). Product values are
//! unchanged; only NaN and masked pixels become the nodata value. Without
//! `final_format` the raw Float32 `.bin` transport is unchanged.
use crate::{MapError, final_tiff, native_output::FinalFormat};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Delivery fields shared by the 1D and coupled requests (flattened into them).
#[derive(Deserialize, Default, Clone)]
pub struct Options {
    #[serde(default)]
    pub final_format: Option<FinalFormat>,
    /// EPSG code written to the GeoTIFF keys; required with `final_format`.
    #[serde(default)]
    pub epsg: Option<u32>,
    /// One byte per window pixel, row-major: 1 keeps the pixel, 0 makes it nodata.
    #[serde(default)]
    pub aoi_mask_file: Option<PathBuf>,
    /// Depth range `(min, max]` counted as flooded; required with `final_format`.
    #[serde(default)]
    pub flooded_depth_range: Option<[f32; 2]>,
}

/// Validated delivery plan for one product window.
pub struct Plan {
    layout: final_tiff::Layout,
    epsg: u32,
    mask: Option<PathBuf>,
    flooded: [f32; 2],
    echo: Value,
}

impl Options {
    pub fn requested(&self) -> bool {
        self.final_format.is_some()
    }

    /// Validate before any payload read; `None` keeps the raw transport.
    pub fn plan(
        &self,
        window_width: usize,
        window_height: usize,
        window_transform: [f64; 6],
    ) -> Result<Option<Plan>, MapError> {
        let Some(format) = &self.final_format else {
            if self.epsg.is_some()
                || self.aoi_mask_file.is_some()
                || self.flooded_depth_range.is_some()
            {
                return Err(MapError::Invalid(
                    "epsg, aoi_mask_file and flooded_depth_range require final_format".into(),
                ));
            }
            return Ok(None);
        };
        let (Some(epsg), Some(flooded)) = (self.epsg, self.flooded_depth_range) else {
            return Err(MapError::Invalid(
                "final_format requires epsg and flooded_depth_range".into(),
            ));
        };
        if !flooded.iter().all(|v| v.is_finite()) || flooded[0] > flooded[1] {
            return Err(MapError::Invalid(
                "flooded_depth_range must be finite and ordered".into(),
            ));
        }
        final_tiff::check_epsg(epsg)?;
        let layout =
            final_tiff::Layout::new(format, window_width, window_height, window_transform)?;
        Ok(Some(Plan {
            layout,
            epsg,
            mask: self.aoi_mask_file.clone(),
            flooded,
            echo: serde_json::to_value(format).map_err(|e| MapError::Internal(e.to_string()))?,
        }))
    }
}

impl Plan {
    /// Writer scratch plus the mask buffer, for the caller's admission total.
    pub fn planned_bytes(&self, threads: usize) -> Result<usize, MapError> {
        let mask = if self.mask.is_some() {
            self.layout
                .window_width
                .checked_mul(self.layout.window_height)
        } else {
            Some(0)
        };
        self.layout
            .planned_bytes(threads)
            .and_then(|n| usize::try_from(n).ok())
            .zip(mask)
            .and_then(|(a, b)| a.checked_add(b))
            .ok_or_else(|| MapError::Budget("final_format plan overflow".into()))
    }

    /// Read and validate the mask before outputs exist.
    pub fn read_mask(&self, base: &Path) -> Result<Option<Vec<u8>>, MapError> {
        let Some(path) = &self.mask else {
            return Ok(None);
        };
        let pixels = self.layout.window_width * self.layout.window_height;
        let mask = crate::one_d_session::read_records(&base.join(path), pixels, |b: [u8; 1]| b[0])?;
        if mask.iter().any(|v| *v > 1) {
            return Err(MapError::Invalid(
                "aoi_mask_file bytes must be 0 or 1".into(),
            ));
        }
        Ok(Some(mask))
    }

    /// Mask, count and write both products into `folder`; return report fields.
    pub fn deliver(
        &self,
        folder: &Path,
        mask: Option<&[u8]>,
        depth: &mut [f32],
        wse: &mut [f32],
        pool: &rayon::ThreadPool,
    ) -> Result<Value, MapError> {
        if let Some(mask) = mask {
            for ((keep, d), w) in mask.iter().zip(depth.iter_mut()).zip(wse.iter_mut()) {
                if *keep == 0 {
                    *d = f32::NAN;
                    *w = f32::NAN;
                }
            }
        }
        let finite = depth.iter().filter(|d| d.is_finite()).count();
        let [low, high] = self.flooded;
        let flooded = depth.iter().filter(|d| **d > low && **d <= high).count();
        let summary = |s: final_tiff::Summary| {
            json!({"valid_pixels": s.valid_pixels, "tiles": s.tiles,
                "shared_nodata_tiles": s.shared_nodata_tiles, "file_bytes": s.file_bytes})
        };
        let depth_final = final_tiff::write(
            &folder.join("depth.tif"),
            depth,
            &self.layout,
            self.epsg,
            pool,
        )?;
        let wse_final =
            final_tiff::write(&folder.join("wse.tif"), wse, &self.layout, self.epsg, pool)?;
        Ok(json!({
            "compression": "DEFLATE", "final_format": self.echo, "epsg": self.epsg,
            "depth_file": "depth.tif", "wse_file": "wse.tif",
            "depth_final": summary(depth_final), "wse_final": summary(wse_final),
            "depth_finite_pixels": finite, "flooded_pixels": flooded,
            "flooded_depth_range": self.flooded, "aoi_mask_applied": mask.is_some(),
        }))
    }
}

/// Capability record advertised by methods that accept these options.
pub fn capability() -> Value {
    json!({"control":"final_format","compression":"DEFLATE","layout":"tiled",
        "block_size":{"default":256,"multiple_of":16,"min":16,"max":4096},
        "zlevel":{"default":6,"min":1,"max":9},"nodata":"finite Float32","full_grid":true,
        "overview_resampling":["average"],"overview_factors":"consecutive powers of two from 2",
        "overview_arithmetic":"gdal-average-cascade/1","shared_nodata_tile":true,
        "aoi_mask_file":"u8 per window pixel; 0 is nodata","flooded_count":"depth in (min, max]",
        "requires":["epsg","flooded_depth_range"]})
}
