//! Numeric visibility bounds for prepared cross-section surfaces.
//!
//! Caller staging converts geometry stations to native normalized-strip M and
//! selects the levee and end-obstruction records. This module does not load
//! geometry, infer obstructions, or assign pixel ownership. Apply these bounds
//! only after geometry has claimed a pixel, so a clipped owner never exposes a
//! different overlapping surface. Terrain visibility remains a separate step.
use crate::MapError;

/// One prepared lateral boundary and its elevation, in model output units.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Barrier {
    pub m: f32,
    pub elevation: f32,
}

/// Prepared per-XS levees and obstructions touching a cross-section end.
///
/// A left-end obstruction stores the M of its right boundary; a right-end
/// obstruction stores the M of its left boundary. Middle obstructions,
/// obstruction polygons and steady encroachments are outside this contract.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Clip {
    pub left_levee: Option<Barrier>,
    pub right_levee: Option<Barrier>,
    pub left_end: Option<Barrier>,
    pub right_end: Option<Barrier>,
}

/// Decode one little-endian transport row after its eight Float32 values have
/// been read: left levee M/elevation, right levee M/elevation, left-end M/elevation,
/// and right-end M/elevation. Two NaNs disable a pair; a partial pair is invalid.
pub fn decode(record: [f32; 8]) -> Result<Clip, MapError> {
    fn pair(m: f32, elevation: f32, name: &str) -> Result<Option<Barrier>, MapError> {
        if m.is_nan() && elevation.is_nan() {
            return Ok(None);
        }
        if !m.is_finite() || !(-2.0..=2.0).contains(&m) || !elevation.is_finite() {
            return Err(MapError::Invalid(format!(
                "1D {name} requires finite M in [-2,2] and elevation, or paired NaNs"
            )));
        }
        Ok(Some(Barrier { m, elevation }))
    }
    Ok(Clip {
        left_levee: pair(record[0], record[1], "left levee")?,
        right_levee: pair(record[2], record[3], "right levee")?,
        left_end: pair(record[4], record[5], "left end obstruction")?,
        right_end: pair(record[6], record[7], "right end obstruction")?,
    })
}

/// Validate a caller-constructed record before rendering touches output buffers.
pub fn validate(clip: Clip) -> Result<(), MapError> {
    for barrier in [
        clip.left_levee,
        clip.right_levee,
        clip.left_end,
        clip.right_end,
    ]
    .into_iter()
    .flatten()
    {
        if !barrier.m.is_finite()
            || !(-2.0..=2.0).contains(&barrier.m)
            || !barrier.elevation.is_finite()
        {
            return Err(MapError::Invalid(
                "1D clip requires finite M in [-2,2] and finite elevation".into(),
            ));
        }
    }
    Ok(())
}

fn bound(us: Barrier, ds: Barrier, weight: f32, complement: f32) -> f32 {
    // Preserve the native Float32 weighted sum; do not rewrite in difference
    // form or introduce fused multiply-add.
    us.m * weight + ds.m * complement
}

/// Whether a geometry-owned pixel survives prepared levees/end obstructions.
///
/// Levees and end obstructions require both endpoint records. Native default
/// strip limits are always active, even without either record. `weight` is the
/// unclamped native upstream weight; `m` is the pixel's normalized-strip M.
/// The caller validates finite profile/sample values and applies terrain nodata
/// and the Float32 plot tolerance separately. A false result only suppresses
/// this owner's output; it does not authorize reassignment to another surface.
#[allow(clippy::too_many_arguments)]
pub fn visible(
    upstream: Clip,
    downstream: Clip,
    us: f32,
    ds: f32,
    weight: f32,
    m: f32,
    wse: f32,
) -> bool {
    let complement = 1.0_f32 - weight;
    for (u, d, left) in [
        (upstream.left_levee, downstream.left_levee, true),
        (upstream.right_levee, downstream.right_levee, false),
    ] {
        if let (Some(u), Some(d)) = (u, d)
            && u.elevation > us
            && d.elevation > ds
        {
            let boundary = bound(u, d, weight, complement);
            if if left { m < boundary } else { m > boundary } {
                return false;
            }
        }
    }
    // The native default strip limits remain active without encroachment data.
    // Retain their Float32 endpoint interpolation, including unclamped weights.
    let left_strip = -2.0_f32 * weight + -2.0_f32 * complement;
    let right_strip = 2.0_f32 * weight + 2.0_f32 * complement;
    if m < left_strip || m > right_strip {
        return false;
    }
    for (u, d, left) in [
        (upstream.left_end, downstream.left_end, true),
        (upstream.right_end, downstream.right_end, false),
    ] {
        if let (Some(u), Some(d)) = (u, d) {
            let both_above = u.elevation > us && d.elevation > ds;
            let equal_and_above_water = u.elevation == d.elevation && wse < u.elevation;
            if both_above || equal_and_above_water {
                let boundary = bound(u, d, weight, complement);
                if if left { m < boundary } else { m > boundary } {
                    return false;
                }
            }
        }
    }
    true
}
