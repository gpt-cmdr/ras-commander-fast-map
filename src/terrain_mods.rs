//! RAS Mapper terrain modifications on the effective terrain
//! (`rasmapper-effective-terrain/2`).
//!
//! Ground lines and ellipses follow RasMapperLib 7.0.1's export arithmetic as
//! described by an interoperability study of RasMapperLib 7.0.1 (prose
//! findings only; no vendor code used or copied). Polygons keep the rule
//! fitted pixel by pixel against
//! `RasTerrain.export_rasmapper_terrain(downsample_factor=1,
//! rasterize_modifications=True)`. Measured results are in `method_record`.
//!
//! * **Ground lines** (`Type=Levee`, subtype `Levee` = high ground, `Channel` =
//!   channel) without a `System Name`. Each feature becomes a ribbon TIN that
//!   is rasterized per 256-pixel output tile:
//!   - *Profile.* Stations (binary32) are shifted so the first is 0; stations
//!     beyond the centerline length are clamped to it; the first elevation per
//!     station wins. Every centerline segment longer than 100 ft (100/3.281 m)
//!     adds stations at 100, 200, ... from a running start that only advances
//!     over such long segments (RAS Mapper's own arithmetic, reproduced). If
//!     the last station falls more than 0.01 short of the length, the length
//!     is appended with the last elevation. Queries are binary32 linear
//!     interpolation, clamped at both ends.
//!   - *Offset lines.* Toes at `Max Reach / 2` and ridges at `Top Width / 2`
//!     on each side, M = centerline station. At a joint the previous and next
//!     offset segments meet in one point when they intersect (M averaged);
//!     otherwise the previous end, an arc around the vertex (chords split while
//!     the chord midpoint lies within 0.95 radius of the vertex) and the next
//!     start are emitted. Ends are square. Self-intersections are trimmed
//!     (the farthest later segment crossing a segment wins, M averaged), and
//!     the profile stations are injected where M crosses them.
//!   - *Elevations.* Centre and ridges take the profile at their station; the
//!     toes are `(Max Reach/2 - Top Width/2) / slope` below (levee) or above
//!     (channel); slopes are horizontal : vertical, 0 is read as 0.001. With
//!     `Max Reach <= Top Width` the toes fold inside the ridges and their
//!     strips draw nothing.
//!   - *TIN.* Five ribbons (left toe, left ridge, centre, right ridge, right
//!     toe; consecutive points within 0.01 dropped) zipped pairwise by the
//!     shorter diagonal (ties advance the right ribbon), fanned at the end.
//!     This is not a Delaunay triangulation.
//!   - *Raster.* The export TIN's edge-function rasterizer
//!     (`export_tin::rasterize`) per output tile: inclusive edges, the last
//!     triangle of the feature wins, clockwise (folded) triangles draw
//!     nothing. Features are never dropped for being narrow (the export
//!     forces rendering).
//!   - Features with `Top Width <= 0` or `Max Reach <= 0` change nothing.
//!   - A non-empty `System Name` is refused: RAS Mapper builds every named
//!     ground line, even a lone one, through a separate system path.
//! * **Polygons** with a constant elevation: pixel centres inside (even-odd).
//! * **Ellipses**: output-tile pixel centres inside or on the ellipse (`<= 1`,
//!   rotation in degrees); a circle (equal radii) uses the centre distance
//!   `<= radius`.
//!
//! Combination (per feature, binary32): a nodata terrain pixel takes the
//! modification value; `SetValue` replaces, `SetIfHigher` / `SetIfLower`
//! replace when strictly higher / lower. Layers are sorted by ascending
//! `Priority` (stable) and applied in reverse, so Priority 0 is applied last
//! and wins; features in stored order. Every result is rounded to binary32
//! and then to 1/32 like the terrain itself (RAS Mapper rounds once when it
//! writes; for these three types the results are the same).
use crate::{
    MapError,
    effective_terrain::{Grid, OutputLattice},
    export_tin::{self, CleanTin, TILE, TileGrid},
};
use serde::Deserialize;
use serde_json::{Value, json};

pub const NODATA: f32 = -9999.0;

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

/// Project length units (RAS Mapper's unit system): sets the 100 ft / 100 m
/// / 3.281 profile densification spacing.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LengthUnits {
    #[default]
    Feet,
    Meters,
}

impl LengthUnits {
    fn profile_spacing(self) -> f64 {
        match self {
            LengthUnits::Feet => 100.0,
            LengthUnits::Meters => 100.0 / 3.281,
        }
    }
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
    /// Stored `[station, elevation]` pairs.
    pub profile: Vec<[f64; 2]>,
    /// The feature's `System Name` attribute. Optional; a non-empty name is
    /// refused (RAS Mapper builds every named ground line, even a single one,
    /// through its system path, which is not reproduced).
    #[serde(default)]
    pub system_name: String,
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
    /// Rotation in degrees as stored (`Circles` column 5). Default 0.
    #[serde(default)]
    pub rotation: f64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Layer {
    GroundLine {
        name: String,
        priority: i64,
        subtype: GroundLineSubtype,
        /// Default `feet`.
        #[serde(default)]
        length_units: LengthUnits,
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
        "basis": "ground lines and ellipses: interoperability study of RasMapperLib 7.0.1 (no vendor code used or copied); polygons: fitted against HEC-RAS 7.0.1 exports",
        "supported": {
            "ground_line": {"subtypes": ["levee", "channel"], "elevation_types": ["SetValue", "SetIfHigher", "SetIfLower"],
                             "requires": "one part, a stored profile of >= 2 stations, slopes >= 0, empty System Name, no control points",
                             "length_units": "feet (default) or meters: profile densification every 100 ft or 100/3.281 m"},
            "polygon": {"elevation_types": ["SetValue", "SetIfHigher", "SetIfLower"],
                        "requires": "constant Elevation Value (Generate Boundary Elevations off, no control points)"},
            "ellipse": {"elevation_types": ["SetValue", "SetIfHigher", "SetIfLower"], "rotation": "degrees, any"}
        },
        "ground_line_rule": "profile: binary32 stations shifted to start at 0, clamped to the length, first value per station, 100 ft (100/3.281 m) densification from a running start that only advances over segments longer than the spacing, length appended when the last station is > 0.01 short; binary32 linear query clamped at both ends. Offset lines: toes at Max Reach/2, ridges at Top Width/2, M = centerline station; joints meet at the offset-segment intersection (M averaged) or get the previous end, an arc split while chord-mid/r < 0.95, and the next start; square ends; self-intersections trimmed (farthest crossing later segment, M averaged); profile stations injected by M fraction. Centre and ridges z = profile(station); toes z = profile -/+ (Max Reach/2 - Top Width/2)/slope (binary32 half-widths, binary64 product), slope 0 read as 0.001; Max Reach <= Top Width folds the toes inside the ridges (those strips draw nothing). Ribbon TIN (left toe, left ridge, centre, right ridge, right toe; 0.01 dedupe; shorter diagonal, ties advance the right ribbon; fan at the end), rasterized per 256-pixel output tile with the export TIN rasterizer (inclusive, last triangle wins, clockwise triangles draw nothing). Top Width <= 0 or Max Reach <= 0: no change",
        "polygon_rule": "pixel centre inside (even-odd), constant elevation",
        "ellipse_rule": "per output-tile pixel centre: circle (equal radii) distance <= radius; otherwise (c dx + s dy)^2/rx^2 + (s dx - c dy)^2/ry^2 <= 1 with c, s = cos, sin(-rotation degrees)",
        "combine": "binary32: a nodata terrain pixel takes the modification value; SetValue replaces; SetIfHigher/SetIfLower replace when strictly higher/lower; layers stable-sorted by ascending Priority and applied in reverse (Priority 0 last, so it wins), features in stored order",
        "rounding": "binary32, then nearest 1/32 (ties to even), after each feature",
        "qualified": {
            "scope": "fast-map 0.16.0 working tree, ras2fim-batch effective-terrain, request schema 2, HEC-RAS 7.0.1 exports with rasterize_modifications=True, downsample_factor=1, whole terrain",
            "real terrains A and B1-B4 (pixels where two exports differ excluded; ground lines Hi/Lo without System Name, constant polygons, circles)": "no difference inside any modification footprint: B1 0 values (was 3 at 1/32 ft), B2 0 values (was 443, max 1.47 ft), B3 and B4 0; the remaining pixels (A 322 values, max 0.53 ft; B1, B2 and B3 2 mask pixels each) lie outside every footprint, in export-TIN gap fills",
            "synthetic T2 terrain, 20 ft (one export each, System Name blank)": "22.5 km channel, 21.2 km levee and channel + polygon + levee: 0 modified pixels differ; four channels with 76-180 degree bends, 50-63 ft segments, a shifted and clamped profile, a crossing and a hairpin overlap: 2 of 8,935 pixels at 1/32 ft; two levees with short segments and a spiral: 0 of 2,384; ground-line SetValue: 0 of 5,282; Max Reach <= Top Width (3 features): 0 of 1,325; five ellipses (exact circle boundary, rotations 0, 30, -63.5, 90 degrees): 0 of 1,005 (2 edge pixels differ in the base export)",
            "named ground lines": "the 22.5 km channel with System Name 'chan' differs from its blank-name export on 1,506 pixels (RAS Mapper's system path): refused"
        },
        "refused": "ground-line Add (needs the unrounded terrain; not measured), ground lines with a System Name, negative slopes, polygons with boundary profiles, shape Z or control points, multi-part features, any other modification type (the caller also refuses ground-line control points and duplicate priorities)"
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
                    if !f.system_name.trim().is_empty() {
                        return Err(MapError::Unsupported(format!(
                            "ground line layer {name:?}: System Name {:?} is not qualified (RAS Mapper builds named ground lines through its system path)",
                            f.system_name
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
                        && f.points.iter().flatten().all(|v| finite(*v))
                        && f.profile.iter().flatten().all(|v| finite(*v));
                    if !ok {
                        return Err(invalid(format!(
                            "ground line layer {name:?}: non-finite geometry, template or profile"
                        )));
                    }
                    if f.left_slope < 0.0 || f.right_slope < 0.0 {
                        return Err(MapError::Unsupported(format!(
                            "ground line layer {name:?}: negative side slopes are not qualified"
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
                        && finite(f.radius_y)
                        && finite(f.rotation))
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

/// RAS Mapper's per-pixel replacement in binary32, then the terrain rounding.
fn combine(base: f32, m: f32, et: ElevationType) -> f32 {
    let v = if base == NODATA {
        m
    } else if m == NODATA {
        base
    } else {
        match et {
            ElevationType::SetValue => m,
            ElevationType::SetIfHigher => {
                if m > base {
                    m
                } else {
                    base
                }
            }
            ElevationType::SetIfLower => {
                if m < base {
                    m
                } else {
                    base
                }
            }
        }
    };
    quantize(v as f64)
}

// ---------------------------------------------------------------------------
// Ground-line geometry
// ---------------------------------------------------------------------------

/// A point with z and m (NaN when absent).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Pm {
    x: f64,
    y: f64,
    z: f64,
    m: f64,
}

impl Pm {
    fn xy(x: f64, y: f64) -> Pm {
        Pm {
            x,
            y,
            z: f64::NAN,
            m: f64::NAN,
        }
    }
}

#[inline]
fn dist(a: Pm, b: Pm) -> f64 {
    let (dx, dy) = (a.x - b.x, a.y - b.y);
    (dx * dx + dy * dy).sqrt()
}

/// `a + t (b - a)` on every ordinate.
#[inline]
fn lerp(a: Pm, b: Pm, t: f64) -> Pm {
    Pm {
        x: a.x + t * (b.x - a.x),
        y: a.y + t * (b.y - a.y),
        z: a.z + t * (b.z - a.z),
        m: a.m + t * (b.m - a.m),
    }
}

fn length(p: &[Pm]) -> f64 {
    let mut l = 0.0;
    for w in p.windows(2) {
        l += dist(w[0], w[1]);
    }
    l
}

/// Segment intersection with a 1e-6 slack on the bounding boxes and on the
/// parameters (clamped into [0, 1] when only the slack admits them). Returns
/// the point on the first segment at the unclamped parameter, and both
/// parameters.
fn intersect(a0: Pm, a1: Pm, b0: Pm, b1: Pm) -> Option<(f64, f64, f64, f64)> {
    const EPS: f64 = 1e-6;
    if a0.x.max(a1.x) + EPS < b0.x.min(b1.x)
        || a0.x.min(a1.x) - EPS > b0.x.max(b1.x)
        || a0.y.min(a1.y) - EPS > b0.y.max(b1.y)
        || a0.y.max(a1.y) + EPS < b0.y.min(b1.y)
    {
        return None;
    }
    let (dx, dy) = (a1.x - a0.x, a1.y - a0.y);
    let (ex, ey) = (b1.x - b0.x, b1.y - b0.y);
    let den = ey * dx - ex * dy;
    if den == 0.0 {
        return None;
    }
    let (oy, ox) = (a0.y - b0.y, a0.x - b0.x);
    let t1 = (ex * oy - ey * ox) / den;
    let t2 = (dx * oy - dy * ox) / den;
    let (x, y) = (a0.x + t1 * dx, a0.y + t1 * dy);
    let unit = |t: f64| (0.0..=1.0).contains(&t);
    if unit(t1) && unit(t2) {
        return Some((x, y, t1, t2));
    }
    let slack = |t: f64| t > -EPS && t < 1.0 + EPS;
    if slack(t1) && slack(t2) {
        return Some((x, y, t1.clamp(0.0, 1.0), t2.clamp(0.0, 1.0)));
    }
    None
}

fn sign(v: f64) -> f64 {
    if v > 0.0 {
        1.0
    } else if v < 0.0 {
        -1.0
    } else {
        0.0
    }
}

/// Left unit normal of `a -> b`.
fn left_normal(a: Pm, b: Pm) -> (f64, f64) {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    if dx == 0.0 {
        (sign(-dy), 0.0)
    } else if dy != 0.0 {
        let l = (dy * dy + dx * dx).sqrt();
        (-dy / l, dx / l)
    } else {
        (0.0, sign(dx))
    }
}

/// Arc points from `s` to `e` around `c` (radius `r`), chord split while the
/// chord midpoint is within `tol * r` of the centre; `e` is emitted at every
/// level of the split.
fn arc(r: f64, c: Pm, s: Pm, e: Pm, out: &mut Vec<Pm>, tol: f64, last: f64) {
    let mid = lerp(s, e, 0.5);
    let ratio = dist(c, mid) / r;
    if ratio != 0.0 && last != ratio {
        if ratio < tol {
            let on = lerp(c, mid, 1.0 / ratio);
            let p = Pm {
                z: mid.z,
                m: mid.m,
                ..on
            };
            arc(r, c, s, p, out, tol, ratio);
            arc(r, c, p, e, out, tol, ratio);
        }
        out.push(e);
    }
}

/// Removes loops: for each segment, the farthest later (non-adjacent) segment
/// that crosses it is joined at the crossing (M averaged).
fn trim_self_intersections(p: &mut Vec<Pm>) {
    if p.len() <= 3 {
        return;
    }
    let mut i = 0;
    while i + 2 < p.len() {
        let (a0, a1) = (p[i], p[i + 1]);
        let nseg = p.len() - 1;
        for k in (i + 2..nseg).rev() {
            let (b0, b1) = (p[k], p[k + 1]);
            if let Some((x, y, t1, t2)) = intersect(a0, a1, b0, b1) {
                let (pa, pb) = (lerp(a0, a1, t1), lerp(b0, b1, t2));
                p[i + 1] = Pm {
                    x,
                    y,
                    z: (pa.z + pb.z) * 0.5,
                    m: (pa.m + pb.m) * 0.5,
                };
                p.drain(i + 2..=k);
                break;
            }
        }
        i += 1;
    }
}

/// Left and right offset lines at `width / 2`, M = cumulative centerline
/// distance.
fn simple_offset(stream: &[Pm], width: f64) -> (Vec<Pm>, Vec<Pm>) {
    let r = width * 0.5;
    let (mut left, mut right) = (Vec::new(), Vec::new());
    let mut m = 0.0;
    let mut prev: [[Pm; 2]; 2] = [[Pm::xy(0.0, 0.0); 2]; 2];
    let mut cur = prev;
    for i in 0..stream.len() - 1 {
        let (a, b) = (stream[i], stream[i + 1]);
        let (nx, ny) = left_normal(a, b);
        let (dx, dy) = (nx * r, ny * r);
        let len = dist(a, b);
        let m0 = m;
        m += len;
        let at = |p: Pm, sx: f64, sy: f64, mm: f64| Pm {
            x: p.x + sx,
            y: p.y + sy,
            z: p.z,
            m: mm,
        };
        cur = [
            [at(a, dx, dy, m0), at(b, dx, dy, m)],
            [at(a, -dx, -dy, m0), at(b, -dx, -dy, m)],
        ];
        if i == 0 {
            left.push(cur[0][0]);
            right.push(cur[1][0]);
        } else if len != 0.0 {
            for (side, out) in [&mut left, &mut right].into_iter().enumerate() {
                let (p, c) = (prev[side], cur[side]);
                if let Some((x, y, t1, t2)) = intersect(p[0], p[1], c[0], c[1]) {
                    let (pa, pb) = (lerp(p[0], p[1], t1), lerp(c[0], c[1], t2));
                    out.push(Pm {
                        x,
                        y,
                        z: (pa.z + pb.z) * 0.5,
                        m: (pa.m + pb.m) * 0.5,
                    });
                } else {
                    out.push(p[1]);
                    arc(r, a, p[1], c[0], out, 0.95, 0.0);
                }
            }
        }
        prev = cur;
    }
    left.push(cur[0][1]);
    right.push(cur[1][1]);
    trim_self_intersections(&mut left);
    trim_self_intersections(&mut right);
    (left, right)
}

/// Binary32 profile `(station, value)`, sorted.
type Profile = Vec<(f32, f32)>;

/// Linear in binary32, clamped at both ends (the first station above `x`
/// bounds the interval).
fn query(p: &[(f32, f32)], x: f32) -> f32 {
    for (i, s) in p.iter().enumerate() {
        if x < s.0 {
            if i == 0 {
                return s.1;
            }
            let a = p[i - 1];
            let slope = (s.1 - a.1) / (s.0 - a.0);
            return a.1 + slope * (x - a.0);
        }
    }
    p[p.len() - 1].1
}

fn sort_stations(p: &mut Profile) {
    p.sort_by(|a, b| a.0.total_cmp(&b.0));
}

/// The centre profile RAS Mapper derives from the stored profile (no control
/// points).
fn levee_profile(stored: &[[f64; 2]], line: &[Pm], len: f64, spacing: f64) -> Profile {
    let mut keys: Profile = Vec::with_capacity(stored.len());
    let first = stored[0][0] as f32;
    let shift = if first != 0.0 { -first } else { 0.0 };
    for s in stored {
        let x0 = s[0] as f32;
        let x = if x0 as f64 > len {
            len as f32
        } else {
            x0 + shift
        };
        if !keys.iter().any(|k| k.0 == x) {
            keys.push((x, s[1] as f32));
        }
    }
    let mut prof = keys.clone();
    sort_stations(&mut prof);
    let mut extra = Vec::new();
    let mut start = 0.0;
    for w in line.windows(2) {
        let l = dist(w[0], w[1]);
        if l <= spacing {
            continue;
        }
        let n = (l / spacing).floor() as i64 - 1;
        for k in 0..=n {
            let s = (start + (k + 1) as f64 * spacing) as f32;
            if !keys.iter().any(|q| q.0 == s) {
                extra.push((s, query(&prof, s)));
            }
        }
        // Advances only over long segments (as RAS Mapper does).
        start += l;
    }
    prof.extend(extra);
    sort_stations(&mut prof);
    let last = prof[prof.len() - 1];
    if (last.0 as f64 - len).abs() > 0.01 {
        prof.push((len as f32, last.1));
    }
    prof
}

/// Running position for repeated distance queries along a polyline.
struct Walk {
    seg: usize,
    sum: f64,
}

fn point_at_distance(p: &[Pm], total: f64, d: f64, w: &mut Walk) -> Pm {
    let nseg = p.len() - 1;
    if d <= 0.0 {
        w.seg = 0;
        return p[0];
    }
    if d >= total {
        w.seg = nseg - 1;
        return p[nseg];
    }
    for i in w.seg..nseg {
        let start = w.sum;
        let l = dist(p[i], p[i + 1]);
        w.sum += l;
        if w.sum >= d {
            w.sum = start;
            w.seg = i;
            return lerp(p[i], p[i + 1], (d - start) / l);
        }
    }
    w.seg = nseg - 1;
    p[nseg]
}

/// Centerline with the interior profile stations injected; z from the
/// profile.
fn centre_line(line: &[Pm], prof: &[(f32, f32)], total: f64) -> Vec<Pm> {
    let nseg = line.len() - 1;
    let mut out = vec![Pm {
        z: query(prof, 0.0) as f64,
        m: 0.0,
        ..line[0]
    }];
    let (mut k, mut seg, mut cum) = (1, 0, 0.0);
    let mut walk = Walk { seg: 0, sum: 0.0 };
    loop {
        let rel = if k < prof.len() - 1 {
            (prof[k].0 as f64 - cum) as f32
        } else {
            f32::MAX
        };
        let l = dist(line[seg], line[seg + 1]);
        let vertex = |cum: f64| {
            let s = (l + cum) as f32;
            Pm {
                z: query(prof, s) as f64,
                m: s as f64 / total,
                ..line[seg + 1]
            }
        };
        if (rel as f64 - l).abs() < 1e-5 {
            out.push(vertex(cum));
            cum += l;
            k += 1;
            seg += 1;
            if seg == nseg {
                break;
            }
        } else if (rel as f64) < l {
            let q = point_at_distance(line, total, rel as f64 + cum, &mut walk);
            out.push(Pm {
                x: q.x,
                y: q.y,
                z: prof[k].1 as f64,
                m: prof[k].0 as f64 / total,
            });
            k += 1;
        } else {
            out.push(vertex(cum));
            cum += l;
            seg += 1;
            if seg == nseg {
                break;
            }
        }
    }
    out
}

/// Injects a point wherever the offset line's M crosses one of the interior
/// `fractions` (segments of constant M, such as arcs, are skipped).
fn inject_fractions(p: &[Pm], fr: &[f64]) -> Vec<Pm> {
    if fr.len() < 2 || p.len() < 2 {
        return p.to_vec();
    }
    let nseg = p.len() - 1;
    let total = length(p);
    let seg_len = |k: usize| dist(p[k], p[k + 1]);
    let mut out = vec![Pm { m: 0.0, ..p[0] }];
    let (mut k, mut mseg, mut gseg) = (1, 0, 0);
    let (mut gcum, mut mcum) = (0.0, 0.0);
    let mut walk = Walk { seg: 0, sum: 0.0 };
    let advance = |mseg: &mut usize, mcum: &mut f64, f: f64| {
        while *mseg + 1 < nseg && !(p[*mseg].m <= f && f < p[*mseg + 1].m) {
            *mcum += seg_len(*mseg);
            *mseg += 1;
        }
    };
    loop {
        let mut at = f32::MAX;
        if k < fr.len() - 1 {
            while mseg < nseg && p[mseg].m == p[mseg + 1].m {
                mcum += seg_len(mseg);
                mseg += 1;
            }
            if mseg < nseg {
                let (a, b) = (p[mseg], p[mseg + 1]);
                let t = (fr[k] - a.m) / (b.m - a.m);
                at = (mcum + dist(a, lerp(a, b, t))) as f32;
            }
        }
        let end = gcum + seg_len(gseg);
        if (at as f64 - end).abs() < 1e-5 {
            out.push(p[gseg + 1]);
            gcum = end;
            k += 1;
            gseg += 1;
            advance(&mut mseg, &mut mcum, fr[k]);
            if gseg == nseg {
                break;
            }
        } else if (at as f64) < end {
            let q = point_at_distance(p, total, at as f64, &mut walk);
            out.push(Pm {
                x: q.x,
                y: q.y,
                z: f64::NAN,
                m: fr[k],
            });
            k += 1;
            advance(&mut mseg, &mut mcum, fr[k]);
        } else {
            out.push(p[gseg + 1]);
            gcum = end;
            gseg += 1;
            if gseg == nseg {
                break;
            }
        }
    }
    out
}

fn dedupe_xy(line: &[Pm], tol: f64) -> Vec<Pm> {
    let mut out = vec![line[0]];
    for &p in &line[1..] {
        if dist(p, *out.last().unwrap()) >= tol {
            out.push(p);
        }
    }
    out
}

/// Zips adjacent ribbons into triangles (shorter diagonal; ties advance the
/// right ribbon; fan when one ribbon is exhausted).
fn ribbon_tin(ribbons: &[Vec<Pm>], tol: f64) -> CleanTin {
    let mut pts: Vec<[f64; 3]> = Vec::new();
    let mut spans = Vec::with_capacity(ribbons.len());
    for rb in ribbons {
        let start = pts.len();
        for (i, p) in rb.iter().enumerate() {
            let keep = i == 0 || {
                let q = pts[pts.len() - 1];
                !export_tin::close_to(q[0], q[1], p.x, p.y, tol)
            };
            if keep {
                pts.push([p.x, p.y, p.z]);
            }
        }
        spans.push((start, pts.len() - start));
    }
    let d = |a: usize, b: usize| {
        let (p, q) = (pts[a], pts[b]);
        ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2)).sqrt()
    };
    let mut tris = Vec::new();
    for w in spans.windows(2) {
        let ((a0, na), (b0, nb)) = (w[0], w[1]);
        let (mut i, mut j) = (a0, b0);
        while i - a0 != na - 1 || j - b0 != nb - 1 {
            let (i1, j1) = (i + 1, j + 1);
            if i1 - a0 >= na {
                tris.push([i, j, j1]);
                j += 1;
            } else if j1 - b0 >= nb || d(i, j1) > d(j, i1) {
                tris.push([i, j, i1]);
                i += 1;
            } else {
                tris.push([i, j, j1]);
                j += 1;
            }
        }
    }
    CleanTin {
        points: pts,
        triangles: tris.into_iter().map(|t| t.map(|v| v as i32)).collect(),
        crossings: 0,
    }
}

/// The ribbon TIN of one ground-line feature; `None` when RAS Mapper builds
/// none (Top Width or Max Reach not positive).
pub(crate) fn ground_line_tin(
    f: &GroundLine,
    subtype: GroundLineSubtype,
    units: LengthUnits,
) -> Option<CleanTin> {
    let reach = f.max_reach as f32;
    let top = f.top_width as f32;
    // Finite (validated), so these are RAS Mapper's `<= 0` skips.
    if reach <= 0.0 || top <= 0.0 {
        return None;
    }
    let line: Vec<Pm> = f.points.iter().map(|p| Pm::xy(p[0], p[1])).collect();
    let total = length(&line);
    let prof = levee_profile(&f.profile, &line, total, units.profile_spacing());
    let stream = dedupe_xy(&line, 0.001_f32 as f64);
    if stream.len() < 2 {
        return None;
    }
    let scale = |v: Vec<Pm>| -> Vec<Pm> {
        v.into_iter()
            .map(|p| Pm {
                m: if p.m.is_nan() { p.m } else { p.m / total },
                ..p
            })
            .collect()
    };
    let (lt, rt) = simple_offset(&stream, reach as f64);
    let (lr, rr) = simple_offset(&stream, top as f64);
    let (lt, rt, lr, rr) = (scale(lt), scale(rt), scale(lr), scale(rr));
    if [&lt, &rt, &lr, &rr].iter().any(|v| v.len() < 2) {
        return None;
    }
    let centre = centre_line(&line, &prof, total);
    let centre_len = length(&centre);
    let fractions: Vec<f64> = prof.iter().map(|p| p.0 as f64 / total).collect();
    let station = |p: &Pm| query(&prof, (p.m * centre_len) as f32);
    let ridge = |v: &[Pm]| -> Vec<Pm> {
        inject_fractions(v, &fractions)
            .into_iter()
            .map(|p| Pm {
                z: station(&p) as f64,
                ..p
            })
            .collect()
    };
    let usable = |s: f64| {
        let s = s as f32;
        if s == 0.0 { 0.001_f32 } else { s }
    };
    let (mut ls, mut rs) = (usable(f.left_slope), usable(f.right_slope));
    match subtype {
        GroundLineSubtype::Channel => ls = -ls,
        GroundLineSubtype::Levee => rs = -rs,
    }
    let run = reach / 2.0 - top / 2.0;
    let toe = |v: &[Pm], slope: f32, side: f64| -> Vec<Pm> {
        inject_fractions(v, &fractions)
            .into_iter()
            .map(|p| Pm {
                z: station(&p) as f64 + side * (run as f64 * (1.0 / slope as f64)),
                ..p
            })
            .collect()
    };
    let ribbons = [
        toe(&lt, ls, -1.0),
        ridge(&lr),
        centre,
        ridge(&rr),
        toe(&rt, rs, 1.0),
    ];
    Some(ribbon_tin(&ribbons, 0.01))
}

// ---------------------------------------------------------------------------
// Application
// ---------------------------------------------------------------------------

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

fn ellipse_contains(f: &Ellipse, x: f64, y: f64) -> bool {
    let (dx, dy) = (x - f.center[0], y - f.center[1]);
    if f.radius_x == f.radius_y {
        return (dx * dx + dy * dy).sqrt() <= f.radius_x;
    }
    let a = (0.0 - f.rotation) * std::f64::consts::PI / 180.0;
    let (c, s) = (a.cos(), a.sin());
    let u = (c * dx + s * dy).powi(2);
    let v = (s * dx - c * dy).powi(2);
    u / (f.radius_x * f.radius_x) + v / (f.radius_y * f.radius_y) <= 1.0
}

/// Output tiles of the grid, with their RAS Mapper geometry.
struct Tiles {
    lattice: OutputLattice,
    tx: usize,
    ty: usize,
    width: usize,
    height: usize,
}

impl Tiles {
    fn new(grid: &Grid) -> Self {
        Tiles {
            lattice: OutputLattice::new(grid),
            tx: grid.width.div_ceil(TILE),
            ty: grid.height.div_ceil(TILE),
            width: grid.width,
            height: grid.height,
        }
    }

    /// Tiles overlapping `[xmin, ymin, xmax, ymax]`, with their grid row and
    /// column counts.
    fn over(&self, b: [f64; 4]) -> Vec<(usize, usize, TileGrid, usize, usize)> {
        let e = [b[0], b[2], b[1], b[3]];
        let mut out = Vec::new();
        for l in 0..self.ty {
            for m in 0..self.tx {
                let tg = self.lattice.tile_grid(l, m);
                if tg.overlaps(&e) {
                    let rows = TILE.min(self.height - l * TILE);
                    let cols = TILE.min(self.width - m * TILE);
                    out.push((l, m, tg, rows, cols));
                }
            }
        }
        out
    }
}

fn bbox_of(points: impl Iterator<Item = (f64, f64)>) -> [f64; 4] {
    let mut b = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    for (x, y) in points {
        b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
    }
    b
}

/// Per-layer counts of changed pixels.
pub fn apply(layers: &[Layer], grid: &Grid, values: &mut [f32]) -> Result<Value, MapError> {
    validate(layers)?;
    let tiles = Tiles::new(grid);
    // Ascending Priority (stable), applied in reverse: Priority 0 is applied
    // last and wins.
    let mut order: Vec<usize> = (0..layers.len()).collect();
    order.sort_by_key(|&i| layers[i].priority());
    order.reverse();
    let mut report = Vec::new();
    let width = grid.width;
    let mut buf = vec![f32::NAN; TILE * TILE];
    for &li in &order {
        let layer = &layers[li];
        let mut changed = 0_u64;
        let mut skipped = 0_u64;
        let mut set = |r: usize, c: usize, s: f32, et: ElevationType| {
            let i = r * width + c;
            let v = combine(values[i], s, et);
            if v.to_bits() != values[i].to_bits() {
                changed += 1;
                values[i] = v;
            }
        };
        match layer {
            Layer::GroundLine {
                subtype,
                features,
                length_units,
                ..
            } => {
                for f in features {
                    let Some(tin) = ground_line_tin(f, *subtype, *length_units) else {
                        skipped += 1;
                        continue;
                    };
                    let b = bbox_of(tin.points.iter().map(|p| (p[0], p[1])));
                    for (l, m, tg, rows, cols) in tiles.over(b) {
                        buf.fill(f32::NAN);
                        export_tin::rasterize(&tin, &tg, &mut buf);
                        for i in 0..rows {
                            for j in 0..cols {
                                let z = buf[i * TILE + j];
                                if !z.is_nan() {
                                    set(l * TILE + i, m * TILE + j, z, f.elevation_type);
                                }
                            }
                        }
                    }
                }
            }
            Layer::Polygon { features, .. } => {
                for f in features {
                    let b = bbox_of(f.rings.iter().flatten().map(|p| (p[0], p[1])));
                    let Some(w) = window(grid, b) else { continue };
                    for r in w.r0..w.r1 {
                        for c in w.c0..w.c1 {
                            let (x, y) = centre(grid, r, c);
                            if point_in_rings(&f.rings, x, y) {
                                set(r, c, f.elevation as f32, f.elevation_type);
                            }
                        }
                    }
                }
            }
            Layer::Ellipse { features, .. } => {
                for f in features {
                    let a = f.rotation.to_radians();
                    let (ca, sa) = (a.cos().abs(), a.sin().abs());
                    let ex = (f.radius_x * ca).hypot(f.radius_y * sa) + grid.cell_size;
                    let ey = (f.radius_x * sa).hypot(f.radius_y * ca) + grid.cell_size;
                    let b = [
                        f.center[0] - ex,
                        f.center[1] - ey,
                        f.center[0] + ex,
                        f.center[1] + ey,
                    ];
                    for (l, m, tg, rows, cols) in tiles.over(b) {
                        for i in 0..rows {
                            let y = tg.ccy(i as i64);
                            if y < b[1] || y > b[3] {
                                continue;
                            }
                            for j in 0..cols {
                                let x = tg.ccx(j as i64);
                                if x >= b[0] && x <= b[2] && ellipse_contains(f, x, y) {
                                    set(
                                        l * TILE + i,
                                        m * TILE + j,
                                        f.elevation as f32,
                                        f.elevation_type,
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
        let mut rec =
            json!({"name": layer.name(), "priority": layer.priority(), "changed_pixels": changed});
        if skipped > 0 {
            rec["skipped_features"] = json!(skipped);
        }
        report.push(rec);
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

    fn levee(points: Vec<[f64; 2]>, profile: Vec<[f64; 2]>) -> GroundLine {
        GroundLine {
            elevation_type: ElevationType::SetIfHigher,
            top_width: 10.0,
            left_slope: 2.0,
            right_slope: 4.0,
            max_reach: 40.0,
            points,
            profile,
            system_name: String::new(),
        }
    }

    fn gl_layer(subtype: GroundLineSubtype, features: Vec<GroundLine>) -> Layer {
        Layer::GroundLine {
            name: "l".into(),
            priority: 0,
            subtype,
            length_units: LengthUnits::Feet,
            features,
        }
    }

    #[test]
    fn straight_levee_matches_closed_form() {
        let f = levee(
            vec![[10.0, 50.0], [90.0, 50.0]],
            vec![[0.0, 20.0], [80.0, 28.0]],
        );
        let g = grid();
        let mut v = vec![0.0_f32; 100 * 100];
        apply(&[gl_layer(GroundLineSubtype::Levee, vec![f])], &g, &mut v).unwrap();
        for r in 0..100 {
            for c in 0..100 {
                let (x, y) = centre(&g, r, c);
                let o = y - 50.0;
                let inside = (10.0..=90.0).contains(&x) && o.abs() <= 20.0;
                let slope = if o > 0.0 { 2.0 } else { 4.0 };
                let s = 20.0 + (x - 10.0) * 0.1 - (o.abs() - 5.0).max(0.0) / slope;
                let want = if inside { quantize(s.max(0.0)) } else { 0.0 };
                let got = v[r * 100 + c];
                assert!(
                    (got - want).abs() <= 1.0 / 32.0,
                    "r{r} c{c}: {got} vs {want}"
                );
            }
        }
    }

    #[test]
    fn channel_toes_are_above_the_centre() {
        let mut f = levee(
            vec![[10.0, 50.0], [90.0, 50.0]],
            vec![[0.0, 0.0], [80.0, 0.0]],
        );
        f.elevation_type = ElevationType::SetIfLower;
        let tin = ground_line_tin(&f, GroundLineSubtype::Channel, LengthUnits::Feet).unwrap();
        // Left toe: 15 / 2 above; right toe: 15 / 4 above; centre and ridges 0.
        let zs: Vec<f64> = tin.points.iter().map(|p| p[2]).collect();
        assert!(zs.contains(&7.5) && zs.contains(&3.75));
        assert!(zs.iter().all(|z| [0.0, 7.5, 3.75].contains(z)));
    }

    #[test]
    fn profile_densification_keeps_the_running_start_bug() {
        let line: Vec<Pm> = [[0.0, 0.0], [50.0, 0.0], [300.0, 0.0]]
            .iter()
            .map(|p| Pm::xy(p[0], p[1]))
            .collect();
        let p = levee_profile(&[[5.0, 1.0], [305.0, 4.0]], &line, 300.0, 100.0);
        // Shifted by -5, clamped to 300 (first value per station wins); the
        // 250 ft segment adds 100 and 200 from a start of 0, not 50.
        let st: Vec<f32> = p.iter().map(|q| q.0).collect();
        assert_eq!(st, vec![0.0, 100.0, 200.0, 300.0]);
        assert_eq!(p[1].1, 2.0);
    }

    #[test]
    fn sharp_outer_bend_gets_an_arc_and_inner_loops_are_trimmed() {
        // 90 degree left turn: the right (outer) side gets arc points, the
        // left (inner) side one miter point.
        let s: Vec<Pm> = [[0.0, 0.0], [100.0, 0.0], [100.0, 100.0]]
            .iter()
            .map(|p| Pm::xy(p[0], p[1]))
            .collect();
        let (l, r) = simple_offset(&s, 20.0);
        assert_eq!(l.len(), 3);
        assert_eq!((l[1].x, l[1].y), (90.0, 10.0));
        assert_eq!(l[1].m, 100.0);
        assert!(r.len() > 4, "{r:?}");
        for p in &r[1..r.len() - 1] {
            assert!(((p.x - 100.0).hypot(p.y) - 10.0).abs() < 1e-9);
            assert_eq!(p.m, 100.0);
        }
        // A short inner segment makes the inner offsets miss each other: the
        // arc loop is trimmed away again.
        let s: Vec<Pm> = [[0.0, 0.0], [100.0, 0.0], [100.0, 2.0], [0.0, 2.0]]
            .iter()
            .map(|p| Pm::xy(p[0], p[1]))
            .collect();
        let (l, _) = simple_offset(&s, 20.0);
        for a in 0..l.len() - 1 {
            for b in a + 2..l.len() - 1 {
                let hit = intersect(l[a], l[a + 1], l[b], l[b + 1]);
                assert!(
                    hit.is_none_or(|h| h.3 == 0.0 || h.3 == 1.0),
                    "segments {a} and {b} cross: {l:?}"
                );
            }
        }
    }

    #[test]
    fn last_triangle_wins_within_a_feature() {
        // A U-turn levee overlapping itself: the later leg's (lower) surface
        // replaces the earlier one where they overlap, no max.
        let f = GroundLine {
            elevation_type: ElevationType::SetIfHigher,
            top_width: 4.0,
            left_slope: 1.0,
            right_slope: 1.0,
            max_reach: 20.0,
            points: vec![[10.0, 50.0], [80.0, 50.0], [80.0, 56.0], [10.0, 56.0]],
            profile: vec![[0.0, 30.0], [76.0, 30.0], [146.0, 10.0]],
            system_name: String::new(),
        };
        let g = grid();
        let mut v = vec![0.0_f32; 100 * 100];
        apply(&[gl_layer(GroundLineSubtype::Levee, vec![f])], &g, &mut v).unwrap();
        // Pixel (20.5, 53.5): between the legs; the second leg (z ~ 12.9 at
        // station 135.5 - its ridge) is drawn after the first (z = 30 ridge).
        let at = v[(100 - 54) * 100 + 20];
        assert!(at < 20.0, "{at}");
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

    fn ellipse(rx: f64, ry: f64, rotation: f64) -> Layer {
        Layer::Ellipse {
            name: "e".into(),
            priority: 0,
            features: vec![Ellipse {
                elevation_type: ElevationType::SetValue,
                elevation: 5.0,
                center: [50.5, 50.5],
                radius_x: rx,
                radius_y: ry,
                rotation,
            }],
        }
    }

    #[test]
    fn ellipse_boundary_is_inclusive() {
        let g = grid();
        let at = |v: &[f32], x: f64, y: f64| v[((100.0 - y) as usize) * 100 + x as usize];
        for layer in [ellipse(2.0, 2.0, 0.0), ellipse(2.0, 1.0, 0.0)] {
            let mut v = vec![1.0_f32; 100 * 100];
            apply(&[layer], &g, &mut v).unwrap();
            // centre (52.5, 50.5) is exactly on the boundary: inside.
            assert_eq!(at(&v, 52.5, 50.5), 5.0);
            assert_eq!(at(&v, 53.5, 50.5), 1.0);
        }
        // Rotated 90 degrees: the long axis is vertical.
        let mut v = vec![1.0_f32; 100 * 100];
        apply(&[ellipse(3.0, 1.0, 90.0)], &g, &mut v).unwrap();
        assert_eq!(at(&v, 50.5, 53.5), 5.0);
        assert_eq!(at(&v, 52.5, 50.5), 1.0);
    }

    #[test]
    fn narrow_template_keeps_only_the_crest() {
        // Max Reach < Top Width: the toe ribbons fold inside the ridges, their
        // strips turn clockwise and draw nothing; the crest stays.
        let mut f = levee(
            vec![[10.0, 50.0], [90.0, 50.0]],
            vec![[0.0, 20.0], [80.0, 20.0]],
        );
        f.top_width = 20.0;
        f.max_reach = 10.0;
        let g = grid();
        let mut v = vec![0.0_f32; 100 * 100];
        apply(&[gl_layer(GroundLineSubtype::Levee, vec![f])], &g, &mut v).unwrap();
        for r in 0..100 {
            for c in 0..100 {
                let (x, y) = centre(&g, r, c);
                let crest = (10.0..=90.0).contains(&x) && (y - 50.0).abs() <= 10.0;
                assert_eq!(v[r * 100 + c], if crest { 20.0 } else { 0.0 }, "r{r} c{c}");
            }
        }
    }

    #[test]
    fn unqualified_forms_are_refused() {
        let mut f = levee(vec![[0.0, 0.0], [1.0, 0.0]], vec![[0.0, 0.0], [1.0, 0.0]]);
        f.elevation_type = ElevationType::SetValue;
        let layers = [gl_layer(GroundLineSubtype::Channel, vec![f.clone()])];
        assert!(validate(&layers).is_ok());
        f.system_name = "chan".into();
        let layers = [gl_layer(GroundLineSubtype::Channel, vec![f.clone()])];
        assert!(matches!(validate(&layers), Err(MapError::Unsupported(_))));
        f.system_name.clear();
        f.left_slope = -1.0;
        let layers = [gl_layer(GroundLineSubtype::Channel, vec![f])];
        assert!(matches!(validate(&layers), Err(MapError::Unsupported(_))));
    }
}
