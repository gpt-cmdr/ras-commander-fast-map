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
//!   - Features with `Top Width <= 0` or `Max Reach <= 0`, or a NaN Max
//!     Reach, change nothing; a NaN slope is read as 3, a negative slope is
//!     used as is (the toe moves to the other side). A NaN Top Width with a
//!     finite Max Reach makes RAS Mapper's export fail and is refused.
//!   - *Control points* of the layer within the feature's Elev Pt Tolerance
//!     of the centerline set the profile at the station of their nearest
//!     centerline point (after the stored stations, so they win); with no
//!     stored profile the ends take the elevations of the first and last
//!     such point.
//! * **Named ground lines** (non-empty `System Name`, also a lone feature)
//!   follow RAS Mapper's system path:
//!   - *Joining.* In feature-ID order, an unassigned named feature starts a
//!     system; features with the same name, elevation type and Elev Pt
//!     Tolerance whose end lies within 60 ft of the system's first point are
//!     prepended (nearest first), and those whose start lies within 60 ft of
//!     its last point appended. The centerline is the members' vertices
//!     concatenated (a junction vertex appears twice).
//!   - *Profile and control cross sections.* Member profiles are concatenated:
//!     each member's stations are offset by the last station so far; its
//!     first point is not added to the profile but becomes a control cross
//!     section (station, elevation, Top Width, slopes, Max Reach, Transition
//!     Percent; Max Reach <= 0 is read as 2 x Top Width). Control cross
//!     sections enter the profile before the stored stations, so at a
//!     junction the next feature's start elevation wins. Densification as
//!     for single features (100 ft, or 30.48 m in SI). A last cross section
//!     is added at the end of the system with the last cross section's
//!     template.
//!   - *Sections.* Between consecutive cross sections, cross-section lines
//!     (perpendicular to the chord half a reach up and down the centerline)
//!     carry the trapezoid (toe, ridge, ridge, toe; a template whose Top Width
//!     equals Max Reach is narrowed by 0.01). When the template changes and
//!     Transition Percent is positive, the section ends Top Width x Transition
//!     Percent before the next cross section (that station enters the centre
//!     profile); a NaN Transition Percent adds nothing. Toe and ridge lines
//!     are stream lines grown from the centerline to the cross-section points
//!     in 4-16 lateral steps (resampled, smoothly blended between the
//!     upstream and downstream offsets, Douglas-Peucker filtered, trimmed at
//!     crossings).
//!   - *Elevations.* As for single features, with Max Reach, Top Width and
//!     slopes interpolated linearly by station between cross sections.
//!   - The system is drawn once, in the place of its highest feature ID,
//!     with the five-ribbon TIN and rasterizer above.
//! * **Polygons** with a constant elevation and no control point inside:
//!   pixel centres inside (even-odd). Otherwise (boundary elevations, a
//!   control point inside, single ring) RAS Mapper's perimeter TIN:
//!   - *Perimeter profile.* With boundary elevations on, control points
//!     within the Elev Pt Tolerance of the perimeter give the profile (at
//!     their nearest perimeter station); without such points the stored
//!     perimeter profile is used. With boundary elevations off the profile
//!     is the constant Elevation Value. The profile is filtered (Douglas-
//!     Peucker, 0.01 ft or 0.01 x 0.3048 m, binary32).
//!   - *Perimeter points.* The ring with the profile's stations injected and
//!     z from the profile; vertices before the first or after the last
//!     station interpolate across the ring's start.
//!   - *TIN.* The export TIN's Delaunay factory (tolerance 0.1 ft or 0.1 x
//!     0.3048 m in binary32): perimeter points, then control points inside
//!     the polygon as mass points, the perimeter enforced, triangles outside
//!     deleted; rasterized per output tile, combined over the cells holding
//!     the polygon's extent.
//! * **Ellipses**: output-tile pixel centres inside or on the ellipse (`<= 1`,
//!   rotation in degrees); a circle (equal radii) uses the centre distance
//!   `<= radius`.
//!
//! Combination (per feature, binary32, on the unrounded terrain): a nodata
//! terrain pixel takes the modification value; `SetValue` replaces, `Add`
//! adds, `SetIfHigher` / `SetIfLower` replace when strictly higher / lower.
//! Layers are sorted by ascending `Priority` (stable) and applied in reverse,
//! so Priority 0 is applied last and wins; features in stored order. The
//! caller rounds every value to 1/32 once, after all modifications, as RAS
//! Mapper's TIFF writer does.
//!
//! `hecras_version` 6.6 (measured on HEC-RAS 6.6 exports): ground lines
//! whose Max Reach is not wider than one cell of the output tile are not
//! drawn; named ground lines (6.6 builds systems differently) and boundary
//! elevations without perimeter control points (6.6 samples the terrain) are
//! refused; everything else follows the 7.0.1 rules.
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
    /// Terrain plus the modification value (binary32, unrounded terrain).
    Add,
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
    /// Template values; `null` = NaN (a NaN slope is read as 3, a NaN Max
    /// Reach draws nothing).
    #[serde(deserialize_with = "nan_if_null")]
    pub top_width: f64,
    #[serde(deserialize_with = "nan_if_null")]
    pub left_slope: f64,
    #[serde(deserialize_with = "nan_if_null")]
    pub right_slope: f64,
    #[serde(deserialize_with = "nan_if_null")]
    pub max_reach: f64,
    /// Centerline vertices `[x, y]` (one part).
    pub points: Vec<[f64; 2]>,
    /// Stored `[station, elevation]` pairs.
    pub profile: Vec<[f64; 2]>,
    /// The feature's `System Name` attribute (optional). Named features are
    /// joined into systems and built through RAS Mapper's system path.
    #[serde(default)]
    pub system_name: String,
    /// `Transition Percent` (optional, default 0; `null` = NaN). Used by
    /// named features only.
    #[serde(default, deserialize_with = "nan_if_null")]
    pub transition_percent: f64,
    /// `Elev Pt Tolerance` (optional, default 0; `null` = NaN). Named
    /// features join only with equal tolerances.
    #[serde(default, deserialize_with = "nan_if_null")]
    pub elev_pt_tolerance: f64,
}

fn nan_if_null<'de, D: serde::Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    Ok(Option::<f64>::deserialize(d)?.unwrap_or(f64::NAN))
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Polygon {
    pub elevation_type: ElevationType,
    pub elevation: f64,
    /// Rings of `[x, y]`; inside is even-odd over all rings.
    pub rings: Vec<Vec<[f64; 2]>>,
    /// `Generate Boundary Elevations` (optional, default false).
    #[serde(default)]
    pub generate_boundary: bool,
    /// `Use ShapeFile Z Elevations` (optional, default false).
    #[serde(default)]
    pub use_shape_z: bool,
    /// Stored perimeter profile `[station, elevation]` (optional).
    #[serde(default)]
    pub profile: Vec<[f64; 2]>,
    /// Vertex z of the (single) ring, one per ring vertex (shape Z).
    #[serde(default)]
    pub vertex_z: Vec<f64>,
    /// `Elev Pt Tolerance` (optional, default 0; `null` = NaN).
    #[serde(default, deserialize_with = "nan_if_null")]
    pub elev_pt_tolerance: f64,
}

/// A point of a layer's `Control Points` group.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlPoint {
    pub x: f64,
    pub y: f64,
    /// `null` = NaN (ignored, as RAS Mapper does).
    #[serde(deserialize_with = "nan_if_null")]
    pub elevation: f64,
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
        /// The layer's control points (optional).
        #[serde(default)]
        control_points: Vec<ControlPoint>,
    },
    Polygon {
        name: String,
        priority: i64,
        features: Vec<Polygon>,
        /// The layer's control points (optional).
        #[serde(default)]
        control_points: Vec<ControlPoint>,
        /// Default `feet` (sets the 0.01 / 0.1 profile and TIN tolerances).
        #[serde(default)]
        length_units: LengthUnits,
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
            "ground_line": {"subtypes": ["levee", "channel"], "elevation_types": ["SetValue", "SetIfHigher", "SetIfLower", "Add"],
                             "requires": "one part, a stored profile of >= 2 stations unless the layer has control points; named features: finite template and no layer control points",
                             "control_points": "layer control_points within the feature's elev_pt_tolerance of an unnamed centerline set the profile",
                             "nan_template": "NaN Max Reach draws nothing, NaN slope read as 3; NaN Top Width with a finite Max Reach refused (RAS Mapper's export fails)",
                             "named": "non-empty system_name: joined systems through RAS Mapper's system path (transition_percent, elev_pt_tolerance optional; null = NaN)",
                             "length_units": "feet (default) or meters: profile densification every 100 ft (single features 100/3.281 m, systems 30.48 m)"},
            "polygon": {"elevation_types": ["SetValue", "SetIfHigher", "SetIfLower", "Add"],
                        "constant": "no boundary elevations and no control point inside: constant fill",
                        "tin": "boundary elevations from perimeter control points or a stored perimeter profile, interior control points as mass points; one ring"},
            "ellipse": {"elevation_types": ["SetValue", "SetIfHigher", "SetIfLower"], "rotation": "degrees, any"}
        },
        "ground_line_rule": "profile: binary32 stations shifted to start at 0, clamped to the length, first value per station, 100 ft (100/3.281 m) densification from a running start that only advances over segments longer than the spacing, length appended when the last station is > 0.01 short; binary32 linear query clamped at both ends. Offset lines: toes at Max Reach/2, ridges at Top Width/2, M = centerline station; joints meet at the offset-segment intersection (M averaged) or get the previous end, an arc split while chord-mid/r < 0.95, and the next start; square ends; self-intersections trimmed (farthest crossing later segment, M averaged); profile stations injected by M fraction. Centre and ridges z = profile(station); toes z = profile -/+ (Max Reach/2 - Top Width/2)/slope (binary32 half-widths, binary64 product), slope 0 read as 0.001; Max Reach <= Top Width folds the toes inside the ridges (those strips draw nothing). Ribbon TIN (left toe, left ridge, centre, right ridge, right toe; 0.01 dedupe; shorter diagonal, ties advance the right ribbon; fan at the end), rasterized per 256-pixel output tile with the export TIN rasterizer (inclusive, last triangle wins, clockwise triangles draw nothing). Top Width <= 0 or Max Reach <= 0: no change",
        "system_rule": "named features joined in feature-ID order (same name, elevation type and Elev Pt Tolerance; end-to-start within 60 ft; nearest first), centerline = members' vertices concatenated; profile = member profiles offset by the running last station, each member's first point a control cross section (Max Reach <= 0 read as 2 x Top Width) that enters the centre profile first (the next feature's start elevation wins at a junction); a closing cross section at the system end; per section, cross-section lines perpendicular to the chord half a reach up and down the centerline carry the trapezoid (Top Width = Max Reach narrowed by 0.01), a changing template with positive Transition Percent ends its section Top Width x Transition Percent early (NaN adds nothing); toe and ridge stream lines grown in 4-16 lateral steps (resample, smooth blend, Douglas-Peucker, crossing trims), M-fraction injection, template values interpolated by station; same ribbon TIN and rasterizer; drawn at the highest member feature ID",
        "polygon_rule": "constant: pixel centre inside (even-odd). TIN: perimeter profile from control points within Elev Pt Tolerance of the perimeter (boundary elevations on), else the stored profile (on), else the constant Elevation Value; Douglas-Peucker filtered at 0.01 ft (binary32); perimeter points with z from the profile (wrapping across the ring start); Delaunay factory of the export TIN at 0.1 ft (binary32 widened), extent of perimeter and mass points (10 x tolerance, 1:8) widened to 3 x the perimeter's, perimeter then interior control points inserted, perimeter enforced, triangles touching the super polygon deleted and the deletion flooded across non-perimeter edges; rasterized per output tile over the cells holding the polygon's extent",
        "ellipse_rule": "per output-tile pixel centre: circle (equal radii) distance <= radius; otherwise (c dx + s dy)^2/rx^2 + (s dx - c dy)^2/ry^2 <= 1 with c, s = cos, sin(-rotation degrees)",
        "combine": "binary32 on the unrounded terrain (the mosaic and export TIN hold binary32 values; 1/32 rounding once after all modifications): a nodata terrain pixel takes the modification value; SetValue replaces; Add adds; SetIfHigher/SetIfLower replace when strictly higher/lower; layers stable-sorted by ascending Priority and applied in reverse (Priority 0 last, so it wins), features in stored order",
        "rounding": "binary32 throughout, then nearest 1/32 (ties to even) once at the end",
        "hecras_version": "7.0.1 (default) or 6.6. 6.6 (measured on HEC-RAS 6.6 exports of the synthetic cases): ground lines with Max Reach not wider than one output-tile cell are not drawn; named ground lines and boundary elevations without perimeter control points are refused (6.6 builds systems differently and samples the terrain); all else as 7.0.1",
        "qualified": {
            "scope": "fast-map 0.16.0 working tree, ras2fim-batch effective-terrain, request schema 2, HEC-RAS 7.0.1 exports with rasterize_modifications=True, downsample_factor=1, whole terrain",
            "real terrains A and B1-B4 (pixels where two exports differ excluded; unnamed ground lines, constant polygons, circles)": "no difference inside any modification footprint: B1 0 values (was 3 at 1/32 ft), B2 0 values (was 443, max 1.47 ft), B3 and B4 0; the remaining pixels (A 322 values, max 0.53 ft; B1, B2 and B3 2 mask pixels each) lie outside every footprint, in export-TIN gap fills",
            "real terrain C, previous round (2 ft, 379.0M px in the source extent; HDF copy reduced to its 10 then-accepted layers, including systems of 3, 2 and lone named channels and a SetValue channel layer; two base and two modified exports, which agree)": "0 mask and 4 value differences, all at 1/32 ft; per layer 0 except 1 pixel at 1/32 ft in the SetValue channel layer",
            "synthetic T2 terrain, 20 ft (one export each)": "named writer cases (22.5 km channel, 21.2 km levee, channel + polygon + levee): 0 modified pixels differ (2 edge pixels differ in the base export); three joined channels with differing templates and profiles, a 20 ft junction gap and Transition Percent 0 / 0.5, a lone channel with NaN Transition Percent and an unnamed channel: 1 of 7,118 pixels at 1/32 ft; levee systems split by elevation type (Transition Percent 1, a profile starting at station 10, SetValue members): 0 of 2,494; a SetValue channel system in scrambled feature order: 0 of 4,567; blank-name cases (sharp bends, short segments, SetValue, Max Reach <= Top Width, ellipses) as before",
            "control points, boundary elevations, Add, NaN templates": "real terrain C, all 13 layers (2 ft, 379.0M px): 0 mask, 4 values at 1/32 ft; real terrain E without its terrain-sampled polygon layer (3 ft, 1.14G px; a 14-feature levee layer with 2,862 control points): 0 mask, 3 values at 1/32 ft; real terrain D without its terrain-sampled polygon layer (228 layers, 924.6M px; 202 boundary-elevation polygon layers with perimeter control points, 20 channel layers with control points): 0 mask, 3 values at 1/32 ft; synthetic polygons (interior points, stored profile, perimeter points, one point, Add): 2 of 12,805 px at 1/32 ft; synthetic ground lines with control points and Add: 1 of 2,499 px at 1/32 ft; NaN and negative templates: exact",
            "hecras_version 6.6": "HEC-RAS 6.6 exports of the synthetic cases: narrow ground lines 474 of 474 px, bends, short segments, SetValue, narrow templates, ellipses, control points, NaN templates, constant polygons: at most 2 px at 1/32 ft per case"
        },
        "known_differences": "RasMapperLib's binary64 powers (.NET Framework Math.Pow, e.g. distances as Pow(x, 0.5)) are not correctly rounded; this engine uses correctly rounded square roots and products. On terrain C's 3-feature system this moves 4 stream-line points by up to 6e-5 ft; no pixel changed in the measured cases",
        "refused": "boundary elevations sampled from the terrain (no stored profile and no perimeter control point: RAS Mapper samples each source raster and the stored stitch TIN), shape-Z polygons (vertex z storage not measured), multi-part polygons needing a TIN, named ground lines with a NaN template or in a layer with control points, a NaN Top Width with a finite Max Reach, multi-part features, any other modification type (the caller also refuses duplicate priorities)"
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
            Layer::GroundLine {
                features,
                name,
                control_points,
                ..
            } => {
                let with_cps = !control_points.is_empty();
                if control_points
                    .iter()
                    .any(|c| !(finite(c.x) && finite(c.y)) || c.elevation.is_infinite())
                {
                    return Err(invalid(format!(
                        "ground line layer {name:?}: non-finite control point"
                    )));
                }
                for f in features {
                    // With control points RAS Mapper builds the profile from
                    // them, so a stored profile may be empty.
                    if f.points.len() < 2 || (!with_cps && f.profile.len() < 2) {
                        return Err(invalid(format!(
                            "ground line layer {name:?}: needs >= 2 points and >= 2 profile stations"
                        )));
                    }
                    if with_cps && !f.system_name.trim().is_empty() {
                        return Err(MapError::Unsupported(format!(
                            "ground line layer {name:?}: named ground lines with control points are not qualified"
                        )));
                    }
                    let template = [f.top_width, f.left_slope, f.right_slope, f.max_reach];
                    let ok = template.iter().all(|v| !v.is_infinite())
                        && f.points.iter().flatten().all(|v| finite(*v))
                        && f.profile.iter().flatten().all(|v| finite(*v));
                    if !ok {
                        return Err(invalid(format!(
                            "ground line layer {name:?}: non-finite geometry, template or profile"
                        )));
                    }
                    let named = !f.system_name.trim().is_empty();
                    if named && template.iter().any(|v| v.is_nan()) {
                        return Err(MapError::Unsupported(format!(
                            "ground line layer {name:?}: named ground lines with a NaN template are not qualified"
                        )));
                    }
                    // Measured on HEC-RAS 7.0.1: a NaN Top Width with a finite
                    // Max Reach makes the export itself fail.
                    if f.top_width.is_nan() && !f.max_reach.is_nan() {
                        return Err(MapError::Unsupported(format!(
                            "ground line layer {name:?}: a NaN Top Width with a finite Max Reach fails in RAS Mapper's export"
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
#[cfg(test)]
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

/// RAS Mapper's per-pixel replacement in binary32 on the unrounded terrain
/// (the caller rounds to 1/32 once, after every modification).
fn combine(base: f32, m: f32, et: ElevationType) -> f32 {
    if base == NODATA {
        return m;
    }
    if m == NODATA {
        return base;
    }
    match et {
        ElevationType::SetValue => m,
        ElevationType::Add => base + m,
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
fn levee_profile(
    stored: &[[f64; 2]],
    line: &[Pm],
    len: f64,
    spacing: f64,
    cps: Option<(&[ControlPoint], f32)>,
) -> Profile {
    let stored: Profile = stored.iter().map(|s| (s[0] as f32, s[1] as f32)).collect();
    seeded_profile(&[], &stored, line, len, spacing, cps)
}

/// `seed` stations (control cross sections) are entered first; then the
/// stored stations, shifted so the first is 0 and clamped to the length; the
/// first value per station wins. Then the long-segment densification and the
/// appended length.
fn seeded_profile(
    seed: &[(f32, f32)],
    stored: &[(f32, f32)],
    line: &[Pm],
    len: f64,
    spacing: f64,
    cps: Option<(&[ControlPoint], f32)>,
) -> Profile {
    let mut keys: Profile = Vec::with_capacity(seed.len() + stored.len());
    let put = |keys: &mut Profile, x: f32, y: f32| {
        if !keys.iter().any(|k| k.0 == x) {
            keys.push((x, y));
        }
    };
    for &(x, y) in seed {
        put(&mut keys, x, y);
    }
    if let Some(&(first, _)) = stored.first() {
        let shift = if first != 0.0 { -first } else { 0.0 };
        for &(x0, y) in stored {
            let x = if x0 as f64 > len {
                len as f32
            } else {
                x0 + shift
            };
            put(&mut keys, x, y);
        }
    }
    if let Some((cps, tol)) = cps {
        control_point_stations(&mut keys, stored.is_empty(), cps, tol, line, len);
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
    cps: &[ControlPoint],
) -> Option<CleanTin> {
    let reach = f.max_reach as f32;
    let top = f.top_width as f32;
    // RAS Mapper's `<= 0` skips; a NaN Max Reach draws nothing (measured).
    if reach.is_nan() || reach <= 0.0 || top <= 0.0 {
        return None;
    }
    let line: Vec<Pm> = f.points.iter().map(|p| Pm::xy(p[0], p[1])).collect();
    let total = length(&line);
    let cp = if cps.is_empty() {
        None
    } else {
        Some((cps, f.elev_pt_tolerance as f32))
    };
    let prof = levee_profile(&f.profile, &line, total, units.profile_spacing(), cp);
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
        if s.is_nan() {
            3.0_f32
        } else if s == 0.0 {
            0.001_f32
        } else {
            s
        }
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
// Named ground lines (RAS Mapper's system path)
// ---------------------------------------------------------------------------

/// Joining radius between a feature end and the next feature's start.
const SYSTEM_SEARCH_RADIUS: f64 = 60.0;

/// A position on a polyline: point, segment index and fraction.
#[derive(Clone, Copy, Debug)]
struct Pop {
    p: Pm,
    seg: usize,
    frac: f64,
}

fn pop_less(a: &Pop, b: &Pop) -> bool {
    a.seg < b.seg || (a.seg == b.seg && a.frac < b.frac)
}

fn first_pop(p: &[Pm]) -> Pop {
    Pop {
        p: p[0],
        seg: 0,
        frac: 0.0,
    }
}

fn last_pop(p: &[Pm]) -> Pop {
    Pop {
        p: p[p.len() - 1],
        seg: p.len().saturating_sub(2),
        frac: 1.0,
    }
}

/// Position at distance `d` from the start (clamped to the ends).
fn pop_at_distance(p: &[Pm], total: f64, d: f64) -> Pop {
    if d <= 0.0 {
        return first_pop(p);
    }
    if d >= total {
        return last_pop(p);
    }
    let mut sum = 0.0;
    for i in 0..p.len() - 1 {
        let start = sum;
        let l = dist(p[i], p[i + 1]);
        sum += l;
        if sum >= d {
            let t = (d - start) / l;
            return Pop {
                p: lerp(p[i], p[i + 1], t),
                seg: i,
                frac: t,
            };
        }
    }
    last_pop(p)
}

/// Position at a fraction of the length (point-list variant).
fn pop_at_fraction(p: &[Pm], f: f64) -> Pop {
    if f <= 0.0 {
        return first_pop(p);
    }
    if f >= 1.0 {
        return last_pop(p);
    }
    let target = length(p) * f.clamp(0.0, 1.0);
    if target == 0.0 {
        return first_pop(p);
    }
    let mut sum = 0.0;
    for i in 0..p.len() - 1 {
        let start = sum;
        let l = dist(p[i], p[i + 1]);
        sum += l;
        if sum >= target {
            let t = (target - start) / l;
            return Pop {
                p: lerp(p[i], p[i + 1], t),
                seg: i,
                frac: t,
            };
        }
    }
    last_pop(p)
}

/// Start point, the vertices after it up to the end segment, and the end
/// point unless it sits at the start of its segment.
fn point_subsection(p: &[Pm], a: &Pop, b: &Pop) -> Vec<Pm> {
    let mut out = vec![a.p];
    for v in p.iter().take(b.seg + 1).skip(a.seg + 1) {
        out.push(*v);
    }
    if b.frac > 0.0 {
        out.push(b.p);
    }
    out
}

fn is_ring(p: &[Pm], tol: f64) -> bool {
    p.len() >= 2 && {
        let (a, b) = (p[0], p[p.len() - 1]);
        (a.x - b.x).abs() < tol && (a.y - b.y).abs() < tol
    }
}

/// Infinite-line intersection, as a point on the first line.
fn line_point(a0: Pm, a1: Pm, b0: Pm, b1: Pm) -> (f64, f64) {
    let (dx, dy) = (a1.x - a0.x, a1.y - a0.y);
    let (ex, ey) = (b1.x - b0.x, b1.y - b0.y);
    let den = ey * dx - ex * dy;
    if den == 0.0 {
        return (0.0, 0.0);
    }
    let (oy, ox) = (a0.y - b0.y, a0.x - b0.x);
    let t1 = (ex * oy - ey * ox) / den;
    (a0.x + t1 * dx, a0.y + t1 * dy)
}

/// A trapezoidal control cross section of a system.
#[derive(Clone, Copy, Debug)]
struct ControlXs {
    station: f32,
    elevation: f32,
    width: f32,
    left_slope: f32,
    right_slope: f32,
    reach: f32,
    transition: f32,
}

/// Cross-section line across `center` at `station`: perpendicular to the
/// chord between the points half a reach up and down the centerline,
/// centred on the centerline point.
fn xs_line(center: &[Pm], station: f32, width: f32, reach: f32) -> Vec<Pm> {
    let mut num = reach;
    if num.is_nan() || num < width {
        num = width;
    }
    if num.is_nan() {
        num = 100.0;
    }
    let total = length(center);
    let at = pop_at_distance(center, total, station as f64);
    let half = num as f64 / 2.0;
    let (mut up, mut down) = (station as f64 + half, station as f64 - half);
    if is_ring(center, half) {
        if up > total {
            up -= total;
        }
        if down < 0.0 {
            down += total;
        }
    }
    let b = pop_at_distance(center, total, up).p;
    let a = pop_at_distance(center, total, down).p;
    let mid = lerp(a, b, 0.5);
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let (hx, hy) = (dx * 0.5, dy * 0.5);
    let mut p0 = Pm::xy(mid.x - hy, mid.y + hx);
    let mut p1 = Pm::xy(mid.x + hy, mid.y - hx);
    let w = (num / 2.0) as f64;
    // Offset chords at +w and -w.
    let (nx, ny) = left_normal(a, b);
    let off = |d: f64| {
        (
            Pm::xy(a.x + nx * d, a.y + ny * d),
            Pm::xy(b.x + nx * d, b.y + ny * d),
        )
    };
    let (s1, s2) = (off(w), off(-w));
    // Extend the perpendicular by w / 2 at each end.
    let l = dist(p0, p1);
    let (ux, uy) = ((p1.x - p0.x) / l, (p1.y - p0.y) / l);
    let e = w / 2.0;
    (p0, p1) = (
        Pm::xy(p0.x - ux * e, p0.y - uy * e),
        Pm::xy(p1.x + ux * e, p1.y + uy * e),
    );
    let (x, y) = line_point(p0, p1, s1.0, s1.1);
    let (x2, y2) = line_point(p0, p1, s2.0, s2.1);
    let (tx, ty) = (at.p.x - mid.x, at.p.y - mid.y);
    vec![Pm::xy(x + tx, y + ty), Pm::xy(x2 + tx, y2 + ty)]
}

/// Cross-section profile: left toe, left ridge, right ridge, right toe.
fn xs_profile(
    subtype: GroundLineSubtype,
    elevation: f32,
    width: f32,
    ls: f32,
    rs: f32,
    reach: f32,
) -> Profile {
    let mut w = width;
    if w == reach && w != 0.0 {
        let mut k = 2;
        loop {
            w = (width as f64 - 10f64.powi(-k)) as f32;
            if w > 0.0 {
                break;
            }
            k += 1;
        }
    }
    let (mut l, mut r) = (ls, rs);
    match subtype {
        GroundLineSubtype::Channel => l = -l,
        GroundLineSubtype::Levee => r = -r,
    }
    let half = reach / 2.0;
    let run = half - w / 2.0;
    let toe = elevation - run / l;
    let ridge = toe + run / l;
    let x1 = run;
    let x2 = x1 + w;
    vec![
        (0.0, toe),
        (x1, ridge),
        (x2, ridge),
        (2.0 * half, ridge + run / r),
    ]
}

/// Polyline with z from `prof` by station; interior profile stations are
/// injected. `m_station`: M = station (else station / `total`).
fn z_from_profile(line: &[Pm], prof: &[(f32, f32)], total: f64, m_station: bool) -> Vec<Pm> {
    if prof.len() < 2 {
        return line.to_vec();
    }
    let mval = |s: f32| {
        if m_station {
            s as f64
        } else {
            s as f64 / total
        }
    };
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
                m: mval(s),
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
                m: mval(prof[k].0),
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

/// M = cumulative binary32 station / length.
fn m_fraction(line: &[Pm]) -> Vec<Pm> {
    let total = length(line);
    let mut s = 0.0_f32;
    let mut out = Vec::with_capacity(line.len());
    for (i, p) in line.iter().enumerate() {
        if i > 0 {
            s += dist(line[i - 1], *p) as f32;
        }
        out.push(Pm {
            m: s as f64 / total,
            ..*p
        });
    }
    out
}

/// Crossings of `xs` (by its segment order) with the centerline:
/// `(point, centerline segment, fraction, xs segment, fraction)`.
fn xs_crossings(center: &[Pm], xs: &[Pm]) -> Vec<(f64, f64, usize, f64, usize, f64)> {
    let mut out: Vec<(f64, f64, usize, f64, usize, f64)> = Vec::new();
    for i in 0..xs.len().saturating_sub(1) {
        for c in 0..center.len() - 1 {
            if let Some((x, y, t1, t2)) = intersect(center[c], center[c + 1], xs[i], xs[i + 1]) {
                if out.last().is_some_and(|l| l.0 == x && l.1 == y) {
                    continue;
                }
                out.push((x, y, c, t1, i, t2));
            }
        }
    }
    out
}

fn compute_length(p: &[Pm]) -> f64 {
    let mut s = 0.0;
    for i in 1..p.len() {
        s += dist(p[i], p[i - 1]);
    }
    s
}

/// Douglas-Peucker filter keeping the first and last points.
fn filter_xy(p: &mut Vec<Pm>, tol: f64) {
    if tol.is_nan() || p.len() <= 2 {
        return;
    }
    let last = p.len() - 1;
    let mut keep = vec![0, last];
    let mut last_unique = last;
    for j in (0..=last).rev() {
        if !(p[0].x == p[j].x && p[0].y == p[j].y) {
            last_unique = j;
            break;
        }
    }
    let tol = tol.max(1e-12);
    fn dp(p: &[Pm], first: usize, lu: usize, tl: usize, tol: f64, keep: &mut Vec<usize>) {
        if tl - first <= 1 {
            return;
        }
        let (mut best, mut at) = (0.0, 0);
        let (ax, ay) = (p[lu].x - p[first].x, p[lu].y - p[first].y);
        for (i, q) in p.iter().enumerate().take(tl + 1).skip(first + 1) {
            let v = (ax * (q.y - p[first].y) - ay * (q.x - p[first].x)).abs();
            if v > best {
                best = v;
                at = i;
            }
        }
        let n = (ax.powf(2.0) + ay.powf(2.0)).sqrt();
        if best / n > tol {
            keep.push(at);
            dp(p, first, at, at, tol, keep);
            dp(p, at, lu, tl, tol, keep);
        }
    }
    dp(p, 0, last_unique, last, tol, &mut keep);
    keep.sort_unstable();
    keep.dedup();
    if keep.len() >= p.len() {
        return;
    }
    let kept: Vec<Pm> = keep.iter().map(|&i| p[i]).collect();
    *p = kept;
}

/// Even resampling at about `spacing`, keeping both ends.
fn resample(p: &[Pm], spacing: f64) -> Vec<Pm> {
    if p.is_empty() {
        return Vec::new();
    }
    let total = compute_length(p);
    if p.len() == 1 || total == 0.0 {
        return vec![p[0]];
    }
    let n = (total / spacing).ceil() as usize;
    let step = total / n as f64;
    let mut out = vec![p[0]];
    let (mut at, mut start) = (step, 0.0);
    for i in 0..p.len() - 1 {
        let l = dist(p[i], p[i + 1]);
        while at <= start + l {
            if out.len() < n {
                out.push(lerp(p[i], p[i + 1], (at - start) / l));
            }
            at += step;
        }
        start += l;
    }
    out.push(p[p.len() - 1]);
    out
}

fn close_xy(a: Pm, b: Pm, d: f64) -> bool {
    export_tin::close_to(a.x, a.y, b.x, b.y, d)
}

/// Distance from `q` to segment `a b`.
fn seg_distance(a: Pm, b: Pm, q: Pm) -> f64 {
    let l = dist(a, b);
    let to = |u: Pm| ((u.x - q.x).powi(2) + (u.y - q.y).powi(2)).sqrt();
    if l == 0.0 {
        return to(a);
    }
    let along = ((b.x - a.x) * (q.x - a.x) + (b.y - a.y) * (q.y - a.y)) / l;
    if along < 0.0 {
        return to(a);
    }
    if along > l {
        return to(b);
    }
    let h = to(a);
    let v = h * h - along * along;
    if v > 0.0 { v.sqrt() } else { 0.0 }
}

/// Appends `q`, replacing the last point when `q` is within `tol` of it or
/// the last point is within `tol` of the new last segment.
fn push_colinear(v: &mut Vec<Pm>, q: Pm, tol: f64) {
    if v.len() <= 1 {
        v.push(q);
        return;
    }
    let n = v.len();
    if close_xy(v[n - 1], q, tol) || seg_distance(v[n - 2], q, v[n - 1]) < tol {
        v[n - 1] = q;
    } else {
        v.push(q);
    }
}

fn trim_beginning(v: &mut Vec<Pm>, at: &Pop) {
    if v.len() <= 1 {
        return;
    }
    if at.seg == 0 {
        if at.frac > 0.0 {
            v[0] = at.p;
        }
    } else {
        v[0] = at.p;
        v.drain(1..1 + at.seg);
    }
}

fn trim_end(v: &mut Vec<Pm>, at: &Pop) {
    if at.seg + 2 == v.len() {
        if at.frac < 1.0 {
            let n = v.len();
            v[n - 1] = at.p;
        }
    } else {
        v.truncate(at.seg + 1);
        v.push(at.p);
    }
}

/// All crossings of `a` with `b`: `(A position on a, B position on b)`.
fn crossings(a: &[Pm], b: &[Pm]) -> Vec<(Pop, Pop)> {
    let mut out = Vec::new();
    for i in 0..a.len().saturating_sub(1) {
        for j in 0..b.len().saturating_sub(1) {
            if let Some((x, y, t1, t2)) = intersect(a[i], a[i + 1], b[j], b[j + 1]) {
                let p = Pm::xy(x, y);
                out.push((
                    Pop {
                        p,
                        seg: i,
                        frac: t1,
                    },
                    Pop {
                        p,
                        seg: j,
                        frac: t2,
                    },
                ));
            }
        }
    }
    out
}

fn last_by_b(mut v: Vec<(Pop, Pop)>) -> (Pop, Pop) {
    v.sort_by(|x, y| {
        (x.1.seg, x.1.frac)
            .partial_cmp(&(y.1.seg, y.1.frac))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    v[v.len() - 1]
}

/// Stream line between two cross-section parts: the centerline part is
/// moved laterally in steps towards the cross-section points, smoothly from
/// the upstream to the downstream offset.
fn stream_offset(stream: &[Pm], usxs: &[Pm], dsxs: &[Pm]) -> Vec<Pm> {
    let mut st = stream.to_vec();
    let mut us = usxs.to_vec();
    let mut ds = dsxs.to_vec();
    if us.is_empty() {
        us.push(st[0]);
    }
    if ds.is_empty() {
        ds.push(st[st.len() - 1]);
    }
    let n = compute_length(&st);
    let (n2, n3) = (compute_length(&us), compute_length(&ds));
    let mean = 0.5 * (n2 + n3);
    if n2 < 0.1 && n3 < 0.1 {
        return st;
    }
    let mut steps = 4_usize;
    let mut cnt = (n / (mean / steps as f64)).round_ties_even();
    while !(cnt >= 8.0 || steps == 16) {
        steps += 1;
        cnt = (n / (mean / steps as f64)).round_ties_even();
    }
    let tol = (mean / 64.0).max(0.001);
    filter_xy(&mut st, tol);
    filter_xy(&mut us, tol);
    filter_xy(&mut ds, tol);
    let mut offset = Vec::new();
    for i in 1..=steps {
        let f = 1.0 / (steps - i + 1) as f64;
        let pu = pop_at_fraction(&us, f);
        let pd = pop_at_fraction(&ds, f);
        let d0 = dist(pu.p, st[0]);
        let d1 = dist(pd.p, st[st.len() - 1]);
        let rs = resample(&st, (d0 + d1) * 0.5);
        let (mut a, mut b) = (d0, d1);
        if rs.len() >= 2 {
            let k = rs.len();
            a = perp_signed(rs[0], rs[1], pu.p);
            b = perp_signed(rs[k - 2], rs[k - 1], pd.p);
        }
        let delta = b - a;
        offset.clear();
        offset.push(pu.p);
        let k = rs.len() as i64;
        for j in 2..=(k - 3) {
            let (s0, s1) = (rs[(j - 1) as usize], rs[(j + 1) as usize]);
            let fa = j as f64 / k as f64;
            let factor = a + delta * (-2.0 * fa.powf(3.0) + 3.0 * fa.powf(2.0));
            let (nx, ny) = left_normal(s0, s1);
            let mid = lerp(s0, s1, 0.5);
            let q = Pm {
                x: mid.x + nx * factor,
                y: mid.y + ny * factor,
                z: mid.z + 0.0 * factor,
                m: mid.m + 0.0 * factor,
            };
            push_colinear(&mut offset, q, 1e-6);
        }
        push_colinear(&mut offset, pd.p, 1e-6);
        trim_beginning(&mut us, &pu);
        trim_beginning(&mut ds, &pd);
        trim_self_intersections(&mut offset);
        let hits = crossings(&offset, &us);
        if hits.len() > 1 {
            let (pa, pb) = last_by_b(hits);
            trim_beginning(&mut offset, &pa);
            trim_beginning(&mut us, &pb);
        }
        let hits = crossings(&offset, &ds);
        if hits.len() > 1 {
            let (pa, pb) = last_by_b(hits);
            trim_end(&mut offset, &pa);
            trim_beginning(&mut ds, &pb);
        }
        if i == steps {
            filter_xy(&mut offset, tol);
        } else {
            st = offset.clone();
        }
    }
    offset
}

fn perp_signed(a: Pm, b: Pm, p: Pm) -> f64 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let l = (dx * dx + dy * dy).sqrt();
    if l == 0.0 {
        return ((a.x - p.x).powi(2) + (a.y - p.y).powi(2)).sqrt();
    }
    ((0.0 - (p.x - a.x)) * dy + (p.y - a.y) * dx) / l
}

/// Offset between the upstream and downstream cross sections towards the
/// given cross-section points.
#[allow(clippy::too_many_arguments)]
fn section_offset(
    stream: &[Pm],
    us_at: &Pop,
    ds_at: &Pop,
    us_line: &[Pm],
    us_pt: &Pop,
    ds_line: &[Pm],
    ds_pt: &Pop,
) -> Vec<Pm> {
    let part = |line: &[Pm], at: &Pop, pt: &Pop| {
        if pop_less(at, pt) {
            point_subsection(line, at, pt)
        } else {
            let mut v = point_subsection(line, pt, at);
            v.reverse();
            v
        }
    };
    stream_offset(
        stream,
        &part(us_line, us_at, us_pt),
        &part(ds_line, ds_at, ds_pt),
    )
}

/// Missing or zero values: zeros become `zero_as`; NaNs take the previous
/// value, leading NaNs the first value (or `default`).
#[allow(clippy::needless_range_loop)]
fn fill_values(v: &mut [f32], default: f32, zero_as: f32) {
    let f = |x: f32| if x == 0.0 { zero_as } else { x };
    let mut prev = f32::NAN;
    let mut found = false;
    for i in 0..v.len() {
        v[i] = f(v[i]);
        if v[i].is_nan() && prev.is_nan() {
            for j in i + 1..v.len() {
                v[j] = f(v[j]);
                if !v[j].is_nan() {
                    prev = v[j];
                    found = true;
                    break;
                }
            }
            if !found {
                prev = default;
            }
            v[i] = prev;
        } else if v[i].is_nan() {
            v[i] = prev;
        } else {
            found = true;
            prev = v[i];
        }
    }
}

/// One system: its centerline, the joined profile and the control cross
/// sections (one per feature, at the feature's first station).
struct System {
    line: Vec<Pm>,
    profile: Profile,
    xs: Vec<ControlXs>,
}

fn build_system(features: &[&GroundLine]) -> System {
    let mut profile: Profile = Vec::new();
    let mut xs = Vec::new();
    let mut line = Vec::new();
    for f in features {
        let prof: Profile = f
            .profile
            .iter()
            .map(|p| (p[0] as f32, p[1] as f32))
            .collect();
        if profile.is_empty() {
            profile.push(prof[0]);
        }
        let base = profile[profile.len() - 1].0;
        let top = f.top_width as f32;
        let reach = f.max_reach as f32;
        xs.push(ControlXs {
            station: base + prof[0].0,
            elevation: prof[0].1,
            width: top,
            left_slope: f.left_slope as f32,
            right_slope: f.right_slope as f32,
            reach: if reach.is_nan() || reach <= 0.0 {
                top * 2.0
            } else {
                reach
            },
            transition: f.transition_percent as f32,
        });
        for p in &prof[1..] {
            profile.push((base + p.0, p.1));
        }
        line.extend(f.points.iter().map(|p| Pm::xy(p[0], p[1])));
    }
    System { line, profile, xs }
}

/// The five ribbons of a system (left toe, left ridge, centre, right ridge,
/// right toe), or `None` where RAS Mapper builds no TIN.
// Negated comparisons keep RAS Mapper's NaN behaviour (a NaN Transition
// Percent passes `!(tp <= 0)`).
#[allow(clippy::neg_cmp_op_on_partial_ord)]
fn system_ribbons(
    sys: &System,
    subtype: GroundLineSubtype,
    units: LengthUnits,
) -> Option<[Vec<Pm>; 5]> {
    let line = &sys.line;
    if line.len() < 2 {
        return None;
    }
    let total = length(line);
    let spacing = match units {
        LengthUnits::Feet => 100.0,
        LengthUnits::Meters => 100.0 * 0.3048,
    };
    let seed: Profile = sys.xs.iter().map(|x| (x.station, x.elevation)).collect();
    let profile = seeded_profile(&seed, &sys.profile, line, total, spacing, None);
    // Control cross sections by station (stable), lines at their stations.
    let mut xs: Vec<(ControlXs, Vec<Pm>)> = sys
        .xs
        .iter()
        .map(|x| {
            let l = if x.reach > 0.0 {
                xs_line(line, x.station, x.width, x.reach)
            } else {
                Vec::new()
            };
            (*x, l)
        })
        .collect();
    xs.sort_by(|a, b| {
        a.0.station
            .partial_cmp(&b.0.station)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    if let Some(last) = xs.last().cloned()
        && (last.0.station as f64) < total
    {
        let at = total as f32;
        let mut x = last.0;
        x.station = at;
        x.elevation = profile[profile.len() - 1].1;
        xs.push((x, xs_line(line, at, last.0.width, last.0.reach)));
    }
    // Merge the profile and the cross sections (a cross section's elevation
    // replaces the profile at its station).
    let mut cp: Profile = Vec::new();
    let mut p2: Profile = Vec::new();
    let mut sel: Vec<&(ControlXs, Vec<Pm>)> = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < profile.len() || j < xs.len() {
        if i < profile.len() && (j == xs.len() || profile[i].0 < xs[j].0.station) {
            cp.push(profile[i]);
            i += 1;
        } else {
            let x = &xs[j];
            cp.push((x.0.station, x.0.elevation));
            p2.push((x.0.station, x.0.elevation));
            sel.push(x);
            j += 1;
        }
        let last = cp[cp.len() - 1].0;
        while i < profile.len() && profile[i].0 <= last {
            i += 1;
        }
        while j < xs.len() && xs[j].0.station <= last {
            j += 1;
        }
    }
    let n = sel.len();
    let mut tw: Vec<f32> = sel.iter().map(|x| x.0.width).collect();
    let mut mr: Vec<f32> = sel.iter().map(|x| x.0.reach).collect();
    let mut ls: Vec<f32> = sel.iter().map(|x| x.0.left_slope).collect();
    let mut rs: Vec<f32> = sel.iter().map(|x| x.0.right_slope).collect();
    let tp: Vec<f32> = sel.iter().map(|x| x.0.transition).collect();
    fill_values(&mut tw, 0.0, 0.0);
    fill_values(&mut mr, 0.0, 0.0);
    fill_values(&mut ls, 3.0, 0.001);
    fill_values(&mut rs, 3.0, 0.001);
    for k in 0..n {
        if mr[k] == 0.0 {
            mr[k] = tw[k];
        } else if tw[k] > mr[k] {
            tw[k] = mr[k];
        }
    }
    // Transition stations: a cross section that changes the template ends
    // its section Top Width x Transition Percent before the next one.
    if n >= 2 {
        for k in 0..=(n - 2).min(p2.len() - 2) {
            if !(tp[k] <= 0.0) && (tw[k] != tw[k + 1] || mr[k] != mr[k + 1]) {
                let (x, x2) = (p2[k].0, p2[k + 1].0);
                let s = f32_max(x, x2 - tw[k] * tp[k]);
                if !(s <= x) && !(s >= x2) && !cp.iter().any(|q| (q.0 - s).abs() <= 0.01) {
                    let v = query(&cp, s);
                    cp.push((s, v));
                }
            }
        }
    }
    sort_stations(&mut cp);
    if sel.iter().any(|x| x.1.len() < 2) {
        return None;
    }
    let mut xprof = Vec::with_capacity(n);
    for k in 0..n {
        if tw[k].is_nan() || (tw[k] == 0.0 && mr[k] == 0.0) {
            return None;
        }
        xprof.push(xs_profile(subtype, p2[k].1, tw[k], ls[k], rs[k], mr[k]));
    }
    match subtype {
        GroundLineSubtype::Channel => ls.iter_mut().for_each(|v| *v = -*v),
        GroundLineSubtype::Levee => rs.iter_mut().for_each(|v| *v = -*v),
    }
    let mm = m_fraction(line);
    let mut out: [Vec<Pm>; 4] = Default::default();
    let with_z = |l: &[Pm], prof: &Profile| -> Option<Vec<Pm>> {
        let len = length(l);
        let plen = (prof[prof.len() - 1].0 - prof[0].0) as f64;
        if (len - plen).abs() > 0.01 && plen > len {
            return None;
        }
        Some(z_from_profile(l, prof, len, true))
    };
    let set_m = |v: Vec<Pm>, m: f64| -> Vec<Pm> { v.into_iter().map(|p| Pm { m, ..p }).collect() };
    for m in 0..n.saturating_sub(1) {
        let x = p2[m].0;
        let us = set_m(with_z(&sel[m].1, &xprof[m])?, x as f64 / total);
        let x2 = p2[m + 1].0;
        let mut s2 = x2;
        if tp[m] > 0.0 && (tw[m] != tw[m + 1] || mr[m] != mr[m + 1]) {
            s2 = f32_max(x, x2 - tw[m] * tp[m]);
        }
        let ds_line = xs_line(&mm, s2, tw[m], mr[m]);
        let ds = set_m(with_z(&ds_line, &xprof[m])?, s2 as f64 / total);
        let at = |hit: (f64, f64, usize, f64, usize, f64), st: f32| {
            let p = Pm {
                x: hit.0,
                y: hit.1,
                z: f64::NAN,
                m: st as f64 / total,
            };
            (
                Pop {
                    p,
                    seg: hit.2,
                    frac: hit.3,
                },
                Pop {
                    p,
                    seg: hit.4,
                    frac: hit.5,
                },
            )
        };
        let h0 = if m == 0 && x == 0.0 {
            (mm[0].x, mm[0].y, 0, 0.0, 0, 0.5)
        } else {
            *xs_crossings(&mm, &us).first()?
        };
        let (a0, b0) = at(h0, x);
        let h1 = if m == n - 2 {
            let k = mm.len() - 1;
            (mm[k].x, mm[k].y, k - 1, 1.0, 0, 0.5)
        } else {
            *xs_crossings(&mm, &ds).first()?
        };
        let (a1, b1) = at(h1, s2);
        let stream = point_subsection(&mm, &a0, &a1);
        let us_tot = length(&us);
        let ds_tot = length(&ds);
        let targets = [
            (first_pop(&us), first_pop(&ds)),
            (last_pop(&us), last_pop(&ds)),
            (
                pop_at_distance(&us, us_tot, xprof[m][1].0 as f64),
                pop_at_distance(&ds, ds_tot, xprof[m][1].0 as f64),
            ),
            (
                pop_at_distance(&us, us_tot, xprof[m][2].0 as f64),
                pop_at_distance(&ds, ds_tot, xprof[m][2].0 as f64),
            ),
        ];
        for (k, (up, dn)) in targets.iter().enumerate() {
            let o = section_offset(&stream, &b0, &b1, &us, up, &ds, dn);
            if o.len() > 1 {
                out[k].extend(o);
            }
        }
    }
    let [mut lt, mut rt, mut lr, mut rr] = out;
    for v in [&mut lt, &mut lr, &mut rr, &mut rt] {
        let closed = v.len() >= 2 && is_ring(v, 0.01);
        let tail = if closed { v.pop() } else { None };
        trim_self_intersections(v);
        if let Some(t) = tail {
            v.push(t);
        }
    }
    let fr: Vec<f64> = cp.iter().map(|p| p.0 as f64 / total).collect();
    let station = |p: &Pm| (p.m * total) as f32;
    let ridge = |v: &[Pm]| -> Vec<Pm> {
        inject_fractions(v, &fr)
            .into_iter()
            .map(|p| Pm {
                z: query(&cp, station(&p)) as f64,
                ..p
            })
            .collect()
    };
    let along = |vals: &[f32]| -> Profile { p2.iter().zip(vals).map(|(a, &v)| (a.0, v)).collect() };
    let (pmr, ptw, pls, prs) = (along(&mr), along(&tw), along(&ls), along(&rs));
    let toe = |v: &[Pm], slopes: &Profile, side: f64| -> Vec<Pm> {
        inject_fractions(v, &fr)
            .into_iter()
            .map(|p| {
                let s = station(&p);
                let q = query(&cp, s);
                let run = query(&pmr, s) / 2.0 - query(&ptw, s) / 2.0;
                Pm {
                    z: q as f64 + side * (run as f64 * (1.0 / query(slopes, s) as f64)),
                    ..p
                }
            })
            .collect()
    };
    let centre = z_from_profile(&mm, &cp, total, false);
    Some([
        toe(&lt, &pls, -1.0),
        ridge(&lr),
        centre,
        ridge(&rr),
        toe(&rt, &prs, 1.0),
    ])
}

/// .NET `Math.Max` on binary32: NaN when either is NaN.
fn f32_max(a: f32, b: f32) -> f32 {
    if a > b || a.is_nan() { a } else { b }
}

/// Systems of a ground-line layer: named features joined end to start
/// (within 60 ft, same name, elevation type and Elev Pt Tolerance), in
/// joined order. Returns the system of each feature.
fn ground_line_systems(features: &[GroundLine]) -> (Vec<Option<usize>>, Vec<Vec<usize>>) {
    let n = features.len();
    let mut of: Vec<Option<usize>> = vec![None; n];
    let mut systems: Vec<Vec<usize>> = Vec::new();
    let first = |k: usize| Pm::xy(features[k].points[0][0], features[k].points[0][1]);
    let last = |k: usize| {
        let p = features[k].points[features[k].points.len() - 1];
        Pm::xy(p[0], p[1])
    };
    for i in 0..n {
        let name = features[i].system_name.trim();
        if of[i].is_some() || name.is_empty() {
            continue;
        }
        let id = systems.len();
        let mut chain = std::collections::VecDeque::from([i]);
        of[i] = Some(id);
        let joins = |k: usize, of: &[Option<usize>]| {
            k != i
                && of[k].is_none()
                && features[k].system_name.trim() == name
                && features[k].elevation_type == features[i].elevation_type
                && features[k].elev_pt_tolerance.to_bits()
                    == features[i].elev_pt_tolerance.to_bits()
        };
        for forward in [false, true] {
            let mut pt = if forward { last(i) } else { first(i) };
            loop {
                let mut best: Option<(usize, f64)> = None;
                for k in 0..n {
                    if !joins(k, &of) {
                        continue;
                    }
                    let d = dist(pt, if forward { first(k) } else { last(k) });
                    if d < SYSTEM_SEARCH_RADIUS && best.is_none_or(|b| d < b.1) {
                        best = Some((k, d));
                    }
                }
                let Some((k, _)) = best else { break };
                of[k] = Some(id);
                if forward {
                    chain.push_back(k);
                    pt = last(k);
                } else {
                    chain.push_front(k);
                    pt = first(k);
                }
            }
        }
        systems.push(chain.into_iter().collect());
    }
    (of, systems)
}

/// The ribbon TIN of one system.
fn system_tin(
    features: &[GroundLine],
    members: &[usize],
    subtype: GroundLineSubtype,
    units: LengthUnits,
) -> Option<CleanTin> {
    let fs: Vec<&GroundLine> = members.iter().map(|&k| &features[k]).collect();
    let sys = build_system(&fs);
    let ribbons = system_ribbons(&sys, subtype, units)?;
    Some(ribbon_tin(&ribbons, 0.01))
}

// ---------------------------------------------------------------------------
// Control points and polygon TINs
// ---------------------------------------------------------------------------

/// Nearest position on `line` (first segment at the smallest distance) and
/// that distance.
fn nearest_on(line: &[Pm], q: Pm) -> (Pop, f64) {
    let (mut best, mut seg) = (f64::MAX, 0);
    for i in 0..line.len() - 1 {
        let d = seg_distance(line[i], line[i + 1], q);
        if d < best {
            best = d;
            seg = i;
        }
    }
    let (a, b) = (line[seg], line[seg + 1]);
    let l = dist(a, b);
    let (p, t) = if l == 0.0 {
        (a, 0.0)
    } else {
        let along = ((b.x - a.x) * (q.x - a.x) + (b.y - a.y) * (q.y - a.y)) / l;
        if along < 0.0 {
            (a, 0.0)
        } else if along > l {
            (b, 1.0)
        } else {
            let t = along / dist(a, b);
            (lerp(a, b, t), t)
        }
    };
    (Pop { p, seg, frac: t }, best)
}

/// Distance along `line` to `at` (segment lengths summed in order).
fn distance_along(line: &[Pm], at: &Pop) -> f64 {
    let mut s = 0.0;
    for i in 0..at.seg {
        s += dist(line[i], line[i + 1]);
    }
    s + dist(line[at.seg], line[at.seg + 1]) * at.frac
}

/// Cumulative segment ends.
fn cumulative(line: &[Pm]) -> Vec<f64> {
    let mut out = Vec::with_capacity(line.len().saturating_sub(1));
    let mut s = 0.0;
    for w in line.windows(2) {
        s += dist(w[0], w[1]);
        out.push(s);
    }
    out
}

/// Distance along with precomputed cumulative lengths.
fn distance_along_fast(line: &[Pm], cum: &[f64], at: &Pop) -> f64 {
    let base = if at.seg > 0 { cum[at.seg - 1] } else { 0.0 };
    base + dist(line[at.seg], line[at.seg + 1]) * at.frac
}

/// Vertex `i` as RAS Mapper lists polyline vertices: the first at the start
/// of segment 0, the others at the end of the previous segment.
fn vertex_pop(line: &[Pm], i: usize) -> Pop {
    if i == 0 {
        Pop {
            p: line[0],
            seg: 0,
            frac: 0.0,
        }
    } else {
        Pop {
            p: line[i],
            seg: i - 1,
            frac: 1.0,
        }
    }
}

/// Control-point stations of a ground line: points within the Elev Pt
/// Tolerance of the centerline set the profile at their nearest station
/// (overriding stored stations); with no stored profile the ends take the
/// elevations of the first and last such point.
fn control_point_stations(
    keys: &mut Profile,
    stored_empty: bool,
    cps: &[ControlPoint],
    tol: f32,
    line: &[Pm],
    len: f64,
) {
    let has = |keys: &Profile, x: f32| keys.iter().any(|k| k.0 == x);
    let mut at_start = has(keys, 0.0);
    let mut at_end = has(keys, len as f32);
    let (mut lo_v, mut lo_s, mut hi_v, mut hi_s) = (0.0_f32, f64::MAX, 0.0_f32, f64::MIN);
    let (first, last) = (line[0], line[line.len() - 1]);
    for cp in cps {
        if cp.elevation.is_nan() {
            continue;
        }
        let e = cp.elevation as f32;
        let (at, d) = nearest_on(line, Pm::xy(cp.x, cp.y));
        if d <= tol as f64 {
            let s = distance_along(line, &at);
            let k = s as f32;
            match keys.iter_mut().find(|q| q.0 == k) {
                Some(q) => q.1 = e,
                None => keys.push((k, e)),
            }
            if s < lo_s {
                lo_s = s;
                lo_v = e;
            }
            if s > hi_s {
                hi_s = s;
                hi_v = e;
            }
            if at.p.x == first.x && at.p.y == first.y {
                at_start = true;
            }
            if at.p.x == last.x && at.p.y == last.y {
                at_end = true;
            }
        }
    }
    if !cps.is_empty() && stored_empty {
        if !at_start && !has(keys, 0.0) {
            keys.push((0.0, lo_v));
        }
        if !at_end {
            let (at, _) = nearest_on(line, last);
            let s = distance_along(line, &at) as f32;
            if !has(keys, s) {
                keys.push((s, hi_v));
            }
        }
    }
}

/// Douglas-Peucker filter of a profile in binary32 (first and last kept).
fn profile_filter(p: &Profile, tol: f32) -> Profile {
    let n = p.len();
    let mut keep = Vec::new();
    if n > 0 {
        keep.push(0);
    }
    if n > 1 {
        keep.push(n - 1);
    }
    fn dp(p: &Profile, first: usize, last: usize, tol: f32, keep: &mut Vec<usize>) {
        if last - first <= 1 {
            return;
        }
        let (a, b) = (p[first], p[last]);
        let d3 = (((b.0 - a.0) as f64).powi(2) + ((b.1 - a.1) as f64).powi(2)).sqrt() as f32;
        let (mut best, mut at) = (0.0_f32, 0);
        for (i, q) in p.iter().enumerate().take(last).skip(first + 1) {
            let v =
                (a.0 * b.1 + b.0 * q.1 + q.0 * a.1 - b.0 * a.1 - q.0 * b.1 - a.0 * q.1).abs() / d3;
            if v > best {
                best = v;
                at = i;
            }
        }
        if best > tol {
            keep.push(at);
            dp(p, first, at, tol, keep);
            dp(p, at, last, tol, keep);
        }
    }
    if n > 1 {
        dp(p, 0, n - 1, tol, &mut keep);
    }
    keep.sort_unstable();
    keep.iter().map(|&i| p[i]).collect()
}

/// The perimeter's points with z from the perimeter profile; vertices before
/// the first or after the last station interpolate across the ring's start.
fn perimeter_points(ring: &[Pm], prof: &Profile) -> Option<Vec<Pm>> {
    let len0 = length(ring);
    if prof.len() >= 2 {
        let plen = (prof[prof.len() - 1].0 - prof[0].0) as f64;
        if (len0 - plen).abs() > 0.01 && plen > len0 {
            return None;
        }
    }
    let line = z_from_profile(ring, prof, len0, true);
    let total = length(&line);
    let cum = cumulative(&line);
    let key = |p: &Pm| [p.x.to_bits(), p.y.to_bits(), p.z.to_bits(), p.m.to_bits()];
    let mut map = std::collections::HashMap::new();
    for i in 0..line.len() {
        map.insert(key(&line[i]), vertex_pop(&line, i));
    }
    let (p0, pn) = (prof[0], prof[prof.len() - 1]);
    let wrap: Profile = vec![pn, (total as f32 + p0.0, p0.1)];
    let mut out = line.clone();
    for (j, p) in line.iter().enumerate() {
        let at = map[&key(p)];
        let s = distance_along_fast(&line, &cum, &at) as f32;
        if s <= p0.0 {
            out[j].z = query(&wrap, (total + s as f64) as f32) as f64;
        } else if s > pn.0 {
            out[j].z = query(&wrap, s) as f64;
        } else {
            out[j].z = query(prof, s) as f64;
        }
    }
    Some(out)
}

/// Whether a polygon needs RAS Mapper's perimeter TIN rather than the
/// constant fill: boundary elevations, shape Z, or a control point inside.
fn polygon_needs_tin(f: &Polygon, cps: &[ControlPoint]) -> bool {
    f.generate_boundary
        || f.use_shape_z
        || cps
            .iter()
            .any(|c| !c.elevation.is_nan() && point_in_rings(&f.rings, c.x, c.y))
}

/// The perimeter TIN of a polygon (single ring).
fn polygon_tin(
    f: &Polygon,
    cps: &[ControlPoint],
    units: LengthUnits,
    name: &str,
    v66: bool,
) -> Result<Option<CleanTin>, MapError> {
    if f.rings.len() != 1 {
        return Err(MapError::Unsupported(format!(
            "polygon layer {name:?}: multi-part polygons with boundary elevations, shape Z or control points are not qualified"
        )));
    }
    if f.use_shape_z && !f.generate_boundary {
        // Elevations taken from the polygon's own vertex Z: how RAS Mapper
        // stores and reads vertex Z has not been measured, so the form is
        // refused (as `method_record` and the caller state).
        return Err(MapError::Unsupported(format!(
            "polygon layer {name:?}: shape-Z polygons (elevations from vertex Z) are not qualified"
        )));
    }
    let mut ring: Vec<Pm> = f.rings[0].iter().map(|p| Pm::xy(p[0], p[1])).collect();
    if f.use_shape_z {
        if f.vertex_z.len() != ring.len() {
            return Err(MapError::Unsupported(format!(
                "polygon layer {name:?}: shape Z needs one vertex_z per ring vertex"
            )));
        }
        for (p, z) in ring.iter_mut().zip(&f.vertex_z) {
            p.z = *z;
        }
    }
    let (a, b) = (ring[0], ring[ring.len() - 1]);
    if !(a.x == b.x && a.y == b.y) {
        ring.push(a);
    }
    let si = units == LengthUnits::Meters;
    let tol = f.elev_pt_tolerance as f32;
    let total = length(&ring);
    let cum = cumulative(&ring);
    let mut pf: Profile = Vec::new();
    let mut stations: Vec<f64> = Vec::new();
    let mut mass: Vec<[f64; 3]> = Vec::new();
    let mut on_perimeter = 0;
    for c in cps {
        if c.elevation.is_nan() {
            continue;
        }
        let e = c.elevation as f32;
        if point_in_rings(&f.rings, c.x, c.y) {
            mass.push([c.x, c.y, e as f64]);
            continue;
        }
        let (at, d) = nearest_on(&ring, Pm::xy(c.x, c.y));
        if f.generate_boundary && !f.use_shape_z && d <= tol as f64 {
            let s = distance_along_fast(&ring, &cum, &at);
            if !stations.contains(&s) {
                pf.push((s as f32, e));
                stations.push(s);
                on_perimeter += 1;
            }
        }
    }
    if f.use_shape_z && !f.generate_boundary {
        for i in 0..ring.len() {
            let s = distance_along_fast(&ring, &cum, &vertex_pop(&ring, i));
            if !stations.contains(&s) {
                pf.push((s as f32, ring[i].z as f32));
                stations.push(s);
            }
        }
    } else if f.generate_boundary && on_perimeter == 0 {
        if v66 {
            return Err(MapError::Unsupported(format!(
                "polygon layer {name:?}: HEC-RAS 6.6 samples boundary elevations from the terrain (not qualified)"
            )));
        }
        if f.profile.is_empty() {
            return Err(MapError::Unsupported(format!(
                "polygon layer {name:?}: boundary elevations sampled from the terrain (no stored profile, no perimeter control point) are not qualified"
            )));
        }
        for p in &f.profile {
            let x = p[0] as f32;
            if !stations.contains(&(x as f64)) {
                pf.push((x, p[1] as f32));
                stations.push(x as f64);
            }
        }
    }
    if pf.is_empty() {
        let v = f.elevation as f32;
        pf.push((0.0, v));
        pf.push((total as f32, v));
    }
    sort_stations(&mut pf);
    let ftol = if si { 0.01_f32 * 0.3048 } else { 0.01_f32 };
    let pf = profile_filter(&pf, ftol);
    let Some(pts) = perimeter_points(&ring, &pf) else {
        return Err(MapError::Unsupported(format!(
            "polygon layer {name:?}: a perimeter profile longer than the perimeter is not qualified"
        )));
    };
    let perim: Vec<[f64; 3]> = pts.iter().map(|p| [p.x, p.y, p.z]).collect();
    let ttol = if si { 0.1_f32 * 0.3048 } else { 0.1_f32 } as f64;
    Ok(export_tin::perimeter_tin(&perim, &mass, ttol).ok())
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

/// The HEC-RAS version whose modification rules apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RasVersion {
    V7_0_1,
    V6_6,
}

impl RasVersion {
    pub fn parse(v: Option<&str>) -> Result<Self, MapError> {
        match v.map(str::trim) {
            None | Some("7.0.1") => Ok(RasVersion::V7_0_1),
            Some("6.6") => Ok(RasVersion::V6_6),
            Some(o) => Err(MapError::Unsupported(format!(
                "hecras_version {o:?}: only 7.0.1 and 6.6 are qualified"
            ))),
        }
    }
}

/// Per-layer counts of changed pixels (HEC-RAS 7.0.1 rules).
pub fn apply(layers: &[Layer], grid: &Grid, values: &mut [f32]) -> Result<Value, MapError> {
    apply_version(layers, grid, values, RasVersion::V7_0_1)
}

/// Per-layer counts of changed pixels under `version`'s rules.
pub fn apply_version(
    layers: &[Layer],
    grid: &Grid,
    values: &mut [f32],
    version: RasVersion,
) -> Result<Value, MapError> {
    validate(layers)?;
    let v66 = version == RasVersion::V6_6;
    if v66 {
        for l in layers {
            if let Layer::GroundLine { features, name, .. } = l
                && features.iter().any(|f| !f.system_name.trim().is_empty())
            {
                return Err(MapError::Unsupported(format!(
                    "ground line layer {name:?}: named ground lines under HEC-RAS 6.6 are not qualified (its system path differs)"
                )));
            }
        }
    }
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
                control_points,
                ..
            } => {
                let (system_of, systems) = ground_line_systems(features);
                for (fi, f) in features.iter().enumerate() {
                    let tin = match system_of[fi] {
                        None => ground_line_tin(f, *subtype, *length_units, control_points),
                        // A system is drawn once, in the place of its
                        // highest feature ID, with that feature's type.
                        Some(s) if systems[s].iter().max() == Some(&fi) => {
                            system_tin(features, &systems[s], *subtype, *length_units)
                        }
                        Some(_) => continue,
                    };
                    let Some(tin) = tin else {
                        skipped += 1;
                        continue;
                    };
                    let b = bbox_of(tin.points.iter().map(|p| (p[0], p[1])));
                    let reach = match system_of[fi] {
                        None => f.max_reach as f32,
                        Some(s) => systems[s]
                            .iter()
                            .map(|&k| features[k].max_reach as f32)
                            .fold(f32::NAN, f32::max),
                    };
                    for (l, m, tg, rows, cols) in tiles.over(b) {
                        // HEC-RAS 6.6 does not draw ground lines whose Max
                        // Reach is not wider than one cell of the tile.
                        if v66 && reach as f64 / tg.cs <= 1.0 {
                            continue;
                        }
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
            Layer::Polygon {
                features,
                control_points,
                length_units,
                name,
                ..
            } => {
                for f in features {
                    if polygon_needs_tin(f, control_points) {
                        let Some(tin) = polygon_tin(f, control_points, *length_units, name, v66)?
                        else {
                            skipped += 1;
                            continue;
                        };
                        let b = bbox_of(f.rings.iter().flatten().map(|p| (p[0], p[1])));
                        for (l, m, tg, rows, cols) in tiles.over(b) {
                            buf.fill(f32::NAN);
                            export_tin::rasterize(&tin, &tg, &mut buf);
                            // RAS Mapper combines over the cells holding the
                            // polygon's extent.
                            let cell = |v: f64, lo: f64| ((v - lo) / tg.cs).floor() as i64;
                            let (c0, c1) = (cell(b[0], tg.min_x), cell(b[2], tg.min_x));
                            let (r0, r1) = (
                                ((tg.max_y - b[3]) / tg.cs).floor() as i64,
                                ((tg.max_y - b[1]) / tg.cs).floor() as i64,
                            );
                            for i in 0..rows {
                                for j in 0..cols {
                                    let (ii, jj) = (i as i64, j as i64);
                                    if ii < r0 || ii > r1 || jj < c0 || jj > c1 {
                                        continue;
                                    }
                                    let z = buf[i * TILE + j];
                                    if !z.is_nan() {
                                        set(l * TILE + i, m * TILE + j, z, f.elevation_type);
                                    }
                                }
                            }
                        }
                        continue;
                    }
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
            transition_percent: 0.0,
            elev_pt_tolerance: 0.0,
        }
    }

    fn gl_layer(subtype: GroundLineSubtype, features: Vec<GroundLine>) -> Layer {
        gl_layer_cp(subtype, features, Vec::new())
    }

    fn gl_layer_cp(
        subtype: GroundLineSubtype,
        features: Vec<GroundLine>,
        control_points: Vec<ControlPoint>,
    ) -> Layer {
        Layer::GroundLine {
            name: "l".into(),
            priority: 0,
            subtype,
            length_units: LengthUnits::Feet,
            features,
            control_points,
        }
    }

    fn poly(ring: Vec<[f64; 2]>, et: ElevationType, elevation: f64) -> Polygon {
        Polygon {
            elevation_type: et,
            elevation,
            rings: vec![ring],
            generate_boundary: false,
            use_shape_z: false,
            profile: Vec::new(),
            vertex_z: Vec::new(),
            elev_pt_tolerance: 20.0,
        }
    }

    fn poly_layer(features: Vec<Polygon>, control_points: Vec<ControlPoint>) -> Layer {
        Layer::Polygon {
            name: "p".into(),
            priority: 0,
            features,
            control_points,
            length_units: LengthUnits::Feet,
        }
    }

    fn cp(x: f64, y: f64, elevation: f64) -> ControlPoint {
        ControlPoint { x, y, elevation }
    }

    #[test]
    fn shape_z_polygon_without_boundary_elevations_is_refused() {
        let ring = vec![[20.0, 20.0], [80.0, 20.0], [80.0, 80.0], [20.0, 80.0]];
        let mut f = poly(ring, ElevationType::SetValue, 10.0);
        f.use_shape_z = true;
        f.vertex_z = vec![1.0, 2.0, 3.0, 4.0];
        let mut v = vec![0.0_f32; 100 * 100];
        let err = apply(&[poly_layer(vec![f], Vec::new())], &grid(), &mut v).unwrap_err();
        assert!(matches!(err, MapError::Unsupported(_)), "{err:?}");
        assert!(
            v.iter().all(|z| *z == 0.0),
            "nothing written before refusing"
        );
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
        let tin = ground_line_tin(&f, GroundLineSubtype::Channel, LengthUnits::Feet, &[]).unwrap();
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
        let p = levee_profile(&[[5.0, 1.0], [305.0, 4.0]], &line, 300.0, 100.0, None);
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
            transition_percent: 0.0,
            elev_pt_tolerance: 0.0,
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
            &[poly_layer(
                vec![poly(
                    vec![[10.0, 10.0], [20.0, 10.0], [20.0, 20.0], [10.0, 20.0]],
                    ElevationType::SetIfLower,
                    7.3,
                )],
                Vec::new(),
            )],
            &g,
            &mut v,
        )
        .unwrap();
        assert_eq!(v[(100 - 15) * 100 + 15], 7.3_f32);
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

    fn named(points: Vec<[f64; 2]>, z: [f64; 2], name: &str, top: f64, tp: f64) -> GroundLine {
        let l: f64 = points
            .windows(2)
            .map(|w| ((w[1][0] - w[0][0]).powi(2) + (w[1][1] - w[0][1]).powi(2)).sqrt())
            .sum();
        let mut f = levee(points, vec![[0.0, z[0]], [l, z[1]]]);
        f.system_name = name.into();
        f.top_width = top;
        f.transition_percent = tp;
        f
    }

    #[test]
    fn named_features_join_end_to_start_within_60_ft() {
        let a = named(vec![[100.0, 0.0], [200.0, 0.0]], [5.0, 5.0], "S", 10.0, 0.0);
        let b = named(vec![[0.0, 0.0], [100.0, 0.0]], [5.0, 5.0], "S", 10.0, 0.0);
        // 30 ft gap: joined after a.
        let c = named(vec![[230.0, 0.0], [300.0, 0.0]], [5.0, 5.0], "S", 10.0, 0.0);
        // 60 ft gap: not joined (strictly within 60).
        let d = named(vec![[360.0, 0.0], [400.0, 0.0]], [5.0, 5.0], "S", 10.0, 0.0);
        let mut e = named(vec![[300.0, 0.0], [330.0, 0.0]], [5.0, 5.0], "S", 10.0, 0.0);
        e.elevation_type = ElevationType::SetIfLower;
        let u = levee(
            vec![[0.0, 50.0], [10.0, 50.0]],
            vec![[0.0, 1.0], [10.0, 1.0]],
        );
        let (of, systems) = ground_line_systems(&[a, b, c, d, e, u]);
        assert_eq!(systems, vec![vec![1, 0, 2], vec![3], vec![4]]);
        assert_eq!(of, vec![Some(0), Some(0), Some(0), Some(1), Some(2), None]);
    }

    #[test]
    fn junction_takes_the_next_feature_start_elevation() {
        let a = named(vec![[0.0, 0.0], [100.0, 0.0]], [5.0, 6.0], "S", 10.0, 0.0);
        let b = named(vec![[100.0, 0.0], [250.0, 0.0]], [9.0, 9.0], "S", 10.0, 0.0);
        let sys = build_system(&[&a, &b]);
        assert_eq!(sys.xs.len(), 2);
        assert_eq!((sys.xs[1].station, sys.xs[1].elevation), (100.0, 9.0));
        let seed: Profile = sys.xs.iter().map(|x| (x.station, x.elevation)).collect();
        let p = seeded_profile(
            &seed,
            &sys.profile,
            &sys.line,
            length(&sys.line),
            100.0,
            None,
        );
        // The previous feature's end elevation (6) is dropped: its profile
        // now runs to the next feature's start elevation.
        assert_eq!(query(&p, 100.0), 9.0);
        assert_eq!(query(&p, 50.0), 7.0);
    }

    #[test]
    fn transition_ends_a_section_early() {
        // Top Width 10 then 30, Transition Percent 0.5: the first section's
        // template ends 5 ft before the second cross section.
        let a = named(
            vec![[0.0, 50.0], [60.0, 50.0]],
            [20.0, 20.0],
            "S",
            10.0,
            0.5,
        );
        let mut b = named(
            vec![[60.0, 50.0], [95.0, 50.0]],
            [20.0, 20.0],
            "S",
            30.0,
            0.5,
        );
        b.max_reach = 60.0;
        let sys = build_system(&[&a, &b]);
        let r = system_ribbons(&sys, GroundLineSubtype::Levee, LengthUnits::Feet).unwrap();
        let stations: Vec<f64> = r[2].iter().map(|p| p.m * 95.0).collect();
        assert!(
            stations.iter().any(|s| (s - 55.0).abs() < 1e-3),
            "{stations:?}"
        );
    }

    #[test]
    fn cross_section_profile_narrows_an_equal_width() {
        let p = xs_profile(GroundLineSubtype::Levee, 10.0, 20.0, 2.0, 2.0, 20.0);
        assert!((p[0].1 - 9.9975).abs() < 1e-5 && p[0].0 == 0.0, "{p:?}");
        assert_eq!(p[3].0, 20.0);
        assert!((p[2].0 - p[1].0 - 19.99).abs() < 1e-4, "{p:?}");
    }

    #[test]
    fn missing_values_take_neighbours() {
        let mut v = [f32::NAN, 0.0, 2.0, f32::NAN];
        fill_values(&mut v, 3.0, 0.001);
        assert_eq!(v, [0.001, 0.001, 2.0, 2.0]);
        let mut v = [f32::NAN, f32::NAN];
        fill_values(&mut v, 3.0, 0.001);
        assert_eq!(v, [3.0, 3.0]);
    }

    #[test]
    fn straight_named_levee_matches_closed_form() {
        let f = named(
            vec![[10.0, 50.0], [90.0, 50.0]],
            [20.0, 20.0],
            "S",
            10.0,
            0.0,
        );
        let g = grid();
        let mut v = vec![0.0_f32; 100 * 100];
        apply(&[gl_layer(GroundLineSubtype::Levee, vec![f])], &g, &mut v).unwrap();
        for r in 0..100 {
            for c in 15..85 {
                let (x, y) = centre(&g, r, c);
                let o = (y - 50.0).abs();
                let slope = if y > 50.0 { 2.0 } else { 4.0 };
                let want = if o <= 20.0 {
                    20.0 - (o - 5.0).max(0.0) / slope
                } else {
                    0.0
                };
                let got = v[r * 100 + c] as f64;
                assert!(
                    (got - want).abs() <= 1.0 / 32.0 + 1e-9,
                    "x {x} y {y}: {got} vs {want}"
                );
            }
        }
    }

    #[test]
    fn control_points_set_ground_line_stations() {
        // Flat stored profile at 20; a control point 3 ft off the centerline
        // at station 40 sets 30 there; one 80 ft away is beyond tolerance.
        let mut f = levee(
            vec![[10.0, 50.0], [90.0, 50.0]],
            vec![[0.0, 20.0], [80.0, 20.0]],
        );
        f.elev_pt_tolerance = 50.0;
        let line: Vec<Pm> = f.points.iter().map(|p| Pm::xy(p[0], p[1])).collect();
        let cps = [cp(50.0, 53.0, 30.0), cp(30.0, 130.0, 99.0)];
        let p = levee_profile(&f.profile, &line, 80.0, 100.0, Some((&cps, 50.0)));
        assert_eq!(query(&p, 40.0), 30.0);
        assert_eq!(query(&p, 20.0), 25.0);
        // No stored profile: the ends take the first and last point's value.
        let p = levee_profile(&[], &line, 80.0, 100.0, Some((&cps[..1], 50.0)));
        assert_eq!(p, vec![(0.0, 30.0), (40.0, 30.0), (80.0, 30.0)]);
    }

    #[test]
    fn interior_control_point_lifts_a_constant_polygon() {
        let g = grid();
        let mut v = vec![0.0_f32; 100 * 100];
        let ring = vec![
            [20.0, 20.0],
            [80.0, 20.0],
            [80.0, 80.0],
            [20.0, 80.0],
            [20.0, 20.0],
        ];
        apply(
            &[poly_layer(
                vec![poly(ring, ElevationType::SetValue, 10.0)],
                vec![cp(50.5, 50.5, 40.0)],
            )],
            &g,
            &mut v,
        )
        .unwrap();
        let at = |x: f64, y: f64| v[((100.0 - y) as usize) * 100 + x as usize];
        assert_eq!(at(50.5, 50.5), 40.0);
        assert!(at(35.5, 50.5) > 10.0 && at(35.5, 50.5) < 40.0);
        assert_eq!(at(10.5, 50.5), 0.0);
    }

    #[test]
    fn stored_boundary_profile_sets_the_perimeter() {
        // Perimeter 240 ft from (20,20) anticlockwise; elevations 0 at the
        // start rising to 60 at station 120 and back to 0.
        let g = grid();
        let mut v = vec![-5.0_f32; 100 * 100];
        let ring = vec![
            [20.0, 20.0],
            [80.0, 20.0],
            [80.0, 80.0],
            [20.0, 80.0],
            [20.0, 20.0],
        ];
        let mut p = poly(ring, ElevationType::SetValue, 0.0);
        p.generate_boundary = true;
        p.profile = vec![[0.0, 0.0], [120.0, 60.0], [240.0, 0.0]];
        apply(&[poly_layer(vec![p], Vec::new())], &g, &mut v).unwrap();
        let at = |x: f64, y: f64| v[((100.0 - y) as usize) * 100 + x as usize];
        // Near the far corner (80, 80) (station 120) and on the first edge.
        assert!((at(79.5, 79.5) - 59.0).abs() < 1.0, "{}", at(79.5, 79.5));
        assert!((at(50.5, 20.5) - 15.0).abs() < 1.0, "{}", at(50.5, 20.5));
    }

    #[test]
    fn boundary_from_the_terrain_is_refused() {
        let ring = vec![
            [20.0, 20.0],
            [80.0, 20.0],
            [80.0, 80.0],
            [20.0, 80.0],
            [20.0, 20.0],
        ];
        let mut p = poly(ring, ElevationType::SetValue, 0.0);
        p.generate_boundary = true;
        let mut v = vec![0.0_f32; 100 * 100];
        let r = apply(&[poly_layer(vec![p], Vec::new())], &grid(), &mut v);
        assert!(matches!(r, Err(MapError::Unsupported(_))));
    }

    #[test]
    fn add_uses_the_unrounded_terrain() {
        assert_eq!(combine(10.01, 3.0, ElevationType::Add), 13.01_f32);
        assert_eq!(combine(NODATA, 3.0, ElevationType::Add), 3.0);
        assert_eq!(combine(10.0, 10.0, ElevationType::SetIfHigher), 10.0);
    }

    #[test]
    fn nan_templates() {
        let g = grid();
        // NaN Max Reach: nothing drawn.
        let mut f = levee(
            vec![[10.0, 50.0], [90.0, 50.0]],
            vec![[0.0, 20.0], [80.0, 20.0]],
        );
        f.max_reach = f64::NAN;
        let mut v = vec![0.0_f32; 100 * 100];
        apply(
            &[gl_layer(GroundLineSubtype::Levee, vec![f.clone()])],
            &g,
            &mut v,
        )
        .unwrap();
        assert!(v.iter().all(|z| *z == 0.0));
        // NaN Top Width with a finite Max Reach: refused.
        f.max_reach = 40.0;
        f.top_width = f64::NAN;
        assert!(matches!(
            validate(&[gl_layer(GroundLineSubtype::Levee, vec![f.clone()])]),
            Err(MapError::Unsupported(_))
        ));
        // NaN slopes are read as 3.
        f.top_width = 10.0;
        f.left_slope = f64::NAN;
        let mut v = vec![0.0_f32; 100 * 100];
        apply(&[gl_layer(GroundLineSubtype::Levee, vec![f])], &g, &mut v).unwrap();
        let at = |x: f64, y: f64| v[((100.0 - y) as usize) * 100 + x as usize];
        assert!((at(50.5, 65.5) as f64 - (20.0 - 10.5 / 3.0)).abs() < 1e-3);
    }

    #[test]
    fn negative_slope_flips_the_side() {
        // Channel at 0 with a negative left slope: the left toe is below.
        let mut f = levee(
            vec![[10.0, 50.0], [90.0, 50.0]],
            vec![[0.0, 0.0], [80.0, 0.0]],
        );
        f.elevation_type = ElevationType::SetIfLower;
        f.left_slope = -2.0;
        let tin = ground_line_tin(&f, GroundLineSubtype::Channel, LengthUnits::Feet, &[]).unwrap();
        assert!(tin.points.iter().any(|p| p[2] == -7.5));
    }

    #[test]
    fn hecras_6_6_skips_narrow_ground_lines() {
        assert_eq!(RasVersion::parse(None).unwrap(), RasVersion::V7_0_1);
        assert!(RasVersion::parse(Some("6.5")).is_err());
        let mut f = levee(
            vec![[10.0, 50.5], [90.0, 50.5]],
            vec![[0.0, 20.0], [80.0, 20.0]],
        );
        f.top_width = 0.5;
        f.max_reach = 1.0; // one cell
        let layers = [gl_layer(GroundLineSubtype::Levee, vec![f])];
        let mut v = vec![0.0_f32; 100 * 100];
        apply_version(&layers, &grid(), &mut v, RasVersion::V6_6).unwrap();
        assert!(v.iter().all(|z| *z == 0.0));
        apply(&layers, &grid(), &mut v).unwrap();
        assert!(v.contains(&20.0));
    }

    #[test]
    fn unqualified_forms_are_refused() {
        let mut f = levee(vec![[0.0, 0.0], [1.0, 0.0]], vec![[0.0, 0.0], [1.0, 0.0]]);
        f.elevation_type = ElevationType::SetValue;
        let layers = [gl_layer(GroundLineSubtype::Channel, vec![f.clone()])];
        assert!(validate(&layers).is_ok());
        f.system_name = "chan".into();
        let layers = [gl_layer(GroundLineSubtype::Channel, vec![f.clone()])];
        assert!(validate(&layers).is_ok());
        let layers = [gl_layer_cp(
            GroundLineSubtype::Channel,
            vec![f],
            vec![cp(0.0, 0.0, 1.0)],
        )];
        assert!(matches!(validate(&layers), Err(MapError::Unsupported(_))));
    }
}
