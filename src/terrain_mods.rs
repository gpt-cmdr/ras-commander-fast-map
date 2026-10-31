//! RAS Mapper terrain modifications on the effective terrain
//! (`rasmapper-effective-terrain/2`).
//!
//! The rules below were fitted pixel by pixel against
//! `RasTerrain.export_rasmapper_terrain(downsample_factor=1,
//! rasterize_modifications=True)` from HEC-RAS 7.0.1 on real,
//! RAS-Mapper-authored modifications (HCFCD Anderson Road `Prop_Topo`,
//! Kingswood `TerrainAlt1`, and the corpora listed in `method_record`). No
//! vendor code was read.
//!
//! * **Ground lines** (`Type=Levee`, subtype `Levee` = high ground, `Channel` =
//!   channel). RAS Mapper builds a triangulated template and rasterizes it:
//!   - the centerline is split at every profile station; the elevation at each
//!     centerline vertex is the profile interpolated linearly by station;
//!   - each side carries a crest edge offset by `Top Width / 2` and a toe
//!     offset by `Max Reach / 2` (so `Max Reach` is the **total** width);
//!     the crest edge is level with the centerline, the toe is
//!     `(Max Reach / 2 - Top Width / 2) / slope` below (levee) or above
//!     (channel) it; slopes are horizontal : vertical;
//!   - joints are beveled on the convex side (a fan around the vertex) and
//!     mitered on the concave side; the ends are cut square;
//!   - each strip quad (centerline to crest edge, crest edge to toe) is split
//!     along its Delaunay diagonal;
//!   - a pixel centre takes the linear value of the triangle containing it
//!     (overlapping triangles of one feature: the higher for a levee, the
//!     lower for a channel).
//! * **Polygons** with a constant elevation: pixel centres inside (even-odd).
//! * **Ellipses**: pixel centres strictly inside the ellipse (semi-axes as
//!   stored).
//!
//! Elevation types: `SetValue` replaces, `SetIfHigher` keeps the higher,
//! `SetIfLower` the lower. A nodata terrain pixel inside a modification takes
//! the modification's value (RAS Mapper writes it, however steep). Layers are
//! applied in descending `Priority` (Priority 0, the highest, last), features
//! in stored order; every result is
//! rounded to binary32 and then to 1/32 like the terrain itself.
use crate::{MapError, effective_terrain::Grid};
use serde::Deserialize;
use serde_json::{Value, json};

pub const NODATA: f32 = -9999.0;
const TRI_INCLUSION: f64 = -1e-9;
/// Profile stations within this distance of a centerline vertex do not split
/// the centerline (stored stations are binary32).
const STATION_SNAP: f64 = 1e-3;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
pub enum ElevationType {
    SetValue,
    SetIfHigher,
    SetIfLower,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GroundLineSubtype {
    Levee,
    Channel,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroundLine {
    pub elevation_type: ElevationType,
    pub top_width: f64,
    pub left_slope: f64,
    pub right_slope: f64,
    pub max_reach: f64,
    /// Centerline vertices `[x, y]` (one part).
    pub points: Vec<[f64; 2]>,
    /// `[station, elevation]`, stations ascending.
    pub profile: Vec<[f64; 2]>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Polygon {
    pub elevation_type: ElevationType,
    pub elevation: f64,
    /// Rings of `[x, y]`; inside is even-odd over all rings.
    pub rings: Vec<Vec<[f64; 2]>>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ellipse {
    pub elevation_type: ElevationType,
    pub elevation: f64,
    pub center: [f64; 2],
    pub radius_x: f64,
    pub radius_y: f64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Layer {
    GroundLine {
        name: String,
        priority: i64,
        subtype: GroundLineSubtype,
        features: Vec<GroundLine>,
    },
    Polygon {
        name: String,
        priority: i64,
        features: Vec<Polygon>,
    },
    Ellipse {
        name: String,
        priority: i64,
        features: Vec<Ellipse>,
    },
}

impl Layer {
    pub fn priority(&self) -> i64 {
        match self {
            Layer::GroundLine { priority, .. }
            | Layer::Polygon { priority, .. }
            | Layer::Ellipse { priority, .. } => *priority,
        }
    }
    pub fn name(&self) -> &str {
        match self {
            Layer::GroundLine { name, .. }
            | Layer::Polygon { name, .. }
            | Layer::Ellipse { name, .. } => name,
        }
    }
}

pub fn method_record() -> Value {
    json!({
        "supported": {
            "ground_line": {"subtypes": ["levee", "channel"], "elevation_types": ["SetIfHigher", "SetIfLower"],
                             "requires": "one part, a profile of >= 2 stations, no control points"},
            "polygon": {"elevation_types": ["SetValue", "SetIfHigher", "SetIfLower"],
                        "requires": "constant Elevation Value (Generate Boundary Elevations off, no control points)"},
            "ellipse": {"elevation_types": ["SetValue", "SetIfHigher", "SetIfLower"], "requires": "rotation 0"}
        },
        "ground_line_rule": "triangulated template: centerline split at profile stations; crest edges at Top Width/2, toes at Max Reach/2 (total width), toe offset (Max Reach/2 - Top Width/2)/slope (H:V) below (levee) or above (channel); convex joints beveled, concave mitered, square ends; quads split on the Delaunay diagonal; linear in the containing triangle",
        "polygon_rule": "pixel centre inside (even-odd), constant elevation",
        "ellipse_rule": "pixel centre strictly inside the ellipse",
        "combine": "SetValue replaces; SetIfHigher/SetIfLower keep the higher/lower; a nodata pixel inside takes the modification value; layers by descending Priority value (Priority 0 applied last, so it wins), features in stored order",
        "rounding": "binary32, then nearest 1/32 (ties to even), after each modification",
        "refused": "ground-line SetValue/Add, Max Reach <= Top Width, polygons with boundary profiles or control points, rotated ellipses, multi-part features, duplicate priorities, any other modification type (the caller also refuses ground lines joined through a shared System Name and control points)"
    })
}

fn invalid(msg: impl Into<String>) -> MapError {
    MapError::Invalid(msg.into())
}

fn finite(v: f64) -> bool {
    v.is_finite()
}

pub fn validate(layers: &[Layer]) -> Result<(), MapError> {
    for l in layers {
        match l {
            Layer::GroundLine { features, name, .. } => {
                for f in features {
                    if !matches!(
                        f.elevation_type,
                        ElevationType::SetIfHigher | ElevationType::SetIfLower
                    ) {
                        return Err(MapError::Unsupported(format!(
                            "ground line layer {name:?}: elevation type {:?} is not qualified",
                            f.elevation_type
                        )));
                    }
                    if f.points.len() < 2 || f.profile.len() < 2 {
                        return Err(invalid(format!(
                            "ground line layer {name:?}: needs >= 2 points and >= 2 profile stations"
                        )));
                    }
                    let ok = [f.top_width, f.left_slope, f.right_slope, f.max_reach]
                        .iter()
                        .all(|v| finite(*v))
                        && f.top_width >= 0.0
                        && f.left_slope > 0.0
                        && f.right_slope > 0.0
                        && f.max_reach > f.top_width
                        && f.points.iter().flatten().all(|v| finite(*v))
                        && f.profile.iter().flatten().all(|v| finite(*v))
                        && f.profile.windows(2).all(|w| w[1][0] >= w[0][0]);
                    if !ok {
                        return Err(invalid(format!(
                            "ground line layer {name:?}: non-finite or out-of-range geometry, slopes or profile"
                        )));
                    }
                    if f.points.windows(2).any(|w| w[0] == w[1]) {
                        return Err(MapError::Unsupported(format!(
                            "ground line layer {name:?}: repeated vertex"
                        )));
                    }
                }
            }
            Layer::Polygon { features, name, .. } => {
                for f in features {
                    if !finite(f.elevation)
                        || f.rings.is_empty()
                        || f.rings
                            .iter()
                            .any(|r| r.len() < 3 || r.iter().flatten().any(|v| !finite(*v)))
                    {
                        return Err(invalid(format!("polygon layer {name:?}: invalid polygon")));
                    }
                }
            }
            Layer::Ellipse { features, name, .. } => {
                for f in features {
                    if !(finite(f.elevation)
                        && f.center.iter().all(|v| finite(*v))
                        && f.radius_x > 0.0
                        && f.radius_y > 0.0
                        && finite(f.radius_x)
                        && finite(f.radius_y))
                    {
                        return Err(invalid(format!("ellipse layer {name:?}: invalid ellipse")));
                    }
                }
            }
        }
    }
    Ok(())
}

/// Rounding of every stored terrain value (binary32, then 1/32 ties to even).
fn quantize(v: f64) -> f32 {
    let f = v as f32 as f64;
    ((f * 32.0).round_ties_even() / 32.0) as f32
}

/// Window of pixels whose centres may fall in `[xmin, xmax] x [ymin, ymax]`.
struct Window {
    r0: usize,
    r1: usize,
    c0: usize,
    c1: usize,
}

fn window(grid: &Grid, b: [f64; 4]) -> Option<Window> {
    let cs = grid.cell_size;
    let c0 = ((b[0] - grid.origin_x) / cs - 0.5).floor().max(0.0);
    let c1 = ((b[2] - grid.origin_x) / cs + 0.5)
        .ceil()
        .min(grid.width as f64);
    let r0 = ((grid.origin_y - b[3]) / cs - 0.5).floor().max(0.0);
    let r1 = ((grid.origin_y - b[1]) / cs + 0.5)
        .ceil()
        .min(grid.height as f64);
    if !(c1 > c0 && r1 > r0) {
        return None;
    }
    Some(Window {
        r0: r0 as usize,
        r1: r1 as usize,
        c0: c0 as usize,
        c1: c1 as usize,
    })
}

#[inline]
fn centre(grid: &Grid, r: usize, c: usize) -> (f64, f64) {
    (
        grid.origin_x + (c as f64 + 0.5) * grid.cell_size,
        grid.origin_y - (r as f64 + 0.5) * grid.cell_size,
    )
}

fn combine(base: f32, s: f64, et: ElevationType) -> f32 {
    let v = if base == NODATA {
        s
    } else {
        let b = base as f64;
        match et {
            ElevationType::SetValue => s,
            ElevationType::SetIfHigher => b.max(s),
            ElevationType::SetIfLower => b.min(s),
        }
    };
    quantize(v)
}

type P3 = [f64; 3];

fn interp(profile: &[[f64; 2]], s: f64) -> f64 {
    // numpy.interp: clamps to the end values.
    if s <= profile[0][0] {
        return profile[0][1];
    }
    let last = profile[profile.len() - 1];
    if s >= last[0] {
        return last[1];
    }
    let k = profile.partition_point(|p| p[0] <= s);
    let (a, b) = (profile[k - 1], profile[k]);
    if b[0] == a[0] {
        return b[1];
    }
    a[1] + (s - a[0]) * (b[1] - a[1]) / (b[0] - a[0])
}

/// Centerline split at the profile stations, with stations.
fn densify(points: &[[f64; 2]], profile: &[[f64; 2]]) -> Vec<([f64; 2], f64)> {
    let mut out = vec![(points[0], 0.0)];
    let mut cum = 0.0;
    for w in points.windows(2) {
        let (a, b) = (w[0], w[1]);
        let len = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
        let next = cum + len;
        for p in profile {
            let sp = p[0];
            if cum + STATION_SNAP < sp && sp < next - STATION_SNAP {
                let t = (sp - cum) / len;
                out.push(([a[0] + t * (b[0] - a[0]), a[1] + t * (b[1] - a[1])], sp));
            }
        }
        out.push((b, next));
        cum = next;
    }
    out
}

/// Offset points of each vertex on one side (`side` +1 left, -1 right):
/// two at a convex joint (bevel), one otherwise (miter, or the normal at the
/// ends).
fn offset_side(line: &[[f64; 2]], d: f64, side: f64) -> Vec<Vec<[f64; 2]>> {
    let n = line.len();
    let mut u = Vec::with_capacity(n - 1);
    for w in line.windows(2) {
        let (dx, dy) = (w[1][0] - w[0][0], w[1][1] - w[0][1]);
        let l = (dx * dx + dy * dy).sqrt();
        u.push([dx / l, dy / l]);
    }
    let normal = |k: usize| [-u[k][1] * side, u[k][0] * side];
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let v = line[i];
        let at = |nn: [f64; 2], dd: f64| [v[0] + dd * nn[0], v[1] + dd * nn[1]];
        if i == 0 {
            out.push(vec![at(normal(0), d)]);
        } else if i == n - 1 {
            out.push(vec![at(normal(n - 2), d)]);
        } else {
            let (n1, n2) = (normal(i - 1), normal(i));
            let cross = u[i - 1][0] * u[i][1] - u[i - 1][1] * u[i][0];
            if cross.abs() < 1e-12 {
                out.push(vec![at(n1, d)]);
            } else if cross * side < 0.0 {
                out.push(vec![at(n1, d), at(n2, d)]);
            } else {
                let dot = n1[0] * n2[0] + n1[1] * n2[1];
                let m = [(n1[0] + n2[0]) / (1.0 + dot), (n1[1] + n2[1]) / (1.0 + dot)];
                out.push(vec![at(m, d)]);
            }
        }
    }
    out
}

fn in_circle(a: P3, b: P3, c: P3, d: P3) -> bool {
    let row = |p: P3| {
        let (x, y) = (p[0] - d[0], p[1] - d[1]);
        [x, y, x * x + y * y]
    };
    let (m0, m1, m2) = (row(a), row(b), row(c));
    let det = m0[0] * (m1[1] * m2[2] - m1[2] * m2[1]) - m0[1] * (m1[0] * m2[2] - m1[2] * m2[0])
        + m0[2] * (m1[0] * m2[1] - m1[1] * m2[0]);
    let o = (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]);
    det * o.signum() > 0.0
}

/// Quad `p0 p1 q1 q0` (p along one edge, q along the other) split on its
/// Delaunay diagonal.
fn quad(tris: &mut Vec<[P3; 3]>, p0: P3, p1: P3, q1: P3, q0: P3) {
    if !in_circle(p0, p1, q1, q0) {
        tris.push([p0, p1, q1]);
        tris.push([p0, q1, q0]);
    } else {
        tris.push([p0, p1, q0]);
        tris.push([p1, q1, q0]);
    }
}

pub fn ground_line_triangles(f: &GroundLine, subtype: GroundLineSubtype) -> Vec<[P3; 3]> {
    let half = f.top_width / 2.0;
    let reach = f.max_reach / 2.0;
    let sign = match subtype {
        GroundLineSubtype::Levee => -1.0,
        GroundLineSubtype::Channel => 1.0,
    };
    let dense = densify(&f.points, &f.profile);
    let line: Vec<[f64; 2]> = dense.iter().map(|p| p.0).collect();
    let z: Vec<f64> = dense.iter().map(|p| interp(&f.profile, p.1)).collect();
    let mut tris = Vec::new();
    for (side, slope) in [(1.0, f.left_slope), (-1.0, f.right_slope)] {
        let e = offset_side(&line, half, side);
        let t = offset_side(&line, reach, side);
        let dz = sign * (reach - half) / slope;
        let p3 = |p: [f64; 2], zz: f64| [p[0], p[1], zz];
        for i in 0..line.len() {
            if e[i].len() == 2 {
                let v = p3(line[i], z[i]);
                let (ea, eb) = (p3(e[i][0], z[i]), p3(e[i][1], z[i]));
                let (ta, tb) = (p3(t[i][0], z[i] + dz), p3(t[i][1], z[i] + dz));
                tris.push([v, ea, eb]);
                tris.push([ea, eb, tb]);
                tris.push([ea, tb, ta]);
            }
            if i + 1 == line.len() {
                continue;
            }
            let c0 = p3(line[i], z[i]);
            let c1 = p3(line[i + 1], z[i + 1]);
            let e0 = p3(*e[i].last().unwrap(), z[i]);
            let e1 = p3(e[i + 1][0], z[i + 1]);
            let t0 = p3(*t[i].last().unwrap(), z[i] + dz);
            let t1 = p3(t[i + 1][0], z[i + 1] + dz);
            quad(&mut tris, c0, c1, e1, e0);
            quad(&mut tris, e0, e1, t1, t0);
        }
    }
    tris
}

/// Rasterize one feature's triangles into `surf` (NaN = outside) over `w`.
fn raster_triangles(tris: &[[P3; 3]], grid: &Grid, w: &Window, levee: bool, surf: &mut [f64]) {
    let ww = w.c1 - w.c0;
    for [a, b, c] in tris {
        let den = (b[1] - c[1]) * (a[0] - c[0]) + (c[0] - b[0]) * (a[1] - c[1]);
        if den.abs() < 1e-12 {
            continue;
        }
        let bx = [
            a[0].min(b[0]).min(c[0]),
            a[1].min(b[1]).min(c[1]),
            a[0].max(b[0]).max(c[0]),
            a[1].max(b[1]).max(c[1]),
        ];
        let Some(tw) = window(grid, bx) else { continue };
        let (r0, r1) = (tw.r0.max(w.r0), tw.r1.min(w.r1));
        let (c0, c1) = (tw.c0.max(w.c0), tw.c1.min(w.c1));
        for r in r0..r1 {
            for cc in c0..c1 {
                let (x, y) = centre(grid, r, cc);
                let w1 = ((b[1] - c[1]) * (x - c[0]) + (c[0] - b[0]) * (y - c[1])) / den;
                let w2 = ((c[1] - a[1]) * (x - c[0]) + (a[0] - c[0]) * (y - c[1])) / den;
                let w3 = 1.0 - w1 - w2;
                if w1 < TRI_INCLUSION || w2 < TRI_INCLUSION || w3 < TRI_INCLUSION {
                    continue;
                }
                let v = w1 * a[2] + w2 * b[2] + w3 * c[2];
                let k = (r - w.r0) * ww + (cc - w.c0);
                let s = &mut surf[k];
                if s.is_nan() || (levee && v > *s) || (!levee && v < *s) {
                    *s = v;
                }
            }
        }
    }
}

fn point_in_rings(rings: &[Vec<[f64; 2]>], x: f64, y: f64) -> bool {
    let mut inside = false;
    for ring in rings {
        let n = ring.len();
        let mut j = n - 1;
        for i in 0..n {
            let (xi, yi) = (ring[i][0], ring[i][1]);
            let (xj, yj) = (ring[j][0], ring[j][1]);
            if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
                inside = !inside;
            }
            j = i;
        }
    }
    inside
}

/// Per-layer counts of changed pixels.
pub fn apply(layers: &[Layer], grid: &Grid, values: &mut [f32]) -> Result<Value, MapError> {
    validate(layers)?;
    let mut order: Vec<usize> = (0..layers.len()).collect();
    // Priority 0 is the highest: it is applied last and wins (HEC-RAS 7.0.1
    // export: a SetIfLower channel at Priority 0 cuts through a SetValue pad
    // at Priority 1).
    order.sort_by_key(|&i| std::cmp::Reverse(layers[i].priority()));
    let mut report = Vec::new();
    let width = grid.width;
    for &li in &order {
        let layer = &layers[li];
        let mut changed = 0_u64;
        let mut set = |r: usize, c: usize, s: f64, et: ElevationType| {
            let i = r * width + c;
            let v = combine(values[i], s, et);
            if v.to_bits() != values[i].to_bits() {
                changed += 1;
                values[i] = v;
            }
        };
        match layer {
            Layer::GroundLine {
                subtype, features, ..
            } => {
                for f in features {
                    let tris = ground_line_triangles(f, *subtype);
                    let mut b = [
                        f64::INFINITY,
                        f64::INFINITY,
                        f64::NEG_INFINITY,
                        f64::NEG_INFINITY,
                    ];
                    for t in &tris {
                        for p in t {
                            b = [
                                b[0].min(p[0]),
                                b[1].min(p[1]),
                                b[2].max(p[0]),
                                b[3].max(p[1]),
                            ];
                        }
                    }
                    let Some(w) = window(grid, b) else { continue };
                    let mut surf = vec![f64::NAN; (w.r1 - w.r0) * (w.c1 - w.c0)];
                    raster_triangles(
                        &tris,
                        grid,
                        &w,
                        *subtype == GroundLineSubtype::Levee,
                        &mut surf,
                    );
                    let ww = w.c1 - w.c0;
                    for r in w.r0..w.r1 {
                        for c in w.c0..w.c1 {
                            let s = surf[(r - w.r0) * ww + (c - w.c0)];
                            if !s.is_nan() {
                                set(r, c, s, f.elevation_type);
                            }
                        }
                    }
                }
            }
            Layer::Polygon { features, .. } => {
                for f in features {
                    let mut b = [
                        f64::INFINITY,
                        f64::INFINITY,
                        f64::NEG_INFINITY,
                        f64::NEG_INFINITY,
                    ];
                    for p in f.rings.iter().flatten() {
                        b = [
                            b[0].min(p[0]),
                            b[1].min(p[1]),
                            b[2].max(p[0]),
                            b[3].max(p[1]),
                        ];
                    }
                    let Some(w) = window(grid, b) else { continue };
                    for r in w.r0..w.r1 {
                        for c in w.c0..w.c1 {
                            let (x, y) = centre(grid, r, c);
                            if point_in_rings(&f.rings, x, y) {
                                set(r, c, f.elevation, f.elevation_type);
                            }
                        }
                    }
                }
            }
            Layer::Ellipse { features, .. } => {
                for f in features {
                    let b = [
                        f.center[0] - f.radius_x,
                        f.center[1] - f.radius_y,
                        f.center[0] + f.radius_x,
                        f.center[1] + f.radius_y,
                    ];
                    let Some(w) = window(grid, b) else { continue };
                    for r in w.r0..w.r1 {
                        for c in w.c0..w.c1 {
                            let (x, y) = centre(grid, r, c);
                            let (u, v) = (
                                (x - f.center[0]) / f.radius_x,
                                (y - f.center[1]) / f.radius_y,
                            );
                            if u * u + v * v < 1.0 {
                                set(r, c, f.elevation, f.elevation_type);
                            }
                        }
                    }
                }
            }
        }
        report.push(
            json!({"name": layer.name(), "priority": layer.priority(), "changed_pixels": changed}),
        );
    }
    Ok(Value::Array(report))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid() -> Grid {
        Grid {
            origin_x: 0.0,
            origin_y: 100.0,
            cell_size: 1.0,
            width: 100,
            height: 100,
        }
    }

    #[test]
    fn straight_levee_matches_closed_form() {
        let f = GroundLine {
            elevation_type: ElevationType::SetIfHigher,
            top_width: 10.0,
            left_slope: 2.0,
            right_slope: 4.0,
            max_reach: 40.0,
            points: vec![[10.0, 50.0], [90.0, 50.0]],
            profile: vec![[0.0, 20.0], [80.0, 28.0]],
        };
        let g = grid();
        let mut v = vec![0.0_f32; 100 * 100];
        apply(
            &[Layer::GroundLine {
                name: "l".into(),
                priority: 0,
                subtype: GroundLineSubtype::Levee,
                features: vec![f],
            }],
            &g,
            &mut v,
        )
        .unwrap();
        for r in 0..100 {
            for c in 0..100 {
                let (x, y) = centre(&g, r, c);
                let o = y - 50.0;
                let inside = (10.0..=90.0).contains(&x) && o.abs() <= 20.0;
                let slope = if o > 0.0 { 2.0 } else { 4.0 };
                let s = 20.0 + (x - 10.0) * 0.1 - (o.abs() - 5.0).max(0.0) / slope;
                let want = if inside { quantize(s.max(0.0)) } else { 0.0 };
                assert_eq!(v[r * 100 + c], want, "r{r} c{c}");
            }
        }
    }

    #[test]
    fn nodata_inside_takes_the_modification() {
        let g = grid();
        let mut v = vec![NODATA; 100 * 100];
        apply(
            &[Layer::Polygon {
                name: "p".into(),
                priority: 0,
                features: vec![Polygon {
                    elevation_type: ElevationType::SetIfLower,
                    elevation: 7.3,
                    rings: vec![vec![[10.0, 10.0], [20.0, 10.0], [20.0, 20.0], [10.0, 20.0]]],
                }],
            }],
            &g,
            &mut v,
        )
        .unwrap();
        assert_eq!(v[(100 - 15) * 100 + 15], quantize(7.3));
        assert_eq!(v[0], NODATA);
    }

    #[test]
    fn ellipse_is_strict() {
        let g = grid();
        let mut v = vec![1.0_f32; 100 * 100];
        apply(
            &[Layer::Ellipse {
                name: "e".into(),
                priority: 0,
                features: vec![Ellipse {
                    elevation_type: ElevationType::SetValue,
                    elevation: 5.0,
                    center: [50.5, 50.5],
                    radius_x: 2.0,
                    radius_y: 2.0,
                }],
            }],
            &g,
            &mut v,
        )
        .unwrap();
        // centre (52.5, 50.5) is exactly on the boundary: not inside.
        let at = |x: f64, y: f64| v[((100.0 - y) as usize) * 100 + x as usize];
        assert_eq!(at(52.5, 50.5), 1.0);
        assert_eq!(at(51.5, 50.5), 5.0);
    }

    #[test]
    fn unqualified_elevation_type_is_refused() {
        let layers = [Layer::GroundLine {
            name: "x".into(),
            priority: 0,
            subtype: GroundLineSubtype::Channel,
            features: vec![GroundLine {
                elevation_type: ElevationType::SetValue,
                top_width: 1.0,
                left_slope: 1.0,
                right_slope: 1.0,
                max_reach: 10.0,
                points: vec![[0.0, 0.0], [1.0, 0.0]],
                profile: vec![[0.0, 0.0], [1.0, 0.0]],
            }],
        }];
        assert!(matches!(validate(&layers), Err(MapError::Unsupported(_))));
    }
}
