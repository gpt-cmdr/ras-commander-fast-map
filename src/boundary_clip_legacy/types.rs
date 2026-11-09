//! Clipper 6's value types and internal records (`IntPoint`, `IntRect`, the
//! enums, `TEdge`, `IntersectNode`, `LocalMinima`, `OutRec`, `OutPt`, `Join`, `Int128Mul`,
//! `ClipperException`). Links between records are arena indices into [`super::Clipper`].

use std::fmt;

/// Clipper2-rust's `Point64`, the type the crate root's helpers already use.
pub type IntPoint = clipper2_rust::Point64;
pub type Path = Vec<IntPoint>;
pub type Paths = Vec<Path>;

/// `new IntPoint(cInt x, cInt y)`.
pub fn ip(x: i64, y: i64) -> IntPoint {
    IntPoint::new(x, y)
}

/// `top` is the smallest y.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IntRect {
    pub left: i64,
    pub top: i64,
    pub right: i64,
    pub bottom: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipType {
    Intersection,
    Union,
    Difference,
    Xor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PolyType {
    Subject,
    Clip,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PolyFillType {
    EvenOdd,
    NonZero,
    Positive,
    Negative,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JoinType {
    Square,
    Round,
    Miter,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndType {
    ClosedPolygon,
    ClosedLine,
    OpenButt,
    OpenSquare,
    OpenRound,
}

/// A clipper failure, one variant per message Clipper 6 raises.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipperError {
    /// "AddPath: Open paths must be subject."
    OpenPathMustBeSubject,
    /// "Coordinate outside allowed range" (beyond +/- [`HI_RANGE`]).
    CoordinateOutsideRange,
    /// "Error: PolyTree struct is needed for open path clipping."
    PolyTreeNeededForOpenPaths,
    /// "UpdateEdgeIntoAEL: invalid call"
    UpdateEdgeIntoAelInvalidCall,
    /// "DoMaxima error"
    DoMaximaError,
    /// An output ring the sweep is still building has no points: an internal invariant
    /// broken, reported rather than read through.
    OutRecHasNoPoints,
    /// The bounded temporary index buffer for ordering local minima could not be allocated.
    LocalMinimaAllocation,
}

impl fmt::Display for ClipperError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ClipperError::OpenPathMustBeSubject => "AddPath: Open paths must be subject.",
            ClipperError::CoordinateOutsideRange => "Coordinate outside allowed range",
            ClipperError::PolyTreeNeededForOpenPaths => {
                "Error: PolyTree struct is needed for open path clipping."
            }
            ClipperError::UpdateEdgeIntoAelInvalidCall => "UpdateEdgeIntoAEL: invalid call",
            ClipperError::DoMaximaError => "DoMaxima error",
            ClipperError::OutRecHasNoPoints => "An output polygon has no points.",
            ClipperError::LocalMinimaAllocation => {
                "Unable to allocate local-minima ordering indices."
            }
        })
    }
}

impl std::error::Error for ClipperError {}

/// `ClipperBase.horizontal`: the `Dx` of a horizontal edge.
pub(super) const HORIZONTAL: f64 = -3.4E+38;
/// `ClipperBase.Skip`.
pub(super) const SKIP: i32 = -2;
/// `ClipperBase.Unassigned`.
pub(super) const UNASSIGNED: i32 = -1;
/// `ClipperBase.tolerance`.
const TOLERANCE: f64 = 1.0E-20;
/// `ClipperBase.loRange`: coordinates up to this keep 64-bit products exact.
pub const LO_RANGE: i64 = 0x3FFFFFFF;
/// `ClipperBase.hiRange`: the largest coordinate Clipper accepts.
pub const HI_RANGE: i64 = 0x3FFFFFFFFFFFFFFF;

/// `ClipperBase.near_zero`.
pub(super) fn near_zero(val: f64) -> bool {
    val > -TOLERANCE && val < TOLERANCE
}

/// `Clipper.Round` / `ClipperOffset.Round`: half away from zero, then truncated to
/// `i64` (`as` saturates out of range, which is never reached inside `HI_RANGE`).
pub(super) fn round(value: f64) -> i64 {
    if value < 0.0 {
        (value - 0.5) as i64
    } else {
        (value + 0.5) as i64
    }
}

/// `Int128.Int128Mul`: the exact product, compared only for equality.
pub(super) fn int128_mul(lhs: i64, rhs: i64) -> i128 {
    lhs as i128 * rhs as i128
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum EdgeSide {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Direction {
    RightToLeft,
    LeftToRight,
}

/// An edge of an input path. `next`/`prev` link the path's edge ring; the rest are optional links.
#[derive(Clone, Debug)]
pub(super) struct TEdge {
    pub bot: IntPoint,
    /// current (updated for every new scanbeam)
    pub curr: IntPoint,
    pub top: IntPoint,
    pub delta: IntPoint,
    pub dx: f64,
    pub poly_typ: PolyType,
    /// side only refers to current side of solution poly
    pub side: EdgeSide,
    /// 1 or -1 depending on winding direction
    pub wind_delta: i32,
    pub wind_cnt: i32,
    /// winding count of the opposite polytype
    pub wind_cnt2: i32,
    pub out_idx: i32,
    pub next: usize,
    pub prev: usize,
    pub next_in_lml: Option<usize>,
    pub next_in_ael: Option<usize>,
    pub prev_in_ael: Option<usize>,
    pub next_in_sel: Option<usize>,
    pub prev_in_sel: Option<usize>,
}

impl TEdge {
    pub fn new() -> Self {
        TEdge {
            bot: ip(0, 0),
            curr: ip(0, 0),
            top: ip(0, 0),
            delta: ip(0, 0),
            dx: 0.0,
            poly_typ: PolyType::Subject,
            side: EdgeSide::Left,
            wind_delta: 0,
            wind_cnt: 0,
            wind_cnt2: 0,
            out_idx: 0,
            next: 0,
            prev: 0,
            next_in_lml: None,
            next_in_ael: None,
            prev_in_ael: None,
            next_in_sel: None,
            prev_in_sel: None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct IntersectNode {
    pub edge1: usize,
    pub edge2: usize,
    pub pt: IntPoint,
}

/// A local minimum, in a singly linked list sorted by descending `y`.
#[derive(Clone, Copy, Debug)]
pub(super) struct LocalMinima {
    pub y: i64,
    pub left_bound: Option<usize>,
    pub right_bound: Option<usize>,
    pub next: Option<usize>,
}

/// A scanbeam, in a singly linked list sorted descending without duplicates.
#[derive(Clone, Copy, Debug)]
pub(super) struct Scanbeam {
    pub y: i64,
    pub next: Option<usize>,
}

/// A maximum, in a doubly linked list sorted ascending without duplicates.
#[derive(Clone, Copy, Debug)]
pub(super) struct Maxima {
    pub x: i64,
    pub next: Option<usize>,
    pub prev: Option<usize>,
}

/// A path in the clipping solution. Its arena index is its identity; `idx` is the
/// record it now lives in, which merging repoints at the surviving record.
#[derive(Clone, Copy, Debug)]
pub(super) struct OutRec {
    pub idx: i32,
    pub is_hole: bool,
    pub is_open: bool,
    /// see comments in clipper.pas
    pub first_left: Option<usize>,
    pub pts: Option<usize>,
    pub bottom_pt: Option<usize>,
    pub poly_node: Option<usize>,
}

/// A vertex of an output ring.
#[derive(Clone, Copy, Debug)]
pub(super) struct OutPt {
    pub idx: i32,
    pub pt: IntPoint,
    pub next: usize,
    pub prev: usize,
}

/// A pending join of two output points. A ghost join has no `out_pt2` (nothing reads it).
#[derive(Clone, Copy, Debug)]
pub(super) struct Join {
    pub out_pt1: usize,
    pub out_pt2: usize,
    pub off_pt: IntPoint,
}
