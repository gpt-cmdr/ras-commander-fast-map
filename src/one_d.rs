//! Experimental mapping over a *stored* cross-section interpolation surface.
//!
//! This is a diagnostic contract, not qualified RASMapper arithmetic. Commander
//! supplies the global TIN and cross-section result ordering; no triangulation,
//! HDF access, junction synthesis, or 1D/2D composition happens here.
use crate::MapError;
use rstar::{AABB, RTree, RTreeObject};

pub const METHOD_ID: &str = "one-d-xs-surface-diagnostic/3";

pub fn method_record() -> serde_json::Value {
    serde_json::json!({
        "method_id": METHOD_ID, "experimental": true, "native_parity_qualified": false,
        "topology": "caller-supplied stored XS TIN; segment-local triangle indices",
        "weight": "TIN point column 2 is upstream weight; column 3 is validated but unused for Depth/WSE",
        "arithmetic": "f64 barycentric weight, clamped to [0,1], cast to f32; weight * upstream + (1 - weight) * downstream, each operation f32; no fused multiply-add",
        "edge_rule": "inclusive barycentric coordinates >= 0; last supplied segment wins, then lowest triangle index within that segment",
        "visibility": "finite WSE >= f32(terrain + 0.001); depth = f32(WSE - terrain)",
        "scope": "prepared cross-section surfaces only; no structure overrides, junction synthesis, storage areas or coupled 1D/2D ownership",
        "profile_missing": "either endpoint missing invalidates the entire segment"
    })
}

/// One row of stored TIN Info plus its XSIDs, in original source order.
#[derive(Clone, Copy, Debug)]
pub struct Segment {
    pub point_start: usize,
    pub point_count: usize,
    pub triangle_start: usize,
    pub triangle_count: usize,
    pub upstream: usize,
    pub downstream: usize,
}

#[derive(Clone)]
struct Triangle {
    bounds: AABB<[f64; 2]>,
    origin: [f64; 2],
    u: [f64; 2],
    v: [f64; 2],
    det: f64,
    weights: [f64; 3],
    id: usize,
    segment: usize,
    upstream: usize,
    downstream: usize,
}
impl RTreeObject for Triangle {
    type Envelope = AABB<[f64; 2]>;
    fn envelope(&self) -> Self::Envelope {
        self.bounds
    }
}

/// Geometry-only lookup; reusable for any profile with the same XS ordering.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Location {
    pub segment: usize,
    pub triangle: usize,
    pub upstream: usize,
    pub downstream: usize,
    pub upstream_weight: f32,
}

/// Immutable geometry. Callers own terrain sampling, row buffers and worker pools.
pub struct PreparedSurface {
    tree: RTree<Triangle>,
    cross_sections: usize,
}

impl PreparedSurface {
    /// Conservative admission estimate for input arrays, prepared triangles and
    /// R-tree construction scratch. Call before reading staged numeric payloads.
    /// This is an admission estimate, not an allocator or process RSS limit.
    pub fn planned_bytes(
        points: usize,
        triangles: usize,
        segments: usize,
    ) -> Result<usize, MapError> {
        points
            .checked_mul(32)
            .and_then(|v| triangles.checked_mul(1024).and_then(|t| v.checked_add(t)))
            .and_then(|v| {
                segments
                    .checked_mul(std::mem::size_of::<Segment>())
                    .and_then(|s| v.checked_add(s))
            })
            .ok_or_else(|| MapError::Budget("1D surface allocation overflow".into()))
    }

    pub fn new(
        points: &[[f64; 4]],
        triangles: &[[i32; 3]],
        segments: &[Segment],
        cross_sections: usize,
        max_bytes: usize,
    ) -> Result<Self, MapError> {
        if Self::planned_bytes(points.len(), triangles.len(), segments.len())? > max_bytes {
            return Err(MapError::Budget("1D surface exceeds admitted bytes".into()));
        }
        if cross_sections < 2 || segments.is_empty() {
            return Err(MapError::Invalid(
                "1D requires cross sections and segments".into(),
            ));
        }
        if points
            .iter()
            .any(|p| !p.iter().all(|x| x.is_finite()) || !(0.0..=1.0).contains(&p[2]))
        {
            return Err(MapError::Invalid(
                "1D points must be finite with upstream weights in [0,1]".into(),
            ));
        }
        let mut prepared = Vec::new();
        prepared
            .try_reserve_exact(triangles.len())
            .map_err(|e| MapError::Budget(format!("1D triangle allocation: {e}")))?;
        let (mut point_end, mut triangle_end) = (0, 0);
        for (segment, s) in segments.iter().enumerate() {
            if s.point_start != point_end
                || s.triangle_start != triangle_end
                || s.point_count < 3
                || s.triangle_count == 0
                || s.upstream >= cross_sections
                || s.downstream >= cross_sections
                || s.upstream == s.downstream
            {
                return Err(MapError::Invalid(format!(
                    "1D segment {segment}: invalid ranges or XS IDs"
                )));
            }
            point_end = s
                .point_start
                .checked_add(s.point_count)
                .filter(|end| *end <= points.len())
                .ok_or_else(|| MapError::Invalid("1D point slice out of bounds".into()))?;
            triangle_end = s
                .triangle_start
                .checked_add(s.triangle_count)
                .filter(|end| *end <= triangles.len())
                .ok_or_else(|| MapError::Invalid("1D triangle slice out of bounds".into()))?;
            let pts = &points[s.point_start..point_end];
            for (offset, indices) in triangles[s.triangle_start..triangle_end].iter().enumerate() {
                if indices.iter().any(|i| *i < 0 || *i as usize >= pts.len()) {
                    return Err(MapError::Invalid(
                        "1D triangle index outside segment-local points".into(),
                    ));
                }
                let [a, b, c] = indices.map(|i| pts[i as usize]);
                let u = [b[0] - a[0], b[1] - a[1]];
                let v = [c[0] - a[0], c[1] - a[1]];
                let det = u[0] * v[1] - u[1] * v[0];
                if !det.is_finite() || det == 0.0 {
                    return Err(MapError::Invalid(
                        "1D degenerate or overflowing triangle".into(),
                    ));
                }
                prepared.push(Triangle {
                    bounds: AABB::from_corners(
                        [a[0].min(b[0]).min(c[0]), a[1].min(b[1]).min(c[1])],
                        [a[0].max(b[0]).max(c[0]), a[1].max(b[1]).max(c[1])],
                    ),
                    origin: [a[0], a[1]],
                    u,
                    v,
                    det,
                    weights: [a[2], b[2], c[2]],
                    id: s.triangle_start + offset,
                    segment,
                    upstream: s.upstream,
                    downstream: s.downstream,
                });
            }
        }
        if point_end != points.len() || triangle_end != triangles.len() {
            return Err(MapError::Invalid(
                "1D segments must cover all stored points and triangles".into(),
            ));
        }
        Ok(Self {
            tree: RTree::bulk_load(prepared),
            cross_sections,
        })
    }

    pub fn locate(&self, point: [f64; 2]) -> Option<Location> {
        if !point.iter().all(|x| x.is_finite()) {
            return None;
        }
        self.tree
            .locate_in_envelope_intersecting(AABB::from_point(point))
            .filter_map(|t| {
                let x = point[0] - t.origin[0];
                let y = point[1] - t.origin[1];
                let b = (x * t.v[1] - y * t.v[0]) / t.det;
                let c = (t.u[0] * y - t.u[1] * x) / t.det;
                let a = 1.0 - b - c;
                if ![a, b, c].iter().all(|v| v.is_finite() && *v >= 0.0) {
                    return None;
                }
                let weight = (a * t.weights[0] + b * t.weights[1]) + c * t.weights[2];
                Some(Location {
                    segment: t.segment,
                    triangle: t.id,
                    upstream: t.upstream,
                    downstream: t.downstream,
                    upstream_weight: weight.clamp(0.0, 1.0) as f32,
                })
            })
            // Native stored surfaces are drawn in segment order. A later
            // segment owns overlaps even when its profile is dry or missing;
            // choosing a maximum WSE or falling back to an earlier wet segment
            // would silently change the complete validity mask.
            .min_by_key(|p| (std::cmp::Reverse(p.segment), p.triangle))
    }

    /// Bind a result vector once; NaN is missing, infinity is invalid.
    pub fn profile<'a>(&'a self, levels: &'a [f32]) -> Result<Profile<'a>, MapError> {
        if levels.len() != self.cross_sections || levels.iter().any(|z| z.is_infinite()) {
            return Err(MapError::Invalid(
                "1D profile must have one finite-or-NaN value per XS".into(),
            ));
        }
        Ok(Profile {
            surface: self,
            levels,
        })
    }
}

pub struct Profile<'a> {
    surface: &'a PreparedSurface,
    levels: &'a [f32],
}
impl Profile<'_> {
    /// Returns (depth, WSE), with a common validity mask. No extrapolation.
    pub fn sample(&self, point: [f64; 2], terrain: f32) -> Option<(f32, f32)> {
        if !terrain.is_finite() {
            return None;
        }
        let p = self.surface.locate(point)?;
        let us = self.levels[p.upstream];
        let ds = self.levels[p.downstream];
        if !us.is_finite() || !ds.is_finite() {
            return None;
        }
        let wse = p.upstream_weight * us + (1.0_f32 - p.upstream_weight) * ds;
        let depth = wse - terrain;
        (wse.is_finite() && depth.is_finite() && wse >= terrain + 0.001_f32).then_some((depth, wse))
    }
}
