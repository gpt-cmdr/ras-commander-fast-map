//! Experimental sloping reconstruction. Native qualification is stage-specific.
//!
//! Topology is immutable; hydraulic connectivity and corner arcs are rebuilt for each
//! profile. A supplied Rayon pool bounds parallel work without changing summation order.
//! Raster ownership, polygon weights and final depth delivery are separate stages.

use crate::MapError;
use rayon::prelude::*;
use std::sync::atomic::{AtomicBool, Ordering};

/// User-visible RAS Mapper sloping controls. Internal experimental modes are excluded.
///
/// `corner_fit_bound` is not a RAS Mapper control: it selects the opt-in method
/// [`BOUNDED_METHOD_ID`]. Absent (the default) means native corner fits. It is
/// omitted from the serialized options when absent, so native cache keys and
/// reports are unchanged.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct Options {
    #[serde(default)]
    pub render_mode: RenderMode,
    #[serde(default)]
    pub reduce_shallow_to_horizontal: bool,
    #[serde(default)]
    pub use_depth_weighted_faces: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corner_fit_bound: Option<CornerFitBound>,
}

/// Opt-in plain-sloping method: native corner plane fits, except that a fit with
/// three or more inputs outside its inputs' range by more than the tolerance (or
/// not finite) is clamped. Not a RAS Mapper method; not native-parity qualified.
pub const BOUNDED_METHOD_ID: &str = "rasmapper-plain-sloping-bounded-corners/1";
/// Reserved slopingPretty (with-faces) variant of [`BOUNDED_METHOD_ID`]. Refused
/// until it is qualified separately.
pub const BOUNDED_WITH_FACES_METHOD_ID: &str = "rasmapper-sloping-with-faces-bounded-corners/1";
/// Basis statement recorded with every bounded method record.
pub const BOUNDED_BASIS: &str = "bit-identical to rasmapper-plain-sloping/1 at every corner whose plane fit lies within its inputs' range \u{b1} tolerance; flagged corners clamped";

/// Corner-fit bound of [`BOUNDED_METHOD_ID`]. Both fields are required.
#[derive(Clone, Copy, Debug, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct CornerFitBound {
    pub mode: CornerFitBoundMode,
    /// Finite and >= 0, in the profile's WSE units. 0 is recommended.
    pub tolerance: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub enum CornerFitBoundMode {
    /// Bound by the group's own input face values at this profile.
    #[serde(rename = "input_range")]
    InputRange,
}

// Exact identity of the requested tolerance bits: a cache key, not arithmetic.
impl PartialEq for CornerFitBound {
    fn eq(&self, other: &Self) -> bool {
        self.mode == other.mode && self.tolerance.to_bits() == other.tolerance.to_bits()
    }
}
impl Eq for CornerFitBound {}

impl CornerFitBound {
    pub fn validate(self) -> Result<(), MapError> {
        if !self.tolerance.is_finite() || self.tolerance < 0.0 {
            return Err(MapError::Invalid(
                "corner_fit_bound tolerance must be finite and >= 0 (WSE units)".into(),
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub enum RenderMode {
    #[default]
    #[serde(rename = "sloping")]
    Corners,
    #[serde(rename = "slopingPretty")]
    WithFaces,
}
impl Options {
    pub fn validate(self) -> Result<(), MapError> {
        if self.render_mode == RenderMode::Corners
            && (self.reduce_shallow_to_horizontal || self.use_depth_weighted_faces)
        {
            return Err(MapError::Invalid(
                "shallow and depth-weighted controls require slopingPretty".into(),
            ));
        }
        if let Some(bound) = self.corner_fit_bound {
            bound.validate()?;
            if self.render_mode == RenderMode::WithFaces {
                return Err(MapError::Unsupported(format!(
                    "{BOUNDED_WITH_FACES_METHOD_ID} (slopingPretty with corner_fit_bound) is not qualified"
                )));
            }
        }
        Ok(())
    }
}

pub const NODATA: f32 = -9999.0;

/// Native uncached face application point: the first centre-line intersection
/// is accepted only within the middle 90% of the full face polyline.
pub fn face_application(points: &[[f64; 2]], centers: [[f64; 2]; 2]) -> Result<[f64; 2], MapError> {
    if points.len() < 2
        || points
            .iter()
            .flatten()
            .chain(centers.iter().flatten())
            .any(|v| !v.is_finite())
    {
        return Err(MapError::Invalid(
            "invalid sloping face polyline or centres".into(),
        ));
    }
    let [c, d] = centers;
    let cx = d[0] - c[0];
    let cy = d[1] - c[1];
    let length = |a: [f64; 2], b: [f64; 2]| {
        let x = b[0] - a[0];
        let y = b[1] - a[1];
        (x * x + y * y).sqrt()
    };
    let total = points.windows(2).map(|p| length(p[0], p[1])).sum::<f64>();
    let mut preceding = 0.0;
    for segment in points.windows(2) {
        let [a, b] = [segment[0], segment[1]];
        let before = preceding;
        let segment_length = length(a, b);
        preceding += segment_length;
        if (0..2).any(|axis| {
            a[axis].max(b[axis]) + 1e-6 < c[axis].min(d[axis])
                || c[axis].max(d[axis]) + 1e-6 < a[axis].min(b[axis])
        }) {
            continue;
        }
        let ax = b[0] - a[0];
        let ay = b[1] - a[1];
        let determinant = cy * ax - cx * ay;
        if determinant == 0.0 {
            continue;
        }
        let dy = a[1] - c[1];
        let dx = a[0] - c[0];
        let t = (cx * dy - cy * dx) / determinant;
        let u = (ax * dy - ay * dx) / determinant;
        if t > -1e-6 && t < 1.0 + 1e-6 && u > -1e-6 && u < 1.0 + 1e-6 {
            let fraction = (before + t.clamp(0.0, 1.0) * segment_length) / total;
            if fraction > 0.05 && fraction < 0.95 {
                // Native returns XY computed before clamping the segment fractions.
                return Ok([a[0] + t * ax, a[1] + t * ay]);
            }
            break;
        }
    }
    let half = total * 0.5;
    let mut running = 0.0;
    for segment in points.windows(2) {
        let before = running;
        let segment_length = length(segment[0], segment[1]);
        running += segment_length;
        if running >= half && segment_length > 0.0 {
            let t = (half - before) / segment_length;
            return Ok([
                segment[0][0] + t * (segment[1][0] - segment[0][0]),
                segment[0][1] + t * (segment[1][1] - segment[0][1]),
            ]);
        }
    }
    Ok(*points.last().unwrap())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Connection {
    None,
    Levee,
    Backfill,
    DownhillDeep,
    DownhillIntermediate,
    DownhillShallow,
}

impl Connection {
    pub fn connected(self) -> bool {
        !matches!(self, Self::None | Self::Levee)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Cell {
    pub minimum: f32,
    pub virtual_cell: bool,
}

#[derive(Clone, Debug)]
pub struct Face {
    pub cells: [usize; 2],
    pub vertices: [usize; 2],
    pub minimum: f32,
    pub perimeter: bool,
    pub application: [f64; 2],
}

#[derive(Clone, Debug)]
pub struct Corner {
    pub point: [f64; 2],
    pub faces: Vec<usize>,
    pub ccw: Vec<usize>,
    pub cw: Vec<usize>,
}

#[derive(Clone, Copy, Debug)]
pub struct FaceWater {
    pub values: [f32; 2],
    pub connection: Connection,
}

/// Oriented native mesh incidence. Face endpoint order is not a spatial sort key.
pub struct Mesh {
    pub cells: Vec<Cell>,
    pub faces: Vec<Face>,
    pub corners: Vec<Corner>,
}

pub struct Reconstruction {
    pub faces: Vec<FaceWater>,
    pub corners: Vec<Vec<f32>>,
}

impl Mesh {
    pub fn validate(&self) -> Result<(), MapError> {
        if self.cells.is_empty()
            || self.faces.is_empty()
            || self.corners.is_empty()
            || self.cells.iter().any(|c| !c.minimum.is_finite())
        {
            return Err(MapError::Invalid(
                "empty sloping mesh or nonfinite cell minimum".into(),
            ));
        }
        for face in &self.faces {
            if face.cells.iter().any(|&i| i >= self.cells.len())
                || face.vertices.iter().any(|&i| i >= self.corners.len())
                || !face.minimum.is_finite()
                || face.application.iter().any(|v| !v.is_finite())
            {
                return Err(MapError::Invalid(
                    "sloping face incidence out of range".into(),
                ));
            }
        }
        for (vertex, corner) in self.corners.iter().enumerate() {
            let n = corner.faces.len();
            if corner.ccw.len() != n
                || corner.cw.len() != n
                || corner.point.iter().any(|v| !v.is_finite())
            {
                return Err(MapError::Invalid(
                    "sloping corner ring length mismatch".into(),
                ));
            }
            for j in 0..n {
                let face = self
                    .faces
                    .get(corner.faces[j])
                    .ok_or_else(|| MapError::Invalid("sloping corner face out of range".into()))?;
                if !face.vertices.contains(&vertex)
                    || corner.ccw[j] >= n
                    || corner.cw[j] >= n
                    || corner.cw[corner.ccw[j]] != j
                {
                    return Err(MapError::Invalid(
                        "invalid sloping corner orientation".into(),
                    ));
                }
            }
            if n > 0 {
                let mut j = 0;
                for step in 0..n {
                    j = corner.ccw[j];
                    if (j == 0) != (step + 1 == n) {
                        return Err(MapError::Invalid(
                            "sloping incidence is not one ring".into(),
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    /// Face classification only (no corner reconstruction): the sloping face
    /// water surfaces of [`Mesh::reconstruct`].
    pub fn face_water(
        &self,
        wse: &[f32],
        plot_tolerance: f32,
        pool: &rayon::ThreadPool,
    ) -> Result<Vec<FaceWater>, MapError> {
        if wse.len() != self.cells.len()
            || wse.iter().any(|v| !v.is_finite())
            || !plot_tolerance.is_finite()
            || plot_tolerance < 0.0
        {
            return Err(MapError::Invalid(
                "invalid sloping profile or native plot tolerance".into(),
            ));
        }
        Ok(pool.install(|| {
            self.faces
                .par_iter()
                .map(|face| reconstruct_face(face, &self.cells, wse, plot_tolerance))
                .collect()
        }))
    }

    pub fn reconstruct(
        &self,
        wse: &[f32],
        plot_tolerance: f32,
        pool: &rayon::ThreadPool,
    ) -> Result<Reconstruction, MapError> {
        self.reconstruct_with_bound(wse, plot_tolerance, pool, None)
            .map(|(reconstruction, _)| reconstruction)
    }

    /// [`Mesh::reconstruct`] with an optional corner-fit bound. Without a bound
    /// the corner values are native and nothing else is returned; with one, the
    /// report counts every corner group with three or more inputs.
    pub fn reconstruct_with_bound(
        &self,
        wse: &[f32],
        plot_tolerance: f32,
        pool: &rayon::ThreadPool,
        bound: Option<CornerFitBound>,
    ) -> Result<(Reconstruction, Option<BoundedCorners>), MapError> {
        if let Some(bound) = bound {
            bound.validate()?;
        }
        self.validate()?;
        if wse.len() != self.cells.len()
            || wse.iter().any(|v| !v.is_finite())
            || !plot_tolerance.is_finite()
            || plot_tolerance < 0.0
        {
            return Err(MapError::Invalid(
                "invalid sloping profile or native plot tolerance".into(),
            ));
        }
        pool.install(|| {
            let faces: Vec<_> = self
                .faces
                .par_iter()
                .map(|face| reconstruct_face(face, &self.cells, wse, plot_tolerance))
                .collect();
            let Some(bound) = bound else {
                let corners = self
                    .corners
                    .par_iter()
                    .enumerate()
                    .map(|(i, corner)| reconstruct_corner(i, corner, &self.faces, &faces, None).0)
                    .collect();
                return Ok((Reconstruction { faces, corners }, None));
            };
            let context = BoundContext {
                bound,
                cells: &self.cells,
                wse,
                plot_tolerance,
            };
            let bounded: Vec<_> = self
                .corners
                .par_iter()
                .enumerate()
                .map(|(i, corner)| {
                    reconstruct_corner(i, corner, &self.faces, &faces, Some(&context))
                })
                .collect();
            // Counts and maxima are order-independent; merge serially.
            let mut report = CornerBoundReport::default();
            let mut corners = Vec::with_capacity(bounded.len());
            let mut input_max = Vec::with_capacity(bounded.len());
            for (values, maxima, corner_report) in bounded {
                report.merge(&corner_report);
                corners.push(values);
                input_max.push(maxima);
            }
            Ok((
                Reconstruction { faces, corners },
                Some(BoundedCorners { report, input_max }),
            ))
        })
    }
}

/// Per-profile record of [`BOUNDED_METHOD_ID`]: what the bound changed.
///
/// A corner group is a run of faces around one vertex between unconnected
/// faces; its value is one plane fit. Only groups with three or more inputs
/// can leave their input range. The depth and L fields are diagnostics of the
/// bounded groups; they never decide whether a group is bounded.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CornerBoundReport {
    /// Corner groups with three or more inputs (the groups the bound tests).
    pub groups_evaluated: u64,
    /// Fits above max(inputs) + tolerance, clamped to that value.
    pub bounded_up: u64,
    /// Fits below min(inputs) - tolerance, clamped to that value.
    pub bounded_down: u64,
    /// Non-finite fits, replaced by the mean of the inputs.
    pub bounded_nonfinite: u64,
    /// Largest |native fit - bounded value| over finite bounded fits (WSE units).
    pub max_removed_overshoot: f64,
    /// Bounded groups whose shallowest wet input cell is less than 0.1 deep.
    pub bounded_min_wet_input_depth_lt_0p1: u64,
    /// Bounded groups whose fit weights have L = sum |w_i| >= 3.
    pub bounded_l_ge_3: u64,
}

impl CornerBoundReport {
    /// Bounded corner groups: up, down and non-finite.
    pub fn bounded_corner_groups(&self) -> u64 {
        self.bounded_up + self.bounded_down + self.bounded_nonfinite
    }

    pub fn merge(&mut self, other: &Self) {
        self.groups_evaluated += other.groups_evaluated;
        self.bounded_up += other.bounded_up;
        self.bounded_down += other.bounded_down;
        self.bounded_nonfinite += other.bounded_nonfinite;
        self.max_removed_overshoot = self.max_removed_overshoot.max(other.max_removed_overshoot);
        self.bounded_min_wet_input_depth_lt_0p1 += other.bounded_min_wet_input_depth_lt_0p1;
        self.bounded_l_ge_3 += other.bounded_l_ge_3;
    }

    /// The `corner_fit_bound` record of a profile in session reports.
    pub fn to_json(&self, bound: CornerFitBound) -> serde_json::Value {
        serde_json::json!({
            "method_id": BOUNDED_METHOD_ID,
            "mode": bound.mode,
            "tolerance": bound.tolerance,
            "corner_groups_evaluated": self.groups_evaluated,
            "bounded_corner_groups": self.bounded_corner_groups(),
            "bounded_up": self.bounded_up,
            "bounded_down": self.bounded_down,
            "bounded_nonfinite": self.bounded_nonfinite,
            "max_removed_overshoot": self.max_removed_overshoot,
            "bounded_groups_min_wet_input_depth_lt_0p1": self.bounded_min_wet_input_depth_lt_0p1,
            "bounded_groups_L_ge_3": self.bounded_l_ge_3,
        })
    }
}

/// What a bounded reconstruction adds to the native one.
pub struct BoundedCorners {
    pub report: CornerBoundReport,
    /// Per vertex and ring slot: the highest WSE, at this profile, of any
    /// physical cell on a face feeding that slot's corner fit (wet or dry);
    /// negative infinity for a slot without inputs. Feeds [`input_excess`].
    pub input_max: Vec<Vec<f32>>,
}

/// The profile a bounded reconstruction reads: only the profile being rendered.
struct BoundContext<'a> {
    bound: CornerFitBound,
    cells: &'a [Cell],
    wse: &'a [f32],
    plot_tolerance: f32,
}

/// One regression input of a corner group: application point relative to the
/// vertex (as the regression translates it), face value, face.
type GroupInput = ([f64; 2], f32, usize);

impl BoundContext<'_> {
    /// Bound one corner group's native fit `value`. Groups with fewer than three
    /// inputs (native returns an input or a mean) and fits within range keep
    /// their exact native value.
    fn apply(
        &self,
        value: f32,
        regression: &Regression,
        inputs: &[GroupInput],
        faces: &[Face],
        report: &mut CornerBoundReport,
    ) -> f32 {
        if inputs.len() < 3 {
            return value;
        }
        report.groups_evaluated += 1;
        let (lo, hi) = inputs.iter().fold(
            (f32::INFINITY, f32::NEG_INFINITY),
            |(lo, hi), &(_, z, _)| (lo.min(z), hi.max(z)),
        );
        let tolerance = f64::from(self.bound.tolerance);
        let low = f64::from(lo) - tolerance;
        let high = f64::from(hi) + tolerance;
        let fit = f64::from(value);
        let bounded = if !value.is_finite() {
            report.bounded_nonfinite += 1;
            // Native's own fallback for a zero determinant.
            (regression.z / regression.n as f64) as f32
        } else if fit > high {
            report.bounded_up += 1;
            let bounded = high as f32;
            report.max_removed_overshoot =
                report.max_removed_overshoot.max(fit - f64::from(bounded));
            bounded
        } else if fit < low {
            report.bounded_down += 1;
            let bounded = low as f32;
            report.max_removed_overshoot =
                report.max_removed_overshoot.max(f64::from(bounded) - fit);
            bounded
        } else {
            return value;
        };
        if self.min_wet_input_depth(inputs, faces) < 0.1 {
            report.bounded_min_wet_input_depth_lt_0p1 += 1;
        }
        if regression.lebesgue_sum(inputs) >= 3.0 {
            report.bounded_l_ge_3 += 1;
        }
        bounded
    }

    /// Highest WSE of any physical cell on either side of the group's input
    /// faces (wet or dry), as the same-profile out-of-range test reads it.
    fn input_cell_max(&self, inputs: &[GroupInput], faces: &[Face]) -> f32 {
        inputs
            .iter()
            .flat_map(|&(_, _, f)| faces[f].cells)
            .filter(|&c| !self.cells[c].virtual_cell)
            .map(|c| self.wse[c])
            .fold(f32::NEG_INFINITY, f32::max)
    }

    /// Shallowest depth among the wet (native plot tolerance) physical cells on
    /// either side of the group's input faces; infinite when none is wet.
    fn min_wet_input_depth(&self, inputs: &[GroupInput], faces: &[Face]) -> f32 {
        inputs
            .iter()
            .flat_map(|&(_, _, f)| faces[f].cells)
            .filter(|&c| {
                !self.cells[c].virtual_cell
                    && self.wse[c] > self.cells[c].minimum + self.plot_tolerance
            })
            .map(|c| self.wse[c] - self.cells[c].minimum)
            .fold(f32::INFINITY, f32::min)
    }
}

/// Preserve binary32 intermediate rounding and native face classification.
fn reconstruct_face(face: &Face, cells: &[Cell], wse: &[f32], tolerance: f32) -> FaceWater {
    let [a, b] = face.cells;
    let [wa, wb] = [wse[a], wse[b]];
    let [za, zb] = [cells[a].minimum, cells[b].minimum];
    let dry_a = wa <= za + tolerance;
    let dry_b = wb <= zb + tolerance;
    if dry_a || dry_b || face.perimeter {
        return FaceWater {
            values: [
                if dry_a || cells[a].virtual_cell {
                    NODATA
                } else {
                    wa
                },
                if dry_b || cells[b].virtual_cell {
                    NODATA
                } else {
                    wb
                },
            ],
            connection: Connection::None,
        };
    }
    if wa <= face.minimum && wb <= face.minimum {
        return FaceWater {
            values: [wa, wb],
            connection: Connection::None,
        };
    }
    let (upstream, high, low) = if wa > wb { (a, wa, wb) } else { (b, wb, wa) };
    let floor = cells[upstream].minimum;
    let elevation_delta = zb - za;
    let backfill = (wb - wa) * elevation_delta <= 0.0;
    let relief = elevation_delta.abs();
    let depth = high - floor;
    let downstream = face.minimum.max(low);
    let submerged = downstream - floor;
    let head = high - face.minimum;
    let average = (wa + wb) * 0.5;
    let critical = head * (2.0_f32 / 3.0_f32) + face.minimum;
    let critical_cap = average.partial_cmp(&critical) != Some(std::cmp::Ordering::Greater);
    let levee = critical_cap && depth / head > 2.0;
    // The current adjusted native method retains the upstream WSE here;
    // the critical computation classifies connectivity, not an averaged level.
    if levee {
        return FaceWater {
            values: if upstream == a {
                [high, wb]
            } else {
                [wa, high]
            },
            connection: Connection::Levee,
        };
    }
    if backfill {
        return FaceWater {
            values: [high; 2],
            connection: Connection::Backfill,
        };
    }
    if depth >= 2.0 * relief {
        return FaceWater {
            values: [high; 2],
            connection: Connection::DownhillDeep,
        };
    }
    let mut value = downstream + (depth * depth - submerged * submerged) / (2.0 * relief);
    let connection = if depth > relief {
        value = ((2.0 * relief - depth) * value + (depth - relief) * high) / relief;
        Connection::DownhillIntermediate
    } else {
        Connection::DownhillShallow
    };
    FaceWater {
        values: [value; 2],
        connection,
    }
}

#[derive(Default)]
struct Regression {
    xx: f64,
    x: f64,
    yy: f64,
    y: f64,
    z: f64,
    xy: f64,
    yz: f64,
    xz: f64,
    n: usize,
}

impl Regression {
    fn add(&mut self, origin: [f64; 2], point: [f64; 2], value: f32) {
        let x = point[0] - origin[0];
        let y = point[1] - origin[1];
        let z = f64::from(value);
        self.xx += x * x;
        self.x += x;
        self.yy += y * y;
        self.y += y;
        self.z += z;
        self.xy += x * y;
        self.yz += y * z;
        self.xz += x * z;
        self.n += 1;
    }

    fn solve(&self) -> f32 {
        if self.n == 1 {
            return self.z as f32;
        }
        if self.n == 2 {
            return (self.z * 0.5) as f32;
        }
        let n = self.n as f64;
        let determinant = self.xx * (self.yy * n - self.y * self.y)
            - self.xy * (self.xy * n - self.y * self.x)
            + self.x * (self.xy * self.y - self.yy * self.x);
        if determinant == 0.0 {
            return (self.z / n) as f32;
        }
        ((self.xx * (self.yy * self.z - self.yz * self.y)
            - self.xy * (self.xy * self.z - self.yz * self.x)
            + self.xz * (self.xy * self.y - self.yy * self.x))
            / determinant) as f32
    }

    /// Diagnostic only: L = sum |w_i| of the fit's weights on its inputs
    /// (value = sum w_i z_i, sum w_i = 1; geometry only). Infinite when the
    /// normal matrix is singular. `inputs` are the regression's own inputs.
    fn lebesgue_sum(&self, inputs: &[GroupInput]) -> f64 {
        let n = self.n as f64;
        let determinant = self.xx * (self.yy * n - self.y * self.y)
            - self.xy * (self.xy * n - self.y * self.x)
            + self.x * (self.xy * self.y - self.yy * self.x);
        if determinant == 0.0 || !determinant.is_finite() {
            return f64::INFINITY;
        }
        // Intercept row of the inverse normal matrix (cofactors of row 3).
        let cx = self.xy * self.y - self.yy * self.x;
        let cy = self.xy * self.x - self.xx * self.y;
        let c1 = self.xx * self.yy - self.xy * self.xy;
        inputs
            .iter()
            .map(|&([x, y], _, _)| ((cx * x + cy * y + c1) / determinant).abs())
            .sum()
    }
}

/// Native corner plane fits around one vertex. With a bound (only the profile
/// being rendered), each group's fit passes [`BoundContext::apply`]; without
/// one, the values are native and the report stays empty.
fn reconstruct_corner(
    vertex: usize,
    corner: &Corner,
    faces: &[Face],
    values: &[FaceWater],
    bound: Option<&BoundContext<'_>>,
) -> (Vec<f32>, Vec<f32>, CornerBoundReport) {
    let mut report = CornerBoundReport::default();
    let mut output = vec![NODATA; corner.faces.len()];
    // Bounded only: the same-profile input maximum of each slot's fit.
    let mut input_max = match bound {
        None => Vec::new(),
        Some(_) => vec![f32::NEG_INFINITY; corner.faces.len()],
    };
    if corner
        .faces
        .iter()
        .all(|&f| values[f].values == [NODATA; 2])
    {
        return (output, input_max, report);
    }
    let mut inputs: Vec<GroupInput> = Vec::new();
    for j in 0..corner.faces.len() {
        if output[j] != NODATA {
            continue;
        }
        let mut stop = corner.ccw[j];
        while values[corner.faces[stop]].connection.connected() && stop != j {
            stop = corner.ccw[stop];
        }
        let mut first = j;
        if first != stop {
            while values[corner.faces[first]].connection.connected() {
                first = corner.cw[first];
            }
        }
        let mut regression = Regression::default();
        inputs.clear();
        let mut current = first;
        loop {
            let f = corner.faces[current];
            let side = usize::from(faces[f].vertices[0] != vertex);
            let value = values[f].values[side];
            if value != NODATA {
                regression.add(corner.point, faces[f].application, value);
                if bound.is_some() {
                    inputs.push((relative(corner.point, faces[f].application), value, f));
                }
            }
            output[current] = value;
            current = corner.ccw[current];
            if current == stop {
                break;
            }
        }
        let boundary = corner.faces[stop];
        if !values[boundary].connection.connected() {
            let side = usize::from(faces[boundary].vertices[0] == vertex);
            let value = values[boundary].values[side];
            if value != NODATA {
                regression.add(corner.point, faces[boundary].application, value);
                if bound.is_some() {
                    inputs.push((
                        relative(corner.point, faces[boundary].application),
                        value,
                        boundary,
                    ));
                }
            }
        }
        if regression.n > 0 {
            let value = regression.solve();
            let value = match bound {
                None => value,
                Some(context) => {
                    let maximum = context.input_cell_max(&inputs, faces);
                    current = first;
                    loop {
                        input_max[current] = maximum;
                        current = corner.ccw[current];
                        if current == stop {
                            break;
                        }
                    }
                    context.apply(value, &regression, &inputs, faces, &mut report)
                }
            };
            current = first;
            loop {
                output[current] = value;
                current = corner.ccw[current];
                if current == stop {
                    break;
                }
            }
        }
        if first == stop {
            break;
        }
    }
    (output, input_max, report)
}

/// The regression's own translation of an application point to the vertex.
fn relative(origin: [f64; 2], point: [f64; 2]) -> [f64; 2] {
    [point[0] - origin[0], point[1] - origin[1]]
}

/// Immutable coefficients for oriented face endpoints, not a centre-point TIN.
pub struct PolygonStencil {
    points: Vec<[f64; 2]>,
    corner_products: Vec<f64>,
}

/// Reuse one scratch buffer per worker; no per-pixel allocation is necessary.
pub struct WeightScratch {
    clipped: bool,
    cross: Vec<f32>,
    products: Vec<f64>,
    weights: Vec<f32>,
    donated: Vec<f32>,
    clockwise: Vec<f32>,
    counterclockwise: Vec<f32>,
}

fn cross(a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> f64 {
    let x1 = b[0] - a[0];
    let y1 = b[1] - a[1];
    let x2 = c[0] - a[0];
    let y2 = c[1] - a[1];
    x1 * y2 - y1 * x2
}

impl WeightScratch {
    /// Clears the retained donation scratch (native: every 256 points).
    pub(crate) fn clear_donation_history(&mut self) {
        self.clockwise.fill(0.);
        self.counterclockwise.fill(0.);
    }

    pub fn new(vertices: usize) -> Self {
        Self {
            clipped: false,
            cross: vec![0.0; vertices],
            products: vec![0.0; vertices],
            weights: vec![0.0; vertices],
            donated: vec![0.0; 2 * vertices],
            clockwise: vec![0.0; vertices],
            counterclockwise: vec![0.0; vertices],
        }
    }

    fn resize(&mut self, vertices: usize) {
        self.cross.resize(vertices, 0.0);
        self.products.resize(vertices, 0.0);
        self.weights.resize(vertices, 0.0);
        self.donated.resize(2 * vertices, 0.0);
        self.clockwise.resize(vertices, 0.0);
        self.counterclockwise.resize(vertices, 0.0);
    }
}

impl PolygonStencil {
    // The native square test intentionally rounds translated coordinates to f32.
    fn square_bounds(&self) -> Option<[f64; 4]> {
        if self.points.len() != 4 {
            return None;
        }
        let min_x = self
            .points
            .iter()
            .map(|p| p[0])
            .fold(f64::INFINITY, f64::min);
        let min_y = self
            .points
            .iter()
            .map(|p| p[1])
            .fold(f64::INFINITY, f64::min);
        let x: Vec<f32> = self.points.iter().map(|p| (p[0] - min_x) as f32).collect();
        let y: Vec<f32> = self.points.iter().map(|p| (p[1] - min_y) as f32).collect();
        let cx = (x[0] + x[1] + x[2] + x[3]) / 4.0;
        let cy = (y[0] + y[1] + y[2] + y[3]) / 4.0;
        let radius = (x[0] - cx).abs();
        if radius == 0.0
            || (0..4).any(|i| {
                ((x[i] - cx).abs() - radius).abs() > 0.001
                    || ((y[i] - cy).abs() - radius).abs() > 0.001
            })
        {
            return None;
        }
        Some([
            self.points[0][0].min(self.points[2][0]),
            self.points[0][1].min(self.points[2][1]),
            self.points[0][0].max(self.points[2][0]),
            self.points[0][1].max(self.points[2][1]),
        ])
    }

    fn square_weights<'a>(
        &self,
        bounds: [f64; 4],
        point: [f64; 2],
        scratch: &'a mut WeightScratch,
    ) -> &'a [f32] {
        let [left, bottom, right, top] = bounds;
        let mut x = (point[0] - left) / (right - left);
        let mut y = (point[1] - bottom) / (top - bottom);
        if x <= 0.0 {
            x = 0.00001;
        }
        if x >= 1.0 {
            x = 0.99999;
        }
        if y <= 0.0 {
            y = 0.00001;
        }
        if y >= 1.0 {
            y = 0.99999;
        }
        for (i, p) in self.points.iter().enumerate() {
            let wx = if p[0] <= (left + right) * 0.5 {
                1.0 - x
            } else {
                x
            };
            let wy = if p[1] <= (bottom + top) * 0.5 {
                1.0 - y
            } else {
                y
            };
            scratch.weights[i] = (wx * wy) as f32;
        }
        &scratch.weights
    }

    pub fn new(points: Vec<[f64; 2]>) -> Result<Self, MapError> {
        let n = points.len();
        if n < 3 || points.iter().flatten().any(|v| !v.is_finite()) {
            return Err(MapError::Invalid(
                "sloping polygon requires at least three finite endpoints".into(),
            ));
        }
        let corner_products = (0..n)
            .map(|j| cross(points[j], points[(j + 1) % n], points[(j + n - 1) % n]))
            .collect();
        Ok(Self {
            points,
            corner_products,
        })
    }

    pub fn weights<'a>(
        &self,
        point: [f64; 2],
        scratch: &'a mut WeightScratch,
    ) -> Result<&'a [f32], MapError> {
        let n = self.points.len();
        scratch.clipped = false;
        if scratch.weights.len() != n || point.iter().any(|v| !v.is_finite()) {
            return Err(MapError::Invalid(
                "sloping weight scratch dimension or query is invalid".into(),
            ));
        }
        for i in 0..n {
            let value = cross(point, self.points[i], self.points[(i + 1) % n]) as f32;
            scratch.cross[i] = if value == 0.0 { 0.00001_f32 } else { value };
        }
        let mut total = 0.0_f64;
        for j in 0..n {
            let previous = (j + n - 1) % n;
            let mut product = 1.0_f64;
            for k in 0..n {
                if k != j && k != previous {
                    product *= f64::from(scratch.cross[k]);
                }
            }
            scratch.products[j] = product * self.corner_products[j];
            total += scratch.products[j];
        }
        let mut has_negative_product = false;
        for j in 0..n {
            if scratch.products[j] < 0.0 {
                has_negative_product = true;
            }
            scratch.weights[j] = (scratch.products[j] / total) as f32;
            if scratch.weights[j] == 0.0 && scratch.products[j] != 0.0 {
                scratch.clipped = true;
            }
        }
        if has_negative_product {
            let mut total = 0.0_f32;
            for value in &mut scratch.weights {
                if *value < 0.0 {
                    scratch.clipped = true;
                    *value = 0.0;
                } else {
                    total += *value;
                }
            }
            if total != 0.0 {
                for value in &mut scratch.weights {
                    *value /= total;
                }
            }
        }
        Ok(&scratch.weights)
    }
}

/// Match native binary32 offset accumulation, including its nodata convention.
pub fn paint_corner_values(weights: &[f32], values: &[f32]) -> Result<f32, MapError> {
    if weights.len() != values.len() || values.is_empty() {
        return Err(MapError::Invalid(
            "sloping paint dimensions do not match".into(),
        ));
    }
    let anchor = values[0];
    let mut delta = 0.0_f32;
    let mut count = 0;
    for (&weight, &value) in weights.iter().zip(values) {
        let difference = value - anchor;
        if difference != NODATA {
            count += 1;
            delta += weight * difference;
        }
    }
    Ok(if count > 0 { delta + anchor } else { NODATA })
}

/// Native plot tolerance is part of the method; there is no extra depth cutoff.
pub fn deliver_pixel(wse: f32, terrain: f32, native_tolerance: f32) -> Option<[f32; 2]> {
    if !wse.is_finite()
        || !terrain.is_finite()
        || wse == NODATA
        || terrain == NODATA
        || wse < terrain + native_tolerance
    {
        None
    } else {
        Some([wse - terrain, wse])
    }
}

pub struct CellStencil {
    pub(crate) square_center: Option<[f64; 2]>,
    pub physical_ring: Vec<[f64; 2]>,
    polygon: PolygonStencil,
    pub(crate) square: Option<[f64; 4]>,
    corners: Vec<[usize; 2]>, // vertex, incident-face slot
    faces: Vec<[usize; 2]>,   // face, cell side
    elevations: Option<Vec<f32>>,
}

/// Prepare immutable geometry once. Profiles never determine ring ordering.
pub fn prepare_stencils(
    mesh: &Mesh,
    cell_faces: &[Vec<usize>],
) -> Result<Vec<Option<CellStencil>>, MapError> {
    mesh.validate()?;
    if cell_faces.len() != mesh.cells.len() {
        return Err(MapError::Invalid(
            "sloping cell-face table length mismatch".into(),
        ));
    }
    cell_faces
        .iter()
        .enumerate()
        .map(|(cell, faces)| {
            if mesh.cells[cell].virtual_cell {
                return Ok(None);
            }
            prepare_stencil(mesh, cell, faces).map(Some)
        })
        .collect()
}

/// One physical cell's stencil from its ordered faces, on an already validated mesh.
/// Shared by complete and staged-subset geometry so ring ordering cannot diverge.
pub(crate) fn prepare_stencil(
    mesh: &Mesh,
    cell: usize,
    faces: &[usize],
) -> Result<CellStencil, MapError> {
    let mut points = Vec::with_capacity(faces.len());
    let mut corners = Vec::with_capacity(faces.len());
    for &id in faces {
        let face = mesh
            .faces
            .get(id)
            .ok_or_else(|| MapError::Invalid("cell face out of range".into()))?;
        let vertex = if face.cells[0] == cell {
            face.vertices[0]
        } else if face.cells[1] == cell {
            face.vertices[1]
        } else {
            return Err(MapError::Invalid("cell not incident to face".into()));
        };
        let corner = &mesh.corners[vertex];
        let slot = corner
            .faces
            .iter()
            .position(|&f| f == id)
            .ok_or_else(|| MapError::Invalid("cell face not incident to corner".into()))?;
        points.push(corner.point);
        corners.push([vertex, slot]);
    }
    let polygon = PolygonStencil::new(points)?;
    let square = polygon.square_bounds();
    Ok(CellStencil {
        square_center: None,
        physical_ring: Vec::new(),
        polygon,
        square,
        corners,
        faces: faces
            .iter()
            .map(|&id| [id, usize::from(mesh.faces[id].cells[0] != cell)])
            .collect(),
        elevations: None,
    })
}

/// Native square centre and oriented physical perimeter of one cell's stencil.
/// `faces` are the cell's faces in ring order as (face cells, full polyline).
pub(crate) fn set_physical_ring<'a>(
    stencil: &mut CellStencil,
    cell: usize,
    center: [f64; 2],
    faces: impl IntoIterator<Item = ([usize; 2], &'a [[f64; 2]])>,
) {
    stencil.square_center = Some(center);
    for (cells, polyline) in faces {
        if cells[0] == cell {
            stencil.physical_ring.extend(polyline.iter().copied());
        } else {
            stencil.physical_ring.extend(polyline.iter().rev().copied());
        }
    }
    stencil.physical_ring.dedup();
}

impl CellStencil {
    /// Axis-aligned square bounds for native velocity scan conversion.
    pub(crate) fn square_bounds(&self) -> Option<[f64; 4]> {
        self.square
    }

    /// Corner (vertex, incident-face slot) pairs in ring order.
    pub(crate) fn corner_slots(&self) -> impl Iterator<Item = [usize; 2]> + '_ {
        self.corners.iter().copied()
    }
    /// Face ids in ring order (face i joins corner i and corner i + 1).
    pub(crate) fn face_ids(&self) -> impl Iterator<Item = usize> + '_ {
        self.faces.iter().map(|f| f[0])
    }
    /// Plain facepoint weights at `point`:
    /// the square or polygon weights painted by plain-corner sloping.
    pub(crate) fn plain_weights<'a>(
        &self,
        point: [f64; 2],
        scratch: &'a mut WeightScratch,
    ) -> Result<&'a [f32], MapError> {
        scratch.resize(self.corners.len());
        if let Some(bounds) = self.square {
            Ok(self.polygon.square_weights(bounds, point, scratch))
        } else {
            self.polygon.weights(point, scratch)
        }
    }

    /// Native face-donation node weights at `point`: `n` corner weights then
    /// `n` face weights, binary32, as used by cell-face sloping painting.
    pub(crate) fn donated_weights<'a>(
        &self,
        point: [f64; 2],
        scratch: &'a mut WeightScratch,
    ) -> Result<&'a [f32], MapError> {
        self.donated_weights_with_history(point, scratch, false)
    }

    /// True when the last weights were clipped such that a corner's neighbours
    /// both carry zero weight: native donation then depends on scratch history.
    pub(crate) fn history_sensitive(&self, scratch: &WeightScratch) -> bool {
        let n = self.corners.len();
        scratch.clipped
            && (0..n)
                .any(|k| scratch.weights[(k + n - 1) % n] + scratch.weights[(k + 1) % n] == 0.0)
    }

    /// As `donated_weights`; with `retain`, clockwise/counter-clockwise
    /// donations of corners with zero-weight neighbours keep the previous
    /// pixel's values (native scratch history).
    pub(crate) fn donated_weights_with_history<'a>(
        &self,
        point: [f64; 2],
        scratch: &'a mut WeightScratch,
        retain: bool,
    ) -> Result<&'a [f32], MapError> {
        let n = self.corners.len();
        scratch.resize(n);
        if let Some(bounds) = self.square {
            self.polygon.square_weights(bounds, point, scratch);
        } else {
            self.polygon.weights(point, scratch)?;
        }
        for i in 0..n {
            let previous = (i + n - 1) % n;
            let next = (i + 1) % n;
            let adjacent = scratch.weights[previous] + scratch.weights[next];
            if !retain {
                scratch.clockwise[i] = 0.0;
                scratch.counterclockwise[i] = 0.0;
            }
            if adjacent != 0.0 {
                let fraction = scratch.weights[i] / adjacent;
                scratch.clockwise[i] = fraction * scratch.weights[previous];
                scratch.counterclockwise[i] = fraction * scratch.weights[next];
            }
            scratch.donated[i] = scratch.weights[i];
        }
        for i in 0..n {
            let next = (i + 1) % n;
            let donation = scratch.counterclockwise[i].min(scratch.clockwise[next]);
            scratch.donated[i] -= donation;
            scratch.donated[next] -= donation;
            scratch.donated[n + i] = donation * 2.0;
        }
        Ok(&scratch.donated[..2 * n])
    }
}

/// Optional native stored corner elevations, required only for depth weighting.
pub fn set_vertex_elevations(
    stencils: &mut [Option<CellStencil>],
    elevations: &[f32],
    vertex_count: usize,
) -> Result<(), MapError> {
    if elevations.len() != vertex_count || elevations.iter().any(|x| !x.is_finite()) {
        return Err(MapError::Invalid(
            "sloping corner elevation dimensions or values invalid".into(),
        ));
    }
    for stencil in stencils.iter_mut().flatten() {
        stencil.elevations = Some(
            stencil
                .corners
                .iter()
                .map(|&[vertex, _]| elevations[vertex])
                .collect(),
        );
    }
    Ok(())
}

pub struct PreparedProfile {
    local_values: Vec<Vec<f32>>,
    face_values: Vec<Vec<f32>>,
    depth_weights: Vec<Vec<f32>>,
    horizontal: Vec<Option<f32>>,
    wet: Vec<bool>,
    options: Options,
    corner_bound: Option<CornerBoundReport>,
    /// Bounded only: per cell, the highest same-profile WSE of any physical
    /// cell feeding its stencil's corner fits (see [`input_excess`]).
    input_max: Vec<f32>,
}

impl PreparedProfile {
    /// Retained heap allocation including nested vector headers/capacities.
    pub(crate) fn retained_bytes(&self) -> u64 {
        fn nested(v: &Vec<Vec<f32>>) -> usize {
            v.capacity() * std::mem::size_of::<Vec<f32>>()
                + v.iter().map(|v| v.capacity() * 4).sum::<usize>()
        }
        (std::mem::size_of::<Self>()
            + nested(&self.local_values)
            + nested(&self.face_values)
            + nested(&self.depth_weights)
            + self.horizontal.capacity() * std::mem::size_of::<Option<f32>>()
            + self.wet.capacity()
            + self.input_max.capacity() * 4) as u64
    }
    /// A staged mesh without owner cells: rendering reads none of its cells.
    pub(crate) fn unread(options: Options) -> Self {
        Self {
            local_values: Vec::new(),
            face_values: Vec::new(),
            depth_weights: Vec::new(),
            horizontal: Vec::new(),
            wet: Vec::new(),
            options,
            corner_bound: options
                .corner_fit_bound
                .map(|_| CornerBoundReport::default()),
            input_max: Vec::new(),
        }
    }
    /// The corner-fit bound record of this profile; `None` for native options.
    pub fn corner_bound_report(&self) -> Option<&CornerBoundReport> {
        self.corner_bound.as_ref()
    }
    /// Native wet gate of a cell: WSE above its minimum plus the plot tolerance.
    pub fn is_wet(&self, cell: usize) -> bool {
        self.wet[cell]
    }
}

pub fn prepare_profile(
    mesh: &Mesh,
    stencils: &[Option<CellStencil>],
    values: &[f32],
    tolerance: f32,
    pool: &rayon::ThreadPool,
) -> Result<PreparedProfile, MapError> {
    prepare_profile_with_options(mesh, stencils, values, tolerance, pool, Options::default())
}

pub fn prepare_profile_with_options(
    mesh: &Mesh,
    stencils: &[Option<CellStencil>],
    values: &[f32],
    tolerance: f32,
    pool: &rayon::ThreadPool,
    options: Options,
) -> Result<PreparedProfile, MapError> {
    options.validate()?;
    if stencils.len() != mesh.cells.len() {
        return Err(MapError::Invalid("sloping stencil count mismatch".into()));
    }
    if options.use_depth_weighted_faces && stencils.iter().flatten().any(|s| s.elevations.is_none())
    {
        return Err(MapError::Unsupported(
            "depth weighting requires stored native FacePoints Elevation".into(),
        ));
    }
    let (reconstructed, bounded) =
        mesh.reconstruct_with_bound(values, tolerance, pool, options.corner_fit_bound)?;
    let (corner_bound, input_max) = match bounded {
        None => (None, Vec::new()),
        Some(bounded) => {
            let maxima = &bounded.input_max;
            let input_max = pool.install(|| {
                stencils
                    .par_iter()
                    .map(|s| match s {
                        Some(s) => s
                            .corners
                            .iter()
                            .map(|&[vertex, slot]| maxima[vertex][slot])
                            .fold(f32::NEG_INFINITY, f32::max),
                        None => f32::NEG_INFINITY,
                    })
                    .collect()
            });
            (Some(bounded.report), input_max)
        }
    };
    let local_values: Vec<Vec<f32>> = pool.install(|| {
        stencils
            .par_iter()
            .map(|s| match s {
                Some(s) => s
                    .corners
                    .iter()
                    .map(|&[vertex, slot]| reconstructed.corners[vertex][slot])
                    .collect(),
                None => Vec::new(),
            })
            .collect()
    });
    let face_values: Vec<Vec<f32>> = if options.render_mode == RenderMode::WithFaces {
        pool.install(|| {
            stencils
                .par_iter()
                .map(|s| match s {
                    Some(s) => s
                        .faces
                        .iter()
                        .map(|&[face, side]| reconstructed.faces[face].values[side])
                        .collect(),
                    None => Vec::new(),
                })
                .collect()
        })
    } else {
        Vec::new()
    };
    let depth_weights = if options.use_depth_weighted_faces {
        pool.install(|| {
            stencils
                .par_iter()
                .enumerate()
                .map(|(cell, s)| match s {
                    Some(s) => {
                        let mut result: Vec<_> = local_values[cell]
                            .iter()
                            .zip(s.elevations.as_ref().unwrap())
                            .map(|(&w, &z)| depth_weight(w, z))
                            .collect();
                        result.extend(
                            s.faces
                                .iter()
                                .zip(&face_values[cell])
                                .map(|(&[face, _], &w)| depth_weight(w, mesh.faces[face].minimum)),
                        );
                        result
                    }
                    None => Vec::new(),
                })
                .collect()
        })
    } else {
        Vec::new()
    };
    let horizontal = stencils
        .iter()
        .enumerate()
        .map(|(cell, s)| {
            s.as_ref().and_then(|s| {
                (options.reduce_shallow_to_horizontal
                    && s.faces.iter().all(|&[face, _]| {
                        let c = reconstructed.faces[face].connection;
                        !c.connected() || c == Connection::DownhillShallow
                    }))
                .then_some(values[cell])
            })
        })
        .collect();
    let wet = mesh
        .cells
        .iter()
        .zip(values)
        .map(|(cell, &value)| !cell.virtual_cell && value > cell.minimum + tolerance)
        .collect();
    Ok(PreparedProfile {
        local_values,
        face_values,
        depth_weights,
        horizontal,
        wet,
        options,
        corner_bound,
        input_max,
    })
}

/// Margin of the same-profile out-of-range test: SPIKE-SWEEP's 0.5 plus the
/// largest measured face-over-cell rebuild excess, 0.22, in WSE units (ft).
pub const INPUT_EXCESS_MARGIN: f64 = 0.72;

/// The same-profile out-of-range test of one rendered window (bounded method
/// only): a wet pixel is out of range when its painted WSE exceeds, by more than
/// [`INPUT_EXCESS_MARGIN`], the highest WSE at that profile of every physical
/// cell feeding its owning cell's corner fits. A painted value is a convex
/// combination of its cell's corners, so a value above every such input can
/// only come from a corner fit outside its inputs' range.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InputExcess {
    pub wet_pixels: u64,
    pub out_of_range_pixels: u64,
    /// Largest painted WSE minus input maximum over wet pixels (negative when
    /// every pixel lies below its inputs; negative infinity without wet pixels).
    pub max_excess: f64,
}

impl InputExcess {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({"margin": INPUT_EXCESS_MARGIN, "wet_pixels": self.wet_pixels,
            "out_of_range_pixels": self.out_of_range_pixels,
            "max_excess": if self.max_excess.is_finite() { Some(self.max_excess) } else { None }})
    }
}

/// Run the test over `wse` (finalized: NaN where dry), the window's pixels from
/// row-major index `start`. `None` when a profile was not bounded.
pub fn input_excess(
    profiles: &[PreparedProfile],
    window: &RasterWindow<'_>,
    wse: &[f32],
    start: usize,
) -> Option<InputExcess> {
    if profiles
        .iter()
        .any(|p| p.options.corner_fit_bound.is_none())
    {
        return None;
    }
    let mut result = InputExcess {
        wet_pixels: 0,
        out_of_range_pixels: 0,
        max_excess: f64::NEG_INFINITY,
    };
    for (index, &value) in wse.iter().enumerate() {
        let label = window.owner[start + index] as usize;
        if label == 0 || !value.is_finite() {
            continue;
        }
        let [mesh, cell] = window.owner_cells[label - 1];
        let Some(&limit) = profiles[mesh].input_max.get(cell) else {
            continue;
        };
        result.wet_pixels += 1;
        let excess = f64::from(value) - f64::from(limit);
        result.max_excess = result.max_excess.max(excess);
        if excess > INPUT_EXCESS_MARGIN {
            result.out_of_range_pixels += 1;
        }
    }
    Some(result)
}

fn depth_weight(wse: f32, elevation: f32) -> f32 {
    if wse == NODATA || elevation == NODATA || wse <= elevation {
        0.01_f32
    } else {
        f64::from(wse - elevation).max(0.01_f64) as f32
    }
}

impl WeightScratch {
    /// Native node donation: all arithmetic remains binary32 and in ring order.
    fn paint_with_faces(&mut self, corners: &[f32], faces: &[f32], depths: Option<&[f32]>) -> f32 {
        self.paint_with_face_history(corners, faces, depths, false)
    }

    fn paint_with_face_history(
        &mut self,
        corners: &[f32],
        faces: &[f32],
        depths: Option<&[f32]>,
        retain: bool,
    ) -> f32 {
        let n = corners.len();
        for i in 0..n {
            let previous = (i + n - 1) % n;
            let next = (i + 1) % n;
            let adjacent = self.weights[previous] + self.weights[next];
            // The independent pass starts fresh; affected cells are replayed
            // below with native scratch retention inside each geometry batch.
            if !retain {
                self.clockwise[i] = 0.0;
                self.counterclockwise[i] = 0.0;
            }
            if adjacent != 0.0 {
                let fraction = self.weights[i] / adjacent;
                self.clockwise[i] = fraction * self.weights[previous];
                self.counterclockwise[i] = fraction * self.weights[next];
            }
            self.donated[i] = self.weights[i];
        }
        for i in 0..n {
            let next = (i + 1) % n;
            let donation = self.counterclockwise[i].min(self.clockwise[next]);
            self.donated[i] -= donation;
            self.donated[next] -= donation;
            self.donated[n + i] = donation * 2.0;
        }
        let mut total = 0.0_f64;
        if let Some(depths) = depths {
            for (weight, &depth) in self.donated.iter_mut().zip(depths) {
                *weight *= depth;
                total += f64::from(*weight);
            }
        }
        let anchor = faces[0];
        let mut delta = 0.0_f32;
        for (i, &corner) in corners.iter().enumerate() {
            delta += (corner - anchor) * self.donated[i];
        }
        for (i, &face) in faces.iter().enumerate() {
            delta += (face - anchor) * self.donated[n + i];
        }
        if depths.is_some() {
            (f64::from(delta) / total) as f32 + anchor
        } else {
            delta + anchor
        }
    }
}

/// Internal signed-depth surface path. Raster plot tolerance is deliberately
/// not applied here: zero-contour interpolation needs valid dry-side samples.
/// The caller subtracts valid terrain from the returned water-surface value.
/// The resulting depth uses Option, so a numeric depth of -9999 is not missing.
pub(crate) fn boundary_surface_value(
    stencils: &[Vec<Option<CellStencil>>],
    profiles: &[PreparedProfile],
    mesh: usize,
    cell: usize,
    point: [f64; 2],
    scratch: &mut WeightScratch,
) -> Result<Option<f32>, MapError> {
    let profile = profiles
        .get(mesh)
        .ok_or_else(|| MapError::Invalid("boundary surface mesh out of range".into()))?;
    if profile.options.render_mode != RenderMode::Corners {
        return Err(MapError::Unsupported(
            "boundary contours currently require plain corner sloping; face options are not qualified".into(),
        ));
    }
    if point.iter().any(|v| !v.is_finite()) {
        return Err(MapError::Invalid("nonfinite boundary surface query".into()));
    }
    let wet = *profile
        .wet
        .get(cell)
        .ok_or_else(|| MapError::Invalid("boundary surface cell out of range".into()))?;
    if !wet {
        return Ok(None);
    }
    let cells = stencils
        .get(mesh)
        .ok_or_else(|| MapError::Invalid("boundary stencil mesh out of range".into()))?;
    let stencil = cells
        .get(cell)
        .ok_or_else(|| MapError::Invalid("boundary stencil cell out of range".into()))?;
    let Some(stencil) = stencil else {
        return Ok(None);
    };
    if let Some(value) = profile.horizontal[cell] {
        return Ok((value != NODATA).then_some(value));
    }
    let values = &profile.local_values[cell];
    scratch.resize(values.len());
    let weights = if let Some(bounds) = stencil.square {
        stencil.polygon.square_weights(bounds, point, scratch)
    } else {
        stencil.polygon.weights(point, scratch)?
    };
    let value = paint_corner_values(weights, values)?;
    if !value.is_finite() {
        return Err(MapError::Invalid(
            "nonfinite boundary surface interpolation".into(),
        ));
    }
    Ok((value != NODATA).then_some(value))
}

/// Geometry and terrain belong to the caller and can be reused across timesteps.
/// Labels index `owner_cells` plus one; zero is outside the physical 2D footprint.
pub struct RasterWindow<'a> {
    /// Selected logical source layout and integer global [column,row] origin.
    pub native_grid: Option<(crate::stored_grid::Layout, [usize; 2])>,
    pub source_transform: [f64; 6],
    pub source_tile: [usize; 2],
    pub width: usize,
    pub height: usize,
    pub transform: [f64; 6],
    pub owner: &'a [u32],
    pub terrain: &'a [f32],
    pub owner_cells: &'a [[usize; 2]], // mesh, local cell
}

impl<'a> RasterWindow<'a> {
    fn validate_native_grid(&self) -> Result<(), MapError> {
        if let Some((layout, origin)) = self.native_grid {
            let window = [origin[0], origin[1], self.width, self.height];
            layout.validate_window(window)?;
            if !crate::stored_grid::same(self.source_transform, layout.source)
                || self.source_tile != [256, 256]
                || !crate::stored_grid::same(self.transform, layout.window_transform(window))
            {
                return Err(MapError::Invalid(
                    "stored sampling window/source framing mismatch".into(),
                ));
            }
        }
        Ok(())
    }
    /// Construct the ordinary source-affine window without selecting a stored
    /// tile contract. Prefer this constructor to public struct literals.
    pub fn new(
        source_transform: [f64; 6],
        source_tile: [usize; 2],
        dimensions: [usize; 2],
        transform: [f64; 6],
        owner: &'a [u32],
        terrain: &'a [f32],
        owner_cells: &'a [[usize; 2]],
    ) -> Self {
        Self {
            native_grid: None,
            source_transform,
            source_tile,
            width: dimensions[0],
            height: dimensions[1],
            transform,
            owner,
            terrain,
            owner_cells,
        }
    }
    /// Explicitly select logical source-tile sampling and its delivery framing.
    pub fn with_stored_grid(
        mut self,
        layout: crate::stored_grid::Layout,
        origin: [usize; 2],
    ) -> Result<Self, MapError> {
        let window = [origin[0], origin[1], self.width, self.height];
        layout.validate_window(window)?;
        if !crate::stored_grid::same(self.source_transform, layout.source)
            || self.source_tile != [256, 256]
        {
            return Err(MapError::Invalid(
                "stored sampling source identity mismatch".into(),
            ));
        }
        self.transform = layout.window_transform(window);
        self.native_grid = Some((layout, origin));
        self.validate_native_grid()?;
        Ok(self)
    }
}

impl CellStencil {
    #[allow(clippy::neg_multiply)] // Retain explicit binary64 stage order of the selected grid contract.
    fn window_weights<'a>(
        &self,
        window: &RasterWindow<'_>,
        index: usize,
        scratch: &'a mut WeightScratch,
    ) -> Result<&'a [f32], MapError> {
        let (row, col) = (index / window.width, index % window.width);
        let point = if let Some((layout, [x, y])) = window.native_grid {
            layout.point(y + row, x + col)
        } else {
            [
                window.transform[0] + (col as f64 + 0.5) * window.transform[1],
                window.transform[3] + (row as f64 + 0.5) * window.transform[5],
            ]
        };
        let Some(bounds) = self.square else {
            return self.polygon.weights(point, scratch);
        };
        let Some((layout, [xoff, yoff])) = window.native_grid else {
            return Ok(self.polygon.square_weights(bounds, point, scratch));
        };
        let (global_row, global_col) = (yoff + row, xoff + col);
        let tile = layout.tile(global_row / 256, global_col / 256);
        let [first_col, _, first_row, _] = tile
            .rectangle(bounds)
            .ok_or_else(|| MapError::Invalid("square owner has no logical tile coverage".into()))?;
        let [left, bottom, right, top] = bounds;
        let seed_x = (tile.x(first_col) - left) / (right - left);
        let seed_y = (tile.y(first_row) - bottom) / (top - bottom);
        let mut x =
            seed_x + (tile.step / (right - left)) * ((global_col % 256) as f64 - first_col as f64);
        let mut y = seed_y
            + ((-1.0 * tile.step) / (top - bottom))
                * ((global_row % 256) as f64 - first_row as f64);
        if x <= 0. {
            x = 0.00001;
        }
        if x >= 1. {
            x = 0.99999;
        }
        if y <= 0. {
            y = 0.00001;
        }
        if y >= 1. {
            y = 0.99999;
        }
        scratch.clipped = false;
        let center = self
            .square_center
            .unwrap_or([(left + right) * 0.5, (bottom + top) * 0.5]);
        for (i, p) in self.polygon.points.iter().enumerate() {
            let wx = if p[0] <= center[0] { 1. - x } else { x };
            let wy = if p[1] <= center[1] { 1. - y } else { y };
            scratch.weights[i] = (wx * wy) as f32;
        }
        Ok(&scratch.weights)
    }
}

/// Bounded immutable geometry-only weights for a prefix of one output window.
/// Face rendering keeps its original scratch/history path.
pub(crate) struct PixelWeights {
    stride: usize,
    values: Vec<f32>,
}

impl PixelWeights {
    pub(crate) fn prepare(
        stencils: &[Vec<Option<CellStencil>>],
        window: &RasterWindow<'_>,
        budget: usize,
        pool: &rayon::ThreadPool,
    ) -> Result<Self, MapError> {
        window.validate_native_grid()?;
        let stride = stencils
            .iter()
            .flatten()
            .filter_map(|s| s.as_ref())
            .map(|s| s.polygon.points.len())
            .max()
            .unwrap_or(1);
        if window.width == 0
            || window
                .owner
                .iter()
                .any(|&label| label as usize > window.owner_cells.len())
            || window
                .owner_cells
                .iter()
                .any(|&[mesh, cell]| mesh >= stencils.len() || cell >= stencils[mesh].len())
        {
            return Err(MapError::Invalid("invalid cached pixel addresses".into()));
        }
        let row_bytes = stride
            .checked_mul(4)
            .ok_or_else(|| MapError::Budget("weight cache stride overflow".into()))?;
        let count = window.owner.len().min(budget / row_bytes);
        let mut values = vec![0.0; count * stride];
        pool.install(|| {
            values.par_chunks_mut(stride).enumerate().try_for_each_init(
                || WeightScratch::new(0),
                |scratch, (i, row)| {
                    let label = window.owner[i] as usize;
                    if label != 0 {
                        let [mesh, cell] = window.owner_cells[label - 1];
                        if let Some(stencil) = &stencils[mesh][cell] {
                            scratch.resize(stencil.polygon.points.len());
                            let weights = stencil.window_weights(window, i, scratch)?;
                            row[..weights.len()].copy_from_slice(weights);
                        }
                    }
                    Ok::<_, MapError>(())
                },
            )
        })?;
        Ok(Self { stride, values })
    }
    pub fn bytes(&self) -> usize {
        self.values.len() * 4
    }
    fn get(&self, index: usize, len: usize) -> Option<&[f32]> {
        (index < self.values.len() / self.stride && len <= self.stride)
            .then(|| &self.values[index * self.stride..index * self.stride + len])
    }
}

pub fn render_profile(
    stencils: &[Vec<Option<CellStencil>>],
    profiles: &[PreparedProfile],
    window: &RasterWindow<'_>,
    tolerance: f32,
    pool: &rayon::ThreadPool,
) -> Result<(Vec<f32>, Vec<f32>), MapError> {
    render_profile_cached(stencils, profiles, window, tolerance, pool, None)
}

pub(crate) fn render_profile_cached(
    stencils: &[Vec<Option<CellStencil>>],
    profiles: &[PreparedProfile],
    window: &RasterWindow<'_>,
    tolerance: f32,
    pool: &rayon::ThreadPool,
    cache: Option<&PixelWeights>,
) -> Result<(Vec<f32>, Vec<f32>), MapError> {
    render_profile_range(
        stencils,
        profiles,
        window,
        tolerance,
        pool,
        cache,
        0,
        window.height,
    )
}

/// Plain-corner sloping rows using the original window's affine arithmetic.
/// Output buffers contain only the requested rows; global hydraulic inputs,
/// ownership and source coordinates are unchanged.
pub fn render_profile_rows(
    stencils: &[Vec<Option<CellStencil>>],
    profiles: &[PreparedProfile],
    window: &RasterWindow<'_>,
    tolerance: f32,
    pool: &rayon::ThreadPool,
    row_start: usize,
    row_count: usize,
) -> Result<(Vec<f32>, Vec<f32>), MapError> {
    if profiles
        .iter()
        .any(|p| p.options.render_mode == RenderMode::WithFaces)
    {
        return Err(MapError::Unsupported(
            "row streaming requires plain corner sloping".into(),
        ));
    }
    render_profile_range(
        stencils, profiles, window, tolerance, pool, None, row_start, row_count,
    )
}

#[allow(clippy::too_many_arguments)]
fn render_profile_range(
    stencils: &[Vec<Option<CellStencil>>],
    profiles: &[PreparedProfile],
    window: &RasterWindow<'_>,
    tolerance: f32,
    pool: &rayon::ThreadPool,
    cache: Option<&PixelWeights>,
    row_start: usize,
    row_count: usize,
) -> Result<(Vec<f32>, Vec<f32>), MapError> {
    window.validate_native_grid()?;
    if window.native_grid.is_some() && profiles.iter().any(|p| p.options != Options::default()) {
        return Err(MapError::Unsupported(
            "stored grid sampling requires plain corner profiles".into(),
        ));
    }
    let count = window
        .width
        .checked_mul(window.height)
        .filter(|&n| n > 0)
        .ok_or_else(|| MapError::Invalid("empty sloping raster window".into()))?;
    if row_count == 0
        || row_start
            .checked_add(row_count)
            .is_none_or(|end| end > window.height)
    {
        return Err(MapError::Invalid("sloping row range out of bounds".into()));
    }
    let start = row_start * window.width;
    let output_count = row_count * window.width;
    if window.owner.len() != count
        || window.terrain.len() != count
        || stencils.len() != profiles.len()
        || window.transform[2] != 0.0
        || window.transform[4] != 0.0
        || window.transform.iter().any(|v| !v.is_finite())
        || !tolerance.is_finite()
        || tolerance < 0.0
    {
        return Err(MapError::Invalid(
            "sloping raster inputs do not match".into(),
        ));
    }
    for &[mesh, cell] in window.owner_cells {
        if mesh >= stencils.len()
            || cell >= stencils[mesh].len()
            || cell >= profiles[mesh].local_values.len()
        {
            return Err(MapError::Invalid(
                "sloping raster owner-cell address out of range".into(),
            ));
        }
    }
    if window.owner[start..start + output_count]
        .iter()
        .any(|&i| i as usize > window.owner_cells.len())
    {
        return Err(MapError::Invalid("sloping owner label out of range".into()));
    }
    let mut depth = vec![NODATA; output_count];
    let mut wse = vec![NODATA; output_count];
    let repair: Vec<_> = window
        .owner_cells
        .iter()
        .map(|_| AtomicBool::new(false))
        .collect();
    pool.install(|| {
        depth
            .par_chunks_mut(4096)
            .zip(wse.par_chunks_mut(4096))
            .enumerate()
            .try_for_each_init(
                || WeightScratch::new(0),
                |scratch, (chunk, (d, w))| {
                    for j in 0..d.len() {
                        let i = start + chunk * 4096 + j;
                        let label = window.owner[i] as usize;
                        if label == 0 {
                            continue;
                        }
                        let [mesh, cell] = window.owner_cells[label - 1];
                        if !profiles[mesh].wet[cell] {
                            continue;
                        }
                        if let Some(value) = profiles[mesh].horizontal[cell] {
                            if let Some([delta, value]) =
                                deliver_pixel(value, window.terrain[i], tolerance)
                            {
                                d[j] = delta;
                                w[j] = value;
                            }
                            continue;
                        }
                        let Some(stencil) = &stencils[mesh][cell] else {
                            continue;
                        };
                        let values = &profiles[mesh].local_values[cell];
                        scratch.resize(values.len());
                        let weights = if let Some(weights) = cache
                            .filter(|_| profiles[mesh].options.render_mode != RenderMode::WithFaces)
                            .and_then(|c| c.get(i, values.len()))
                        {
                            weights
                        } else {
                            stencil.window_weights(window, i, scratch)?
                        };
                        let value = if profiles[mesh].options.render_mode == RenderMode::WithFaces {
                            // End the borrowed weights view before mutating scratch.
                            let _ = weights;
                            if scratch.clipped
                                && (0..values.len()).any(|k| {
                                    scratch.weights[(k + values.len() - 1) % values.len()]
                                        + scratch.weights[(k + 1) % values.len()]
                                        == 0.0
                                })
                            {
                                repair[label - 1].store(true, Ordering::Relaxed);
                            }
                            scratch.paint_with_faces(
                                values,
                                &profiles[mesh].face_values[cell],
                                profiles[mesh]
                                    .options
                                    .use_depth_weighted_faces
                                    .then(|| profiles[mesh].depth_weights[cell].as_slice()),
                            )
                        } else {
                            paint_corner_values(weights, values)?
                        };
                        if let Some([delta, value]) =
                            deliver_pixel(value, window.terrain[i], tolerance)
                        {
                            d[j] = delta;
                            w[j] = value;
                        }
                    }
                    Ok::<_, MapError>(())
                },
            )
    })?;
    // Native geometry donation retains scratch inside fixed 256-point ranges.
    // Replay only cells where clipped weights can expose that state. Replaying
    // complete source tiles keeps bbox/polygon outputs equal to full-map crops.
    let repaired = pool.install(|| {
        window
            .owner_cells
            .par_iter()
            .enumerate()
            .filter_map(|(label, &[mesh, cell])| {
                repair[label]
                    .load(Ordering::Relaxed)
                    .then_some((label + 1, mesh, cell))
            })
            .map(
                |(label, mesh, cell)| -> Result<Vec<(usize, [f32; 2])>, MapError> {
                    let stencil = stencils[mesh][cell].as_ref().unwrap();
                    if stencil.physical_ring.is_empty() {
                        return Err(MapError::Unsupported(
                            "native face donation replay needs full cell perimeter geometry".into(),
                        ));
                    }
                    let [tw, th] = window.source_tile;
                    if tw == 0 || th == 0 || tw.checked_mul(th).is_none_or(|n| n > 16_777_216) {
                        return Err(MapError::Unsupported(
                            "unsupported native donation tile size".into(),
                        ));
                    }
                    let gt = window.source_transform;
                    let xoff = ((window.transform[0] - gt[0]) / gt[1]).round() as i64;
                    let yoff = ((window.transform[3] - gt[3]) / gt[5]).round() as i64;
                    let minx = stencil
                        .physical_ring
                        .iter()
                        .map(|p| p[0])
                        .fold(f64::INFINITY, f64::min);
                    let maxx = stencil
                        .physical_ring
                        .iter()
                        .map(|p| p[0])
                        .fold(f64::NEG_INFINITY, f64::max);
                    let miny = stencil
                        .physical_ring
                        .iter()
                        .map(|p| p[1])
                        .fold(f64::INFINITY, f64::min);
                    let maxy = stencil
                        .physical_ring
                        .iter()
                        .map(|p| p[1])
                        .fold(f64::NEG_INFINITY, f64::max);
                    let c0 = (((minx - gt[0]) / gt[1]).floor() as i64).max(xoff).max(0);
                    let c1 =
                        (((maxx - gt[0]) / gt[1]).ceil() as i64).min(xoff + window.width as i64);
                    let r0 = (((maxy - gt[3]) / gt[5]).floor() as i64).max(yoff).max(0);
                    let r1 =
                        (((miny - gt[3]) / gt[5]).ceil() as i64).min(yoff + window.height as i64);
                    let mut output = Vec::new();
                    if c0 >= c1 || r0 >= r1 {
                        return Ok(output);
                    }
                    for ty in r0 as usize / th..=(r1 as usize - 1) / th {
                        for tx in c0 as usize / tw..=(c1 as usize - 1) / tw {
                            let tile_gt = [
                                gt[0] + (tx * tw) as f64 * gt[1],
                                gt[1],
                                0.,
                                gt[3] + (ty * th) as f64 * gt[5],
                                0.,
                                gt[5],
                            ];
                            let owned = crate::ownership::burn_ring(
                                &stencil.physical_ring,
                                terrio::ops::rasterize::Grid {
                                    geo_transform: tile_gt,
                                    cols: tw,
                                    rows: th,
                                    crs: Default::default(),
                                },
                            )?;
                            let values = &profiles[mesh].local_values[cell];
                            let faces = &profiles[mesh].face_values[cell];
                            let depths = profiles[mesh]
                                .options
                                .use_depth_weighted_faces
                                .then(|| profiles[mesh].depth_weights[cell].as_slice());
                            let mut scratch = WeightScratch::new(values.len());
                            let mut native_index = 0;
                            for (i, &inside) in owned.iter().enumerate() {
                                if inside == 0 {
                                    continue;
                                }
                                if native_index % 256 == 0 {
                                    scratch.clockwise.fill(0.);
                                    scratch.counterclockwise.fill(0.);
                                }
                                native_index += 1;
                                let point = [
                                    tile_gt[0] + (i % tw) as f64 * gt[1] + 0.5 * gt[1],
                                    tile_gt[3] + (i / tw) as f64 * gt[5] + 0.5 * gt[5],
                                ];
                                stencil.polygon.weights(point, &mut scratch)?;
                                let value =
                                    scratch.paint_with_face_history(values, faces, depths, true);
                                let col = (tx * tw + i % tw) as i64 - xoff;
                                let row = (ty * th + i / tw) as i64 - yoff;
                                if col < 0
                                    || row < 0
                                    || col >= window.width as i64
                                    || row >= window.height as i64
                                {
                                    continue;
                                }
                                let index = row as usize * window.width + col as usize;
                                if window.owner[index] as usize != label {
                                    continue;
                                }
                                output.push((
                                    index,
                                    deliver_pixel(value, window.terrain[index], tolerance)
                                        .unwrap_or([NODATA, NODATA]),
                                ));
                            }
                        }
                    }
                    Ok(output)
                },
            )
            .collect::<Result<Vec<_>, MapError>>()
    })?;
    for cell in repaired {
        for (index, [d, w]) in cell {
            if (start..start + output_count).contains(&index) {
                depth[index - start] = d;
                wse[index - start] = w;
            }
        }
    }
    Ok((depth, wse))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_adjacent_donation_keeps_native_batch_history() {
        let corners = [1., 2., 3., 4., 5., 6.];
        let faces = [2., 3., 4., 5., 6., 7.];
        let mut carried = WeightScratch::new(6);
        carried
            .weights
            .copy_from_slice(&[0.1, 0.2, 0.1, 0.2, 0.1, 0.3]);
        carried.paint_with_face_history(&corners, &faces, None, true);
        let previous = carried.clockwise[3];
        assert_ne!(previous, 0.);
        carried.weights.copy_from_slice(&[1., 0., 0., 0., 0., 0.]);
        let value = carried.paint_with_face_history(&corners, &faces, None, true);
        assert_eq!(carried.clockwise[3].to_bits(), previous.to_bits());
        let mut fresh = WeightScratch::new(6);
        fresh.weights.copy_from_slice(&[1., 0., 0., 0., 0., 0.]);
        assert_ne!(
            value.to_bits(),
            fresh.paint_with_faces(&corners, &faces, None).to_bits()
        );
    }

    #[test]
    fn general_polygon_donation_borrows_reused_scratch() {
        let points = vec![[0., 0.], [0., 4.], [2., 5.], [5., 3.], [4., 0.]];
        let polygon = PolygonStencil::new(points.clone()).unwrap();
        let stencil = CellStencil {
            square_center: None,
            square: None,
            polygon,
            physical_ring: points,
            corners: vec![[0, 0]; 5],
            faces: vec![[0, 0]; 5],
            elevations: None,
        };
        let mut scratch = WeightScratch::new(5);
        let allocation = scratch.donated.as_ptr();
        for point in [[1., 1.], [2., 3.], [4., 2.], [1., 1.]] {
            let borrowed = stencil.donated_weights(point, &mut scratch).unwrap();
            assert_eq!(borrowed.as_ptr(), allocation);
            assert_eq!(borrowed.len(), 10);
            assert!(borrowed.iter().all(|x| x.is_finite()));
        }
    }

    #[test]
    #[ignore = "manual release-mode borrowed versus former copied-weight microbenchmark"]
    fn benchmark_borrowed_general_donation() {
        let points = vec![[0., 0.], [0., 4.], [2., 5.], [5., 3.], [4., 0.]];
        let polygon = PolygonStencil::new(points.clone()).unwrap();
        let stencil = CellStencil {
            square_center: None,
            square: None,
            polygon,
            physical_ring: points,
            corners: vec![[0, 0]; 5],
            faces: vec![[0, 0]; 5],
            elevations: None,
        };
        for round in 0..6 {
            let mut result = [(0.0, 0); 2];
            for copied in if round % 2 == 0 {
                [true, false]
            } else {
                [false, true]
            } {
                let mut scratch = WeightScratch::new(5);
                let mut checksum = 0_u64;
                let start = std::time::Instant::now();
                for i in 0..250_000 {
                    let point = [1. + (i % 17) as f64 * 0.05, 1. + (i % 13) as f64 * 0.07];
                    let weights = stencil.donated_weights(point, &mut scratch).unwrap();
                    let bits = if copied {
                        // The former return expression allocated this identical copy.
                        let old = std::hint::black_box(weights.to_vec());
                        old[0].to_bits()
                    } else {
                        std::hint::black_box(weights)[0].to_bits()
                    };
                    checksum = checksum.wrapping_add(u64::from(bits));
                }
                result[usize::from(!copied)] = (start.elapsed().as_secs_f64(), checksum);
            }
            assert_eq!(result[0].1, result[1].1);
            println!(
                "round={round} pixels=250000 copied_seconds={} borrowed_seconds={}",
                result[0].0, result[1].0
            );
        }
    }

    #[test]
    fn native_davis_donated_and_depth_rebalanced_scalar_bits() {
        // Independent reflection of native 7.0.1 private scalar painters on
        // public Davis corner/face values. Supplied depth coefficients isolate
        // normalization; tiny negative donated weights must not be clamped.
        let weights = [0x3e648a40, 0x3eda93c1, 0x3e74091d, 0x3de48a40].map(f32::from_bits);
        let corners = [0x42452a70, 0x4244fae3, 0x4242a6f2, 0x424466fa].map(f32::from_bits);
        let faces = [0x4244fae3, 0x4244fae3, 0x4243c5bf, 0x42453384].map(f32::from_bits);
        let depths = [
            0x3c23d70a, 0x3fa00000, 0x418b0000, 0x40490fd0, 0x3c23d70a, 0x3fa00000, 0x418b0000,
            0x40490fd0,
        ]
        .map(f32::from_bits);
        let mut scratch = WeightScratch::new(4);
        scratch.weights.copy_from_slice(&weights);
        assert_eq!(
            scratch.paint_with_faces(&corners, &faces, None).to_bits(),
            1111809851
        );
        let expected_donated = [
            0xb2000000, 0x3d7a0954, 0xb2800000, 0x3c82b774, 0x3eb52e2d, 0x3ec17700, 0x3dca4878,
            0x3dbd704e,
        ];
        assert_eq!(
            scratch
                .donated
                .iter()
                .map(|v| v.to_bits())
                .collect::<Vec<_>>(),
            expected_donated
        );
        assert_eq!(
            scratch
                .paint_with_faces(&corners, &faces, Some(&depths))
                .to_bits(),
            1111765762
        );
    }

    #[test]
    fn davis_square_pixel_preserves_native_rounding() {
        // Real Davis p02 cell146 and a pixel that differed with general weights.
        let polygon = PolygonStencil::new(vec![
            [6630441.54307014, 1966773.76305611],
            [6630441.54307014, 1966623.76305611],
            [6630591.54307014, 1966623.76305611],
            [6630591.54307014, 1966773.76305611],
        ])
        .unwrap();
        let mut scratch = WeightScratch::new(4);
        let point = [6630507.763170041, 1966711.399494069];
        let values = [0x423e156c, 0x423b1da6, 0x423a9ca2, 0x423dcb4e].map(f32::from_bits);
        let weights = polygon.square_weights(polygon.square_bounds().unwrap(), point, &mut scratch);
        assert_eq!(
            paint_corner_values(weights, &values).unwrap().to_bits(),
            0x423caebe
        );
        let general = polygon.weights(point, &mut scratch).unwrap();
        assert_ne!(
            paint_corner_values(general, &values).unwrap().to_bits(),
            0x423caebe
        );
    }

    #[test]
    fn bounded_pixel_weights_preserve_square_bits_and_fall_back_outside_prefix() {
        let polygon = PolygonStencil::new(vec![[0., 0.], [0., 2.], [2., 2.], [2., 0.]]).unwrap();
        let square = polygon.square_bounds();
        let stencil = CellStencil {
            square_center: None,
            physical_ring: polygon.points.clone(),
            polygon,
            square,
            corners: vec![],
            faces: vec![],
            elevations: None,
        };
        let stencils = vec![vec![Some(stencil)]];
        let window = RasterWindow {
            native_grid: None,
            source_transform: [0., 1., 0., 2., 0., -1.],
            source_tile: [2, 2],
            width: 2,
            height: 2,
            transform: [0., 1., 0., 2., 0., -1.],
            owner: &[1, 1, 1, 1],
            terrain: &[0.; 4],
            owner_cells: &[[0, 0]],
        };
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        let weights = PixelWeights::prepare(&stencils, &window, 32, &pool).unwrap();
        assert_eq!(weights.bytes(), 32);
        let stencil = stencils[0][0].as_ref().unwrap();
        let mut scratch = WeightScratch::new(4);
        for i in 0..2 {
            let point = [i as f64 + 0.5, 1.5];
            let expected = stencil
                .polygon
                .square_weights(square.unwrap(), point, &mut scratch);
            assert_eq!(
                weights
                    .get(i, 4)
                    .unwrap()
                    .iter()
                    .map(|v| v.to_bits())
                    .collect::<Vec<_>>(),
                expected.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
            );
        }
        assert!(weights.get(2, 4).is_none());
        let empty = PixelWeights::prepare(&stencils, &window, 0, &pool).unwrap();
        assert_eq!(empty.bytes(), 0);
        assert!(empty.get(0, 4).is_none());
    }

    #[test]
    fn davis_endpoint_intersection_uses_polyline_midpoint() {
        let point = face_application(
            &[
                [6628997.072593668, 1968668.2335325822],
                [6629091.54307014, 1968573.76305611],
            ],
            [
                [6629166.54307014, 1968648.76305611],
                [6629016.54307014, 1968498.76305611],
            ],
        )
        .unwrap();
        assert_eq!(point, [6629044.307831904, 1968620.998294346]);
    }

    #[test]
    fn native_plot_tolerance_is_not_an_extra_depth_cutoff() {
        assert!(deliver_pixel(10.0005, 10.0, 0.001).is_none());
        assert!(deliver_pixel(10.01, 10.0, 0.001).is_some());
        // Native f32 terrain+tolerance may round to terrain at large elevations.
        assert_eq!(
            deliver_pixel(100000.0, 100000.0, 0.001),
            Some([0.0, 100000.0])
        );
        assert!(deliver_pixel(NODATA, 10.0, 0.001).is_none());
    }

    #[test]
    fn invalid_face_geometry_is_rejected() {
        assert!(face_application(&[[0.0, f64::NAN], [1.0, 1.0]], [[0.0; 2]; 2]).is_err());
        assert!(face_application(&[[0.0; 2]], [[0.0; 2]; 2]).is_err());
    }
}

#[cfg(test)]
mod boundary_surface_tests {
    use super::*;
    #[test]
    fn stored_grid_seeded_squares_cache_rows_and_crops_keep_identical_bits() {
        let layout = crate::stored_grid::Layout::new(
            513,
            257,
            [1_000_000.125, 0.5, 0., 2_000_000.25, 0., -0.5],
        )
        .unwrap();
        let left = layout.source[0];
        let top = layout.source[3];
        let points = vec![
            [left, top - 512.],
            [left, top],
            [left + 512., top],
            [left + 512., top - 512.],
        ];
        let polygon = PolygonStencil::new(points.clone()).unwrap();
        let stencils = vec![vec![Some(CellStencil {
            square_center: Some([left + 256., top - 256.]),
            physical_ring: points,
            square: polygon.square_bounds(),
            polygon,
            corners: vec![],
            faces: vec![],
            elevations: None,
        })]];
        let profiles = vec![PreparedProfile {
            local_values: vec![vec![200., 201., 203., 197.]],
            face_values: vec![vec![]],
            depth_weights: vec![vec![]],
            horizontal: vec![None],
            wet: vec![true],
            options: Options::default(),
            corner_bound: None,
            input_max: Vec::new(),
        }];
        let owners = vec![1; 513 * 257];
        let ground: Vec<_> = (0..owners.len())
            .map(|i| if i % 43 == 0 { NODATA } else { 0. })
            .collect();
        let full = RasterWindow {
            native_grid: Some((layout, [0, 0])),
            source_transform: layout.source,
            source_tile: [256, 256],
            width: 513,
            height: 257,
            transform: layout.output,
            owner: &owners,
            terrain: &ground,
            owner_cells: &[[0, 0]],
        };
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(4)
            .build()
            .unwrap();
        let mut invalid = RasterWindow::new(
            layout.source,
            [256, 256],
            [513, 257],
            layout.output,
            &owners,
            &ground,
            &[[0, 0]],
        )
        .with_stored_grid(layout, [0, 0])
        .unwrap();
        invalid.native_grid = Some((layout, [usize::MAX, 0]));
        assert!(render_profile(&stencils, &profiles, &invalid, 0.001, &pool).is_err());
        assert!(PixelWeights::prepare(&stencils, &invalid, 4096, &pool).is_err());
        let expected = render_profile(&stencils, &profiles, &full, 0.001, &pool).unwrap();
        for bytes in [0, 4096, owners.len() * 16] {
            let cache = PixelWeights::prepare(&stencils, &full, bytes, &pool).unwrap();
            let got =
                render_profile_cached(&stencils, &profiles, &full, 0.001, &pool, Some(&cache))
                    .unwrap();
            assert_eq!(
                expected.0.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                got.0.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
            );
            assert_eq!(
                expected.1.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                got.1.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
            );
        }
        let part = render_profile_rows(&stencils, &profiles, &full, 0.001, &pool, 255, 2).unwrap();
        assert_eq!(
            expected.1[255 * 513..]
                .iter()
                .map(|v| v.to_bits())
                .collect::<Vec<_>>(),
            part.1.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
        );
        let mut terrain = Vec::new();
        for r in 255..257 {
            terrain.extend_from_slice(&ground[r * 513 + 255..r * 513 + 258]);
        }
        let crop = RasterWindow {
            native_grid: Some((layout, [255, 255])),
            source_transform: layout.source,
            source_tile: [256, 256],
            width: 3,
            height: 2,
            transform: layout.window_transform([255, 255, 3, 2]),
            owner: &[1; 6],
            terrain: &terrain,
            owner_cells: &[[0, 0]],
        };
        let actual = render_profile(&stencils, &profiles, &crop, 0.001, &pool).unwrap();
        for r in 0..2 {
            for c in 0..3 {
                assert_eq!(
                    expected.1[(r + 255) * 513 + c + 255].to_bits(),
                    actual.1[r * 3 + c].to_bits()
                );
            }
        }
    }
    fn fixture() -> (Vec<Vec<Option<CellStencil>>>, Vec<PreparedProfile>) {
        let points = vec![[0., 0.], [1., 0.], [1., 1.], [0., 1.]];
        let polygon = PolygonStencil::new(points.clone()).unwrap();
        let stencil = CellStencil {
            square_center: None,
            physical_ring: points,
            polygon,
            square: Some([0., 0., 1., 1.]),
            corners: vec![[0, 0]; 4],
            faces: vec![],
            elevations: None,
        };
        let profile = PreparedProfile {
            local_values: vec![vec![2.; 4]],
            face_values: vec![vec![]],
            depth_weights: vec![vec![]],
            horizontal: vec![None],
            wet: vec![true],
            options: Options::default(),
            corner_bound: None,
            input_max: Vec::new(),
        };
        (vec![vec![Some(stencil)]], vec![profile])
    }
    #[test]
    fn streamed_rows_are_full_window_crops_without_affine_reassociation() {
        let (mut stencils, mut profiles) = fixture();
        let transform = [
            10_000_000.1234567,
            0.0037,
            0.,
            2_000_000.7654321,
            0.,
            -0.0037,
        ];
        let (width, height) = (17, 19);
        let bounds = [
            transform[0],
            transform[3] + height as f64 * transform[5],
            transform[0] + width as f64 * transform[1],
            transform[3],
        ];
        let points = vec![
            [bounds[0], bounds[1]],
            [bounds[0], bounds[3]],
            [bounds[2], bounds[3]],
            [bounds[2], bounds[1]],
        ];
        let polygon = PolygonStencil::new(points.clone()).unwrap();
        stencils[0][0] = Some(CellStencil {
            square_center: None,
            square: polygon.square_bounds(),
            polygon,
            physical_ring: points,
            corners: vec![[0, 0]; 4],
            faces: vec![],
            elevations: None,
        });
        profiles[0].local_values[0] = vec![2., 3., 4., 5.];
        let owner: Vec<u32> = (0..width * height)
            .map(|i| u32::from(i % 11 != 0))
            .collect();
        let terrain: Vec<f32> = (0..width * height)
            .map(|i| if i % 7 == 0 { 4.5 } else { 0. })
            .collect();
        let window = RasterWindow {
            native_grid: None,
            source_transform: transform,
            source_tile: [256, 256],
            width,
            height,
            transform,
            owner: &owner,
            terrain: &terrain,
            owner_cells: &[[0, 0]],
        };
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(3)
            .build()
            .unwrap();
        let full = render_profile(&stencils, &profiles, &window, 0.001, &pool).unwrap();
        for (row, rows) in [(0, 1), (7, 5), (18, 1)] {
            let part = render_profile_rows(&stencils, &profiles, &window, 0.001, &pool, row, rows)
                .unwrap();
            for (expected, actual) in [(&full.0, &part.0), (&full.1, &part.1)] {
                assert_eq!(
                    expected[row * width..(row + rows) * width]
                        .iter()
                        .map(|v| v.to_bits())
                        .collect::<Vec<_>>(),
                    actual.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
                );
            }
        }
        assert!(render_profile_rows(&stencils, &profiles, &window, 0.001, &pool, 18, 2).is_err());
        profiles[0].options.render_mode = RenderMode::WithFaces;
        assert!(render_profile_rows(&stencils, &profiles, &window, 0.001, &pool, 0, 1).is_err());
    }
    #[test]
    fn signed_boundary_surface_preserves_dry_side_without_changing_raster_visibility() {
        let (stencils, profiles) = fixture();
        let mut scratch = WeightScratch::new(0);
        let surface = boundary_surface_value(&stencils, &profiles, 0, 0, [0.5, 0.5], &mut scratch)
            .unwrap()
            .unwrap();
        assert_eq!(surface.to_bits(), 2f32.to_bits());
        assert_eq!(Some(surface - 10001.), Some(-9999.));
        assert!(deliver_pixel(surface, 10001., 0.001).is_none());
    }
    #[test]
    fn boundary_surface_rejects_faces_and_retains_explicit_missing_cells() {
        let (stencils, mut profiles) = fixture();
        let mut scratch = WeightScratch::new(0);
        profiles[0].wet[0] = false;
        assert_eq!(
            boundary_surface_value(&stencils, &profiles, 0, 0, [0.5, 0.5], &mut scratch).unwrap(),
            None
        );
        profiles[0].options.render_mode = RenderMode::WithFaces;
        assert!(matches!(
            boundary_surface_value(&stencils, &profiles, 0, 0, [0.5, 0.5], &mut scratch),
            Err(MapError::Unsupported(_))
        ));
    }
}

#[cfg(test)]
// The recorded binary32 values are written out exactly, as in the evidence.
#[allow(clippy::excessive_precision)]
mod bounded_corner_tests {
    //! `rasmapper-plain-sloping-bounded-corners/1` on real corner rings. The
    //! rings, face values and cell states are the UPGU1 (HEC-RAS 6.6) values at
    //! the named vertices, extracted from the p03 slim HDF with the replay port
    //! P03-DEPTH validated against native RAS Mapper; local indices replace ids.
    use super::*;

    type RingFace = ([usize; 2], [usize; 2], [f32; 2], Connection, [f64; 2]);

    /// One vertex (local 0) with its ring in counter-clockwise order.
    fn ring(point: [f64; 2], ring: &[RingFace]) -> (Corner, Vec<Face>, Vec<FaceWater>) {
        let n = ring.len();
        let faces = ring
            .iter()
            .map(|&(cells, vertices, _, _, application)| Face {
                cells,
                vertices,
                minimum: 0.0,
                perimeter: false,
                application,
            })
            .collect();
        let values = ring
            .iter()
            .map(|&(_, _, values, connection, _)| FaceWater { values, connection })
            .collect();
        let corner = Corner {
            point,
            faces: (0..n).collect(),
            ccw: (0..n).map(|j| (j + 1) % n).collect(),
            cw: (0..n).map(|j| (j + n - 1) % n).collect(),
        };
        (corner, faces, values)
    }

    fn cells(states: &[(f32, f32)]) -> (Vec<Cell>, Vec<f32>) {
        (
            states
                .iter()
                .map(|&(_, minimum)| Cell {
                    minimum,
                    virtual_cell: false,
                })
                .collect(),
            states.iter().map(|&(wse, _)| wse).collect(),
        )
    }

    fn bounded(tolerance: f32) -> CornerFitBound {
        CornerFitBound {
            mode: CornerFitBoundMode::InputRange,
            tolerance,
        }
    }

    use Connection::{DownhillDeep, DownhillShallow, Levee, None as Unconnected};

    /// wb-2417240, p03 minute 468 (01JAN2020 07:48), vertex 52923 at
    /// (1867521.7602708347, 13975846.4679681) EPSG:2278. Ring faces 103626,
    /// 103621, 103624, 880557, 110505, 108198; cells 37464, 37830, 385790,
    /// 385791, 384019, 384018.
    fn wb_2417240() -> (Corner, Vec<Face>, Vec<FaceWater>, Vec<Cell>, Vec<f32>) {
        let (corner, faces, values) = ring(
            [1867521.7602708347, 13975846.4679681],
            &[
                (
                    [2, 0],
                    [0, 1],
                    [2014.0091552734375; 2],
                    DownhillShallow,
                    [1867536.2658785027, 13975898.04001275],
                ),
                (
                    [3, 2],
                    [0, 2],
                    [1992.9306640625, 1994.413818359375],
                    Unconnected,
                    [1867421.2346157592, 13975971.709476482],
                ),
                (
                    [3, 4],
                    [3, 0],
                    [2011.8123779296875; 2],
                    DownhillShallow,
                    [1867466.150882308, 13975827.636414781],
                ),
                (
                    [4, 5],
                    [4, 0],
                    [2014.575439453125; 2],
                    DownhillShallow,
                    [1867496.9244559794, 13975673.47924017],
                ),
                (
                    [1, 5],
                    [0, 5],
                    [NODATA, 2014.828857421875],
                    Unconnected,
                    [1867558.4931723755, 13975780.56591139],
                ),
                (
                    [0, 1],
                    [0, 6],
                    [2014.02734375, NODATA],
                    Unconnected,
                    [1867657.10061502, 13975846.4679681],
                ),
            ],
        );
        let (cells, wse) = cells(&[
            (2014.02734375, 2014.0091552734375),
            (2015.7213134765625, 2015.7213134765625),
            (1994.413818359375, 1994.41259765625),
            (1992.9306640625, 1992.92919921875),
            (2012.0435791015625, 2011.8109130859375),
            (2014.828857421875, 2014.5625),
        ]);
        (corner, faces, values, cells, wse)
    }

    /// p03 summary Max profile, vertex 136296 at (1877780.1158722613,
    /// 13949789.469003635). Ring faces 278396, 871705, 871704, 278398, 278390;
    /// cells 101015, 364725, 364724, 366777, 366778.
    fn max_136296() -> (Corner, Vec<Face>, Vec<FaceWater>, Vec<Cell>, Vec<f32>) {
        let (corner, faces, values) = ring(
            [1877780.1158722613, 13949789.469003635],
            &[
                (
                    [0, 1],
                    [1, 0],
                    [1759.1474609375; 2],
                    DownhillShallow,
                    [1877760.6277300008, 13949758.802121554],
                ),
                (
                    [2, 1],
                    [0, 2],
                    [1760.1295166015625, 1759.6790771484375],
                    Levee,
                    [1877831.0975270872, 13949673.572002241],
                ),
                (
                    [2, 3],
                    [3, 0],
                    [1759.6458740234375; 2],
                    DownhillShallow,
                    [1877825.4736998752, 13949795.422545616],
                ),
                (
                    [4, 3],
                    [0, 4],
                    [1752.5445556640625; 2],
                    DownhillDeep,
                    [1877799.2271683163, 13949914.22497657],
                ),
                (
                    [0, 4],
                    [0, 5],
                    [1752.2613525390625, 1752.3653564453125],
                    Unconnected,
                    [1877702.0107853098, 13949830.976303],
                ),
            ],
        );
        let (cells, wse) = cells(&[
            (1752.2613525390625, 1751.3450927734375),
            (1759.6790771484375, 1759.0789794921875),
            (1760.1295166015625, 1759.572021484375),
            (1752.5445556640625, 1734.0),
            (1752.3653564453125, 1733.9761962890625),
        ]);
        (corner, faces, values, cells, wse)
    }

    fn run(
        fixture: &(Corner, Vec<Face>, Vec<FaceWater>, Vec<Cell>, Vec<f32>),
        bound: Option<CornerFitBound>,
    ) -> (Vec<f32>, Vec<f32>, CornerBoundReport) {
        let (corner, faces, values, cells, wse) = fixture;
        let context = bound.map(|bound| BoundContext {
            bound,
            cells,
            wse,
            plot_tolerance: 0.001,
        });
        reconstruct_corner(0, corner, faces, values, context.as_ref())
    }

    fn bits(values: &[f32]) -> Vec<u32> {
        values.iter().map(|v| v.to_bits()).collect()
    }

    #[test]
    fn wb_2417240_minute_468_corner_is_clamped_to_its_highest_input() {
        let fixture = wb_2417240();
        let (native, maxima, report) = run(&fixture, None);
        // Native (and RAS Mapper 6.6): 46.0 ft above the highest input.
        let spike = 2060.02587890625_f32;
        let inside = 2009.2427978515625_f32;
        assert_eq!(
            bits(&native),
            bits(&[spike, inside, inside, inside, NODATA, spike])
        );
        assert!(maxima.is_empty());
        assert_eq!(report, CornerBoundReport::default());

        let (zero, maxima, report) = run(&fixture, Some(bounded(0.0)));
        let top = 2014.02734375_f32;
        assert_eq!(
            bits(&zero),
            bits(&[top, inside, inside, inside, NODATA, top])
        );
        assert_eq!(report.groups_evaluated, 2);
        assert_eq!(report.bounded_up, 1);
        assert_eq!(report.bounded_down + report.bounded_nonfinite, 0);
        assert_eq!(report.bounded_corner_groups(), 1);
        assert_eq!(
            report.max_removed_overshoot,
            f64::from(spike) - f64::from(top)
        );
        // First wetting: the shallowest wet input cell is 0.0012 ft deep.
        assert_eq!(report.bounded_min_wet_input_depth_lt_0p1, 1);
        assert_eq!(report.bounded_l_ge_3, 1);
        // Same-profile input maximum of both fits: cell 37830, dry, 2015.72.
        let high = 2015.7213134765625_f32;
        assert_eq!(
            bits(&maxima),
            bits(&[high, high, high, high, f32::NEG_INFINITY, high])
        );

        let (half, _, _) = run(&fixture, Some(bounded(0.5)));
        let lifted = 2014.52734375_f32;
        assert_eq!(
            bits(&half),
            bits(&[lifted, inside, inside, inside, NODATA, lifted])
        );

        let (wide, _, report) = run(&fixture, Some(bounded(f32::MAX)));
        assert_eq!(bits(&wide), bits(&native));
        assert_eq!(report.bounded_corner_groups(), 0);
        assert_eq!(report.groups_evaluated, 2);
    }

    #[test]
    fn max_profile_vertex_136296_downward_fit_is_clamped_to_its_lowest_input() {
        let fixture = max_136296();
        let (native, _, _) = run(&fixture, None);
        let hole = 1486.1566162109375_f32; // 266.1 ft below every input
        let inside = 1756.191162109375_f32;
        assert_eq!(bits(&native), bits(&[hole, inside, inside, inside, hole]));
        let (zero, _, report) = run(&fixture, Some(bounded(0.0)));
        let low = 1752.2613525390625_f32;
        assert_eq!(bits(&zero), bits(&[low, inside, inside, inside, low]));
        assert_eq!(report.bounded_down, 1);
        assert_eq!(report.bounded_up + report.bounded_nonfinite, 0);
        assert_eq!(
            report.max_removed_overshoot,
            f64::from(low) - f64::from(hole)
        );
        // Not first wetting: the shallowest wet input cell is 0.56 ft deep.
        assert_eq!(report.bounded_min_wet_input_depth_lt_0p1, 0);
        let (half, _, _) = run(&fixture, Some(bounded(0.5)));
        assert_eq!(half[0].to_bits(), 1751.7613525390625_f32.to_bits());
    }

    #[test]
    fn non_finite_fit_becomes_the_mean_of_its_inputs() {
        let origin = [0.0, 0.0];
        let points = [[1.0, 0.0], [0.0, 1.0], [-1.0, 0.25]];
        let values = [10.0_f32, 11.0, 13.0];
        let mut regression = Regression::default();
        let mut inputs = Vec::new();
        for (point, value) in points.iter().zip(values) {
            regression.add(origin, *point, value);
            inputs.push((relative(origin, *point), value, 0));
        }
        let faces = vec![Face {
            cells: [0, 0],
            vertices: [0, 1],
            minimum: 0.0,
            perimeter: false,
            application: [0.0, 0.0],
        }];
        let cells = [Cell {
            minimum: 9.0,
            virtual_cell: false,
        }];
        let wse = [12.0_f32];
        let context = BoundContext {
            bound: bounded(0.0),
            cells: &cells,
            wse: &wse,
            plot_tolerance: 0.001,
        };
        let mean = ((10.0_f64 + 11.0 + 13.0) / 3.0) as f32;
        for fit in [f32::INFINITY, f32::NEG_INFINITY, f32::NAN] {
            let mut report = CornerBoundReport::default();
            let value = context.apply(fit, &regression, &inputs, &faces, &mut report);
            assert_eq!(value.to_bits(), mean.to_bits());
            assert_eq!(report.bounded_nonfinite, 1);
            assert_eq!(report.max_removed_overshoot, 0.0);
        }
        // An in-range fit keeps its exact bits.
        let mut report = CornerBoundReport::default();
        let value = context.apply(12.5, &regression, &inputs, &faces, &mut report);
        assert_eq!(value.to_bits(), 12.5_f32.to_bits());
        assert_eq!(report.bounded_corner_groups(), 0);
        assert_eq!(report.groups_evaluated, 1);
        // Fewer than three inputs are never tested.
        let mut report = CornerBoundReport::default();
        let value = context.apply(f32::NAN, &regression, &inputs[..2], &faces, &mut report);
        assert!(value.is_nan());
        assert_eq!(report, CornerBoundReport::default());
    }

    #[test]
    fn tolerance_and_mode_are_validated() {
        for tolerance in [-0.01, -f32::MAX, f32::INFINITY, f32::NAN] {
            assert!(bounded(tolerance).validate().is_err(), "{tolerance}");
            let options = Options {
                corner_fit_bound: Some(bounded(tolerance)),
                ..Options::default()
            };
            assert!(matches!(options.validate(), Err(MapError::Invalid(_))));
        }
        for tolerance in [0.0, -0.0, 0.5, f32::MAX] {
            assert!(bounded(tolerance).validate().is_ok(), "{tolerance}");
        }
        let pretty = Options {
            render_mode: RenderMode::WithFaces,
            corner_fit_bound: Some(bounded(0.0)),
            ..Options::default()
        };
        assert!(
            matches!(pretty.validate(), Err(MapError::Unsupported(m)) if m.contains(BOUNDED_WITH_FACES_METHOD_ID))
        );
        // Absent stays absent on the wire, so native keys are unchanged.
        assert_eq!(
            serde_json::to_string(&Options::default()).unwrap(),
            r#"{"render_mode":"sloping","reduce_shallow_to_horizontal":false,"use_depth_weighted_faces":false}"#
        );
        assert_ne!(
            Options {
                corner_fit_bound: Some(bounded(0.0)),
                ..Options::default()
            },
            Options::default()
        );
    }

    /// The census of DEPTH-ANOMALY.md, written independently of
    /// `reconstruct_corner`: walk each ring's groups, collect their inputs.
    fn census_groups(
        corner: &Corner,
        faces: &[Face],
        values: &[FaceWater],
    ) -> Vec<(Vec<usize>, Vec<f32>)> {
        let n = corner.faces.len();
        let conn: Vec<bool> = corner
            .faces
            .iter()
            .map(|&f| values[f].connection.connected())
            .collect();
        let mut groups = Vec::new();
        if corner
            .faces
            .iter()
            .all(|&f| values[f].values == [NODATA; 2])
        {
            return groups;
        }
        let mut done = vec![false; n];
        for j in 0..n {
            if done[j] {
                continue;
            }
            let mut stop = (j + 1) % n;
            while conn[stop] && stop != j {
                stop = (stop + 1) % n;
            }
            let mut first = j;
            if first != stop {
                while conn[first] {
                    first = (first + n - 1) % n;
                }
            }
            let (mut slots, mut inputs) = (Vec::new(), Vec::new());
            let mut current = first;
            loop {
                let f = corner.faces[current];
                let value = values[f].values[usize::from(faces[f].vertices[0] != 0)];
                if value != NODATA {
                    inputs.push(value);
                }
                slots.push(current);
                done[current] = true;
                current = (current + 1) % n;
                if current == stop {
                    break;
                }
            }
            let boundary = corner.faces[stop];
            if !conn[stop] {
                let value = values[boundary].values[usize::from(faces[boundary].vertices[0] == 0)];
                if value != NODATA {
                    inputs.push(value);
                }
            }
            groups.push((slots, inputs));
            if first == stop {
                break;
            }
        }
        groups
    }

    #[test]
    fn census_recheck_on_random_near_collinear_rings() {
        let mut seed = 0x2417240_u64;
        let mut next = move || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 11) as f64 / (1u64 << 53) as f64
        };
        let classes = [
            Unconnected,
            Levee,
            DownhillShallow,
            DownhillDeep,
            Connection::Backfill,
        ];
        let (mut evaluated, mut flagged, mut native_out) = (0, 0, 0);
        for tolerance in [0.0_f32, 0.5] {
            for _ in 0..4000 {
                let n = 3 + (next() * 5.0) as usize;
                // Half the rings: application points near one line the vertex
                // is off (extrapolation); half: points around the vertex.
                let offset = 0.5 + 8.0 * next();
                let around = next() < 0.5;
                let ring_faces: Vec<RingFace> = (0..n)
                    .map(|k| {
                        let along = -20.0 + 40.0 * next();
                        let across = if around {
                            -20.0 + 40.0 * next()
                        } else {
                            offset + 0.3 * (next() - 0.5)
                        };
                        let connection = classes[(next() * classes.len() as f64) as usize];
                        let draws = [next(), next(), next(), next()];
                        let wet = |dry: f64, z: f64| {
                            if dry < 0.15 {
                                NODATA
                            } else {
                                (100.0 + 10.0 * z) as f32
                            }
                        };
                        let values = if connection.connected() {
                            [(100.0 + 10.0 * draws[1]) as f32; 2]
                        } else {
                            [wet(draws[0], draws[1]), wet(draws[2], draws[3])]
                        };
                        let vertices = if next() < 0.5 { [0, k + 1] } else { [k + 1, 0] };
                        ([0, 0], vertices, values, connection, [along, across])
                    })
                    .collect();
                let (corner, faces, values) = ring([0.0, 0.0], &ring_faces);
                let cells = [Cell {
                    minimum: 0.0,
                    virtual_cell: false,
                }];
                let wse = [105.0_f32];
                let context = BoundContext {
                    bound: bounded(tolerance),
                    cells: &cells,
                    wse: &wse,
                    plot_tolerance: 0.001,
                };
                let (native, _, _) = reconstruct_corner(0, &corner, &faces, &values, None);
                let (result, _, report) =
                    reconstruct_corner(0, &corner, &faces, &values, Some(&context));
                let mut expected_flags = 0;
                let mut expected_evaluated = 0;
                for (slots, inputs) in census_groups(&corner, &faces, &values) {
                    for &slot in &slots {
                        let (v, b) = (native[slot], result[slot]);
                        if inputs.len() < 3 {
                            assert_eq!(v.to_bits(), b.to_bits());
                            continue;
                        }
                        let lo = inputs.iter().copied().fold(f32::INFINITY, f32::min);
                        let hi = inputs.iter().copied().fold(f32::NEG_INFINITY, f32::max);
                        let low = (f64::from(lo) - f64::from(tolerance)) as f32;
                        let high = (f64::from(hi) + f64::from(tolerance)) as f32;
                        // Re-census of the bounded output: nothing outside.
                        assert!(b >= low && b <= high, "{b} outside [{low}, {high}]");
                        if v.is_finite() && v >= low && v <= high {
                            assert_eq!(v.to_bits(), b.to_bits(), "unflagged fit changed");
                        } else {
                            native_out += 1;
                        }
                    }
                    if inputs.len() >= 3 {
                        expected_evaluated += 1;
                        let v = native[slots[0]];
                        let (lo, hi) = inputs
                            .iter()
                            .fold((f32::INFINITY, f32::NEG_INFINITY), |(a, b), &z| {
                                (a.min(z), b.max(z))
                            });
                        if !v.is_finite()
                            || f64::from(v) > f64::from(hi) + f64::from(tolerance)
                            || f64::from(v) < f64::from(lo) - f64::from(tolerance)
                        {
                            expected_flags += 1;
                        }
                    }
                }
                assert_eq!(report.bounded_corner_groups(), expected_flags);
                assert_eq!(report.groups_evaluated, expected_evaluated);
                evaluated += expected_evaluated;
                flagged += expected_flags;
            }
        }
        // The random rings exercise both outcomes many times.
        assert!(
            flagged > 500 && evaluated - flagged > 500,
            "{flagged} of {evaluated}"
        );
        assert!(native_out > 0);
    }

    #[test]
    fn same_profile_out_of_range_test_counts_pixels_above_their_inputs() {
        let profile = |bound: Option<CornerFitBound>, limit: f32| PreparedProfile {
            local_values: vec![vec![2.; 4]],
            face_values: vec![vec![]],
            depth_weights: vec![vec![]],
            horizontal: vec![None],
            wet: vec![true],
            options: Options {
                corner_fit_bound: bound,
                ..Options::default()
            },
            corner_bound: bound.map(|_| CornerBoundReport::default()),
            input_max: if bound.is_some() { vec![limit] } else { vec![] },
        };
        let owner = [1_u32, 1, 0, 1];
        let terrain = [0.0_f32; 4];
        let owner_cells = [[0_usize, 0]];
        let window = RasterWindow {
            native_grid: None,
            source_transform: [0., 1., 0., 2., 0., -1.],
            source_tile: [256, 256],
            width: 2,
            height: 2,
            transform: [0., 1., 0., 2., 0., -1.],
            owner: &owner,
            terrain: &terrain,
            owner_cells: &owner_cells,
        };
        let wse = [3.0_f32, f32::NAN, 9.0, 2.5];
        assert!(input_excess(&[profile(None, 0.0)], &window, &wse, 0).is_none());
        let result = input_excess(&[profile(Some(bounded(0.0)), 2.0)], &window, &wse, 0).unwrap();
        // Pixel 0 is 1.0 above (out), pixel 3 is 0.5 above (within the 0.72
        // margin), pixel 1 is dry and pixel 2 is unowned.
        assert_eq!(result.wet_pixels, 2);
        assert_eq!(result.out_of_range_pixels, 1);
        assert_eq!(result.max_excess, 1.0);
        let rows =
            input_excess(&[profile(Some(bounded(0.0)), 2.0)], &window, &wse[2..], 2).unwrap();
        assert_eq!((rows.wet_pixels, rows.out_of_range_pixels), (1, 0));
    }
}
