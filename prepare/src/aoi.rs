//! Area of interest: `fim/_aoi.py` (`_polygon_from_frame`, `validate_polygon`)
//! and `_aoi_lattice.index_range` / `_aoi.center_index_range`.
//!
//! The AOI is given as the WKB of the polygon layer's features (the reach
//! polygon fim-commander writes to `reach.gpkg`) and their EPSG code. The
//! Python path reprojects an AOI in another CRS; this path refuses instead.

use crate::error::{Error, Result};
use crate::geom;
use geos::{Geom, Geometry, GeometryTypes};

pub struct Aoi {
    pub geometry: Geometry,
    pub epsg: u32,
    pub source: String,
}

/// `validate_polygon`: a valid, non-empty Polygon or MultiPolygon with finite
/// coordinates.
fn validate(g: &Geometry, source: &str) -> Result<()> {
    let t = g.geometry_type()?;
    if !matches!(t, GeometryTypes::Polygon | GeometryTypes::MultiPolygon) {
        return Err(Error::aoi(format!(
            "{source}: polygon AOI must be a Polygon or MultiPolygon, got {t:?}"
        )));
    }
    if g.is_empty()? {
        return Err(Error::aoi(format!("{source}: polygon geometry is empty")));
    }
    let finite = geom::polygon_rings(g)?
        .iter()
        .flatten()
        .flatten()
        .all(|p| p[0].is_finite() && p[1].is_finite());
    if !finite {
        return Err(Error::aoi(format!(
            "{source}: polygon has non-finite coordinates"
        )));
    }
    if !g.is_valid()? {
        return Err(Error::aoi(format!(
            "{source}: polygon geometry is invalid ({})",
            g.is_valid_reason()?
        )));
    }
    Ok(())
}

impl Aoi {
    /// `_polygon_from_frame`: validate each feature, union them
    /// (`shapely.union_all`) and validate the union.
    pub fn from_features(features: &[Vec<u8>], epsg: u32, source: &str) -> Result<Self> {
        if features.is_empty() {
            return Err(Error::aoi(format!("{source}: no features")));
        }
        let mut geoms = Vec::new();
        for (i, f) in features.iter().enumerate() {
            let g = geom::from_wkb(f)?;
            validate(&g, &format!("{source} feature {i}"))?;
            geoms.push(g);
        }
        let union = geom::union_all(geoms)?;
        validate(&union, &format!("{source} (union of features)"))?;
        Ok(Self {
            geometry: union,
            epsg,
            source: source.to_string(),
        })
    }

    /// The AOI geometry in the grid CRS (`geometry_in_crs`); equal CRS only.
    pub fn in_grid(&self, grid_epsg: u32) -> Result<&Geometry> {
        if self.epsg != grid_epsg {
            return Err(Error::unsupported(format!(
                "AOI EPSG:{} differs from the terrain EPSG:{grid_epsg}; reprojection is not supported",
                self.epsg
            )));
        }
        Ok(&self.geometry)
    }
}

/// `_aoi_lattice.index_range`: indices `[start, stop)` on the global terrain lattice.
pub fn index_range(lo: f64, hi: f64, origin: f64, step: f64, count: i64) -> (i64, i64) {
    let lower = (lo - origin) / step.abs();
    let upper = (hi - origin) / step.abs();
    let (start, stop) = if step > 0.0 {
        ((lower - 0.5).ceil() as i64, (upper - 0.5).ceil() as i64)
    } else {
        (
            ((-upper - 0.5).floor() as i64).saturating_add(1),
            ((-lower - 0.5).floor() as i64).saturating_add(1),
        )
    };
    (start.min(count).max(0), stop.min(count).max(0))
}

/// `_aoi.center_index_range`: indices whose centre `origin + (i + 0.5) * step`
/// lies in `[lo, hi)`.
pub fn center_index_range(lo: f64, hi: f64, origin: f64, step: f64, count: i64) -> (i64, i64) {
    let inside = |i: i64| {
        let c = origin + (i as f64 + 0.5) * step;
        lo <= c && c < hi
    };
    let a = (lo - origin) / step - 0.5;
    let b = (hi - origin) / step - 0.5;
    let i_lo = (a.min(b).floor() as i64)
        .saturating_sub(1)
        .min(count)
        .max(0);
    let i_hi = (a.max(b).ceil() as i64).saturating_add(2).min(count).max(0);
    let Some(start) = (i_lo..i_lo.saturating_add(4).min(i_hi)).find(|&i| inside(i)) else {
        return (0, 0);
    };
    let last = (start..i_hi).rev().find(|&i| inside(i)).unwrap_or(start);
    (start, last + 1)
}
