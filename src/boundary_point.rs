//! Native X-ray membership for original global-stitch sample locations.
//!
//! Physical-cell admission is deliberately narrower than fuzzy native ownership:
//! edge early returns or differing fuzzy/plain parity are unsupported until the
//! native candidate insertion order is qualified. Storage uses plain membership.

use crate::MapError;
use wbvector::{Coord, Ring};

pub(crate) const FUZZY_TOLERANCE: f64 = 1e-5;

fn count(ring: &Ring) -> usize {
    ring.0
        .len()
        .saturating_sub(usize::from(ring.0.first() == ring.0.last()))
}

fn bounds(exterior: &Ring, interiors: &[Ring]) -> [f64; 4] {
    exterior
        .0
        .iter()
        .chain(interiors.iter().flat_map(|ring| &ring.0))
        .fold(
            [
                f64::INFINITY,
                f64::INFINITY,
                f64::NEG_INFINITY,
                f64::NEG_INFINITY,
            ],
            |b, p| [b[0].min(p.x), b[1].min(p.y), b[2].max(p.x), b[3].max(p.y)],
        )
}

fn validate(ring: &Ring) -> Result<(), MapError> {
    if count(ring) < 3 || ring.0.iter().any(|p| !p.x.is_finite() || !p.y.is_finite()) {
        return Err(MapError::Invalid(
            "native boundary membership requires a finite polygon ring".into(),
        ));
    }
    Ok(())
}

// Preserve the native two weighted products and their addition. In particular,
// this is not AY + (BY-AY)*w, and out-of-segment X returns NaN, not a clamp.
fn interpolated_y(a: &Coord, b: &Coord, x: f64) -> f64 {
    let dx = b.x - a.x;
    if dx == 0.0 {
        return f64::NAN;
    }
    let weight = (b.x - x) / dx;
    if !(0.0..=1.0).contains(&weight) {
        return f64::NAN;
    }
    a.y * weight + b.y * (1.0 - weight)
}

fn plain_ring(ring: &Ring, point: [f64; 2]) -> bool {
    let n = count(ring);
    if n < 3 {
        return false;
    }
    let [x, y] = point;
    let mut hit = false;
    for i in 0..n {
        let (a, b) = (&ring.0[i], &ring.0[(i + 1) % n]);
        if ((a.x <= x && b.x > x) || (b.x <= x && a.x > x)) && y > interpolated_y(a, b, x) {
            hit = !hit;
        }
    }
    hit
}

fn guarded_ring(ring: &Ring, point: [f64; 2]) -> Result<bool, MapError> {
    let [x, y] = point;
    let tolerance = FUZZY_TOLERANCE;
    let n = count(ring);
    let mut fuzzy = false;
    for i in 0..n {
        let (a, b) = (&ring.0[i], &ring.0[(i + 1) % n]);
        let eligible = (a.x - tolerance <= x && b.x + tolerance > x)
            || (b.x - tolerance <= x && a.x + tolerance > x);
        if !eligible {
            continue;
        }
        if !(b.x - a.x).is_finite() || !(b.x - x).is_finite() {
            return Err(MapError::Invalid(
                "native boundary membership arithmetic overflow".into(),
            ));
        }
        let ordinate = interpolated_y(a, b, x);
        let difference = ordinate - y;
        if ordinate.is_infinite() || difference.is_infinite() {
            return Err(MapError::Invalid(
                "native boundary membership arithmetic overflow".into(),
            ));
        }
        // Native fuzzy admission returns early here. Refuse instead of choosing
        // which physical candidate would win a parallel native lookup.
        let ordinate_edge = difference.abs() < tolerance;
        let vertical_edge = (a.x - x).abs() < tolerance
            && (a.x - b.x).abs() < tolerance
            && y - tolerance < a.y.max(b.y)
            && y + tolerance > a.y.min(b.y);
        if ordinate_edge || vertical_edge {
            return Err(MapError::Unsupported(
                "native boundary stitch point touches a fuzzy polygon edge; ownership is unqualified".into(),
            ));
        }
        if difference < 0.0 {
            fuzzy = !fuzzy;
        }
    }
    let plain = plain_ring(ring, point);
    if fuzzy != plain {
        return Err(MapError::Unsupported(
            "native boundary stitch point has ambiguous fuzzy/plain X-ray vertex parity".into(),
        ));
    }
    Ok(plain)
}

/// Conservative native physical-cell membership. The caller must expand its
/// spatial candidate query by `FUZZY_TOLERANCE` and refuse multiple claims.
pub(crate) fn contains_polygon(
    exterior: &Ring,
    interiors: &[Ring],
    point: [f64; 2],
) -> Result<bool, MapError> {
    if point.iter().any(|v| !v.is_finite()) {
        return Err(MapError::Invalid(
            "nonfinite native boundary membership point".into(),
        ));
    }
    validate(exterior)?;
    for ring in interiors {
        validate(ring)?;
    }
    let [x, y] = point;
    let [min_x, min_y, max_x, max_y] = bounds(exterior, interiors);
    let tolerance = FUZZY_TOLERANCE;
    if x + tolerance < min_x
        || x - tolerance > max_x
        || y + tolerance < min_y
        || y - tolerance > max_y
    {
        return Ok(false);
    }
    let mut hit = guarded_ring(exterior, point)?;
    // Native uses one overall bbox and every part's segments. A query outside
    // an individual hole bbox can still have ambiguous fuzzy vertex parity.
    for ring in interiors {
        hit ^= guarded_ring(ring, point)?;
    }
    Ok(hit)
}

/// Native plain X-ray membership for already validated finite storage polygons.
/// Boundary ties intentionally retain the strict native Y comparison, without
/// fuzzy admission or a physical-cell candidate-order qualification gate.
pub(crate) fn contains_polygon_plain(exterior: &Ring, interiors: &[Ring], point: [f64; 2]) -> bool {
    let [x, y] = point;
    let [min_x, min_y, max_x, max_y] = bounds(exterior, interiors);
    if x < min_x || x > max_x || y < min_y || y > max_y {
        return false;
    }
    // Native storage membership counts all parts with one odd/even parity.
    interiors
        .iter()
        .fold(plain_ring(exterior, point), |hit, ring| {
            hit ^ plain_ring(ring, point)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ring(points: &[[f64; 2]]) -> Ring {
        Ring(points.iter().map(|p| Coord::xy(p[0], p[1])).collect())
    }
    fn square() -> Ring {
        ring(&[[0., 0.], [10., 0.], [10., 10.], [0., 10.], [0., 0.]])
    }

    #[test]
    fn ordinary_unique_interior_and_unowned_points_are_admitted() {
        let exterior = square();
        assert!(contains_polygon(&exterior, &[], [5., 5.]).unwrap());
        assert!(!contains_polygon(&exterior, &[], [-1., 5.]).unwrap());
        assert!(!contains_polygon(&exterior, &[], [5., 11.]).unwrap());
        assert!(!contains_polygon(&exterior, &[], [11., 11.]).unwrap());
    }

    #[test]
    fn every_axis_edge_corner_and_near_edge_fuzzy_admission_refuses() {
        for point in [
            [0., 0.],
            [10., 10.],
            [5., 0.],
            [5., 10.],
            [0., 5.],
            [10., 5.],
            [5., 0.5e-5],
            [5., -0.5e-5],
            [-0.5e-5, 5.],
        ] {
            let error = contains_polygon(&square(), &[], point)
                .unwrap_err()
                .to_string();
            assert!(error.contains("fuzzy polygon edge"), "{point:?}: {error}");
        }
    }

    #[test]
    fn plain_storage_keeps_native_top_bottom_and_vertical_ties() {
        let exterior = square();
        assert!(!contains_polygon_plain(&exterior, &[], [5., 0.]));
        assert!(contains_polygon_plain(&exterior, &[], [5., 10.]));
        assert!(contains_polygon_plain(&exterior, &[], [0., 5.]));
        assert!(!contains_polygon_plain(&exterior, &[], [10., 5.]));
    }

    #[test]
    fn near_vertical_early_admission_refuses_even_when_interpolation_is_nan() {
        let exterior = ring(&[[0., 0.], [10., 0.], [10., 10.], [2e-6, 10.], [0., 0.]]);
        let a = Coord::xy(2e-6, 10.);
        let b = Coord::xy(0., 0.);
        assert!(interpolated_y(&a, &b, 8e-6).is_nan());
        assert!(
            contains_polygon(&exterior, &[], [8e-6, 5.])
                .unwrap_err()
                .to_string()
                .contains("fuzzy polygon edge")
        );
    }

    #[test]
    fn xray_through_local_vertex_extremum_refuses_parity_ambiguity() {
        let exterior = ring(&[[0., 10.], [5., 0.], [10., 10.], [0., 10.]]);
        assert!(contains_polygon_plain(&exterior, &[], [5., 5.]));
        assert!(
            contains_polygon(&exterior, &[], [5., 5.])
                .unwrap_err()
                .to_string()
                .contains("vertex parity")
        );
        assert!(contains_polygon(&exterior, &[], [4., 5.]).unwrap());
    }

    #[test]
    fn admitted_hole_interior_excludes_and_fuzzy_hole_edge_refuses() {
        let holes = [ring(&[[3., 3.], [7., 3.], [7., 7.], [3., 7.], [3., 3.]])];
        assert!(!contains_polygon(&square(), &holes, [5., 5.]).unwrap());
        assert!(contains_polygon(&square(), &holes, [2., 5.]).unwrap());
        assert!(contains_polygon(&square(), &holes, [3., 5.]).is_err());
        assert!(!contains_polygon_plain(&square(), &holes, [3., 5.]));
        assert!(contains_polygon_plain(&square(), &holes, [7., 5.]));
    }

    #[test]
    fn fuzzy_hole_segments_are_checked_outside_the_individual_hole_bbox() {
        let holes = [ring(&[[3., 3.], [5., 7.], [7., 3.], [3., 3.]])];
        // This point is above the hole but within the complete polygon. Native
        // expanded-X tests count both segments at the hole vertex, unlike the
        // strict plain X-ray. A per-hole bbox shortcut would conceal this tie.
        assert!(contains_polygon_plain(&square(), &holes, [5., 8.]));
        assert!(
            contains_polygon(&square(), &holes, [5., 8.])
                .unwrap_err()
                .to_string()
                .contains("vertex parity")
        );
    }

    #[test]
    fn plain_storage_retains_native_all_part_odd_even_parity() {
        let hole = ring(&[[3., 3.], [7., 3.], [7., 7.], [3., 7.], [3., 3.]]);
        // Duplicate raw parts cancel under the native even/odd rule. This test
        // specifies membership only; it does not relax model geometry admission.
        assert!(contains_polygon_plain(
            &square(),
            &[hole.clone(), hole],
            [5., 5.]
        ));
    }

    #[test]
    fn interpolation_preserves_native_weight_arithmetic_and_nan_policy() {
        let a = Coord::xy(2., 10.);
        let b = Coord::xy(8., 22.);
        let x = 3.;
        let weight = (b.x - x) / (b.x - a.x);
        assert_eq!(
            interpolated_y(&a, &b, x).to_bits(),
            (a.y * weight + b.y * (1. - weight)).to_bits()
        );
        assert_eq!(interpolated_y(&a, &b, 2.), 10.);
        assert_eq!(interpolated_y(&a, &b, 8.), 22.);
        assert!(interpolated_y(&a, &b, 1.).is_nan());
        assert!(interpolated_y(&a, &Coord::xy(2., 22.), 2.).is_nan());
    }

    #[test]
    fn open_closed_and_reversed_rings_agree_away_from_ties() {
        let open = ring(&[[0., 0.], [10., 0.], [10., 10.], [0., 10.]]);
        let mut reversed = square();
        reversed.0.reverse();
        for point in [[4., 5.], [15., 5.]] {
            assert_eq!(
                contains_polygon(&open, &[], point).unwrap(),
                contains_polygon(&reversed, &[], point).unwrap()
            );
        }
    }

    #[test]
    fn invalid_or_overflowing_geometry_refuses_without_allocation() {
        assert!(contains_polygon(&square(), &[], [f64::NAN, 1.]).is_err());
        assert!(contains_polygon(&ring(&[[0., 0.], [1., 0.]]), &[], [0., 0.]).is_err());
        assert!(
            contains_polygon(
                &ring(&[[0., 0.], [f64::INFINITY, 1.], [2., 0.]]),
                &[],
                [1., 0.]
            )
            .is_err()
        );
        let extreme = ring(&[[-f64::MAX, 0.], [f64::MAX, 0.], [0., 10.]]);
        assert!(contains_polygon(&extreme, &[], [0., 1.]).is_err());
    }
}
