//! Clipper 6's static members: `Area`, `Orientation`, `PointInPolygon`,
//! `GetBounds`, `ReversePaths`, `SimplifyPolygon(s)`, `MinkowskiSum`/`MinkowskiDiff`, and the
//! PolyTree-to-paths walks. `CleanPolygon(s)` (a modified version) lives in
//! `crate::clean_polygon`.

use super::engine::Clipper;
use super::poly_tree::PolyTree;
use super::types::{
    ClipType, ClipperError, IntPoint, IntRect, Path, Paths, PolyFillType, PolyType, ip,
};

/// The signed area, positive when the polygon is counter-clockwise (y up).
/// Fewer than three points is 0.
pub fn area(poly: &[IntPoint]) -> f64 {
    let cnt = poly.len();
    if cnt < 3 {
        return 0.0;
    }
    let mut a = 0.0;
    let mut j = cnt - 1;
    for i in 0..cnt {
        a += (poly[j].x as f64 + poly[i].x as f64) * (poly[j].y as f64 - poly[i].y as f64);
        j = i;
    }
    -a * 0.5
}

/// True when [`area`] is not negative.
pub fn orientation(poly: &[IntPoint]) -> bool {
    area(poly) >= 0.0
}

pub fn reverse_paths(polys: &mut Paths) {
    for poly in polys {
        poly.reverse();
    }
}

/// 0 outside, +1 inside, -1 on the boundary (Hormann & Agathos,
/// "The Point in Polygon Problem for Arbitrary Polygons").
pub fn point_in_polygon(pt: IntPoint, path: &[IntPoint]) -> i32 {
    let mut result = 0;
    let cnt = path.len();
    if cnt < 3 {
        return 0;
    }
    let mut ip = path[0];
    for i in 1..=cnt {
        let ip_next = if i == cnt { path[0] } else { path[i] };
        if ip_next.y == pt.y
            && (ip_next.x == pt.x || (ip.y == pt.y && ((ip_next.x > pt.x) == (ip.x < pt.x))))
        {
            return -1;
        }
        if (ip.y < pt.y) != (ip_next.y < pt.y) {
            if ip.x >= pt.x {
                if ip_next.x > pt.x {
                    result = 1 - result;
                } else {
                    let d = (ip.x - pt.x) as f64 * (ip_next.y - pt.y) as f64
                        - (ip_next.x - pt.x) as f64 * (ip.y - pt.y) as f64;
                    if d == 0.0 {
                        return -1;
                    } else if (d > 0.0) == (ip_next.y > ip.y) {
                        result = 1 - result;
                    }
                }
            } else if ip_next.x > pt.x {
                let d = (ip.x - pt.x) as f64 * (ip_next.y - pt.y) as f64
                    - (ip_next.x - pt.x) as f64 * (ip.y - pt.y) as f64;
                if d == 0.0 {
                    return -1;
                } else if (d > 0.0) == (ip_next.y > ip.y) {
                    result = 1 - result;
                }
            }
        }
        ip = ip_next;
    }
    result
}

/// All zero when every path is empty.
pub fn get_bounds(paths: &[Path]) -> IntRect {
    let mut points = paths.iter().flatten();
    let Some(first) = points.next() else {
        return IntRect::default();
    };
    let mut result = IntRect {
        left: first.x,
        top: first.y,
        right: first.x,
        bottom: first.y,
    };
    for point in points {
        if point.x < result.left {
            result.left = point.x;
        } else if point.x > result.right {
            result.right = point.x;
        }
        if point.y < result.top {
            result.top = point.y;
        } else if point.y > result.bottom {
            result.bottom = point.y;
        }
    }
    result
}

/// A strictly simple union of the polygon with
/// itself, which splits a self-intersecting polygon (or one touching itself at a vertex) into
/// simple ones.
pub fn simplify_polygon(poly: &[IntPoint], fill_type: PolyFillType) -> Result<Paths, ClipperError> {
    let mut result = Paths::new();
    let mut c = Clipper::new();
    c.strictly_simple = true;
    c.add_path(poly, PolyType::Subject, true)?;
    c.execute(ClipType::Union, &mut result, fill_type, fill_type)?;
    Ok(result)
}

pub fn simplify_polygons(polys: &[Path], fill_type: PolyFillType) -> Result<Paths, ClipperError> {
    let mut result = Paths::new();
    let mut c = Clipper::new();
    c.strictly_simple = true;
    c.add_paths(polys, PolyType::Subject, true)?;
    c.execute(ClipType::Union, &mut result, fill_type, fill_type)?;
    Ok(result)
}

/// The quads swept by `pattern` along `path`.
fn minkowski(pattern: &[IntPoint], path: &[IntPoint], is_sum: bool, is_closed: bool) -> Paths {
    let delta = usize::from(is_closed);
    let poly_cnt = pattern.len();
    let path_cnt = path.len();
    let result: Paths = path
        .iter()
        .map(|p| {
            pattern
                .iter()
                .map(|q| {
                    if is_sum {
                        ip(p.x + q.x, p.y + q.y)
                    } else {
                        ip(p.x - q.x, p.y - q.y)
                    }
                })
                .collect()
        })
        .collect();

    let mut quads = Paths::new();
    if path_cnt + delta < 1 {
        return quads;
    }
    for i in 0..path_cnt + delta - 1 {
        for j in 0..poly_cnt {
            let mut quad = vec![
                result[i % path_cnt][j % poly_cnt],
                result[(i + 1) % path_cnt][j % poly_cnt],
                result[(i + 1) % path_cnt][(j + 1) % poly_cnt],
                result[i % path_cnt][(j + 1) % poly_cnt],
            ];
            if !orientation(&quad) {
                quad.reverse();
            }
            quads.push(quad);
        }
    }
    quads
}

pub fn minkowski_sum(
    pattern: &[IntPoint],
    path: &[IntPoint],
    path_is_closed: bool,
) -> Result<Paths, ClipperError> {
    let mut paths = minkowski(pattern, path, true, path_is_closed);
    let mut c = Clipper::new();
    c.add_paths(&paths, PolyType::Subject, true)?;
    c.execute(
        ClipType::Union,
        &mut paths,
        PolyFillType::NonZero,
        PolyFillType::NonZero,
    )?;
    Ok(paths)
}

pub fn minkowski_sum_paths(
    pattern: &[IntPoint],
    paths: &[Path],
    path_is_closed: bool,
) -> Result<Paths, ClipperError> {
    let mut solution = Paths::new();
    let mut c = Clipper::new();
    for path in paths {
        let tmp = minkowski(pattern, path, true, path_is_closed);
        c.add_paths(&tmp, PolyType::Subject, true)?;
        if path_is_closed {
            let translated: Path = path
                .iter()
                .map(|p| ip(p.x + pattern[0].x, p.y + pattern[0].y))
                .collect();
            c.add_path(&translated, PolyType::Clip, true)?;
        }
    }
    c.execute(
        ClipType::Union,
        &mut solution,
        PolyFillType::NonZero,
        PolyFillType::NonZero,
    )?;
    Ok(solution)
}

pub fn minkowski_diff(poly1: &[IntPoint], poly2: &[IntPoint]) -> Result<Paths, ClipperError> {
    let mut paths = minkowski(poly1, poly2, false, true);
    let mut c = Clipper::new();
    c.add_paths(&paths, PolyType::Subject, true)?;
    c.execute(
        ClipType::Union,
        &mut paths,
        PolyFillType::NonZero,
        PolyFillType::NonZero,
    )?;
    Ok(paths)
}

/// `closed_only` is `ntClosed`, otherwise `ntAny`.
fn add_poly_node_to_paths(tree: &PolyTree, node: usize, closed_only: bool, paths: &mut Paths) {
    let match_ = !closed_only || !tree.nodes[node].is_open;
    if !tree.nodes[node].polygon.is_empty() && match_ {
        paths.push(tree.nodes[node].polygon.clone());
    }
    for &child in &tree.nodes[node].childs {
        add_poly_node_to_paths(tree, child, closed_only, paths);
    }
}

/// Every contour, parents before children.
pub fn poly_tree_to_paths(polytree: &PolyTree) -> Paths {
    let mut result = Paths::new();
    add_poly_node_to_paths(polytree, PolyTree::ROOT, false, &mut result);
    result
}

/// Every closed contour, parents before children.
pub fn closed_paths_from_poly_tree(polytree: &PolyTree) -> Paths {
    let mut result = Paths::new();
    add_poly_node_to_paths(polytree, PolyTree::ROOT, true, &mut result);
    result
}

/// The open contours (all top level).
pub fn open_paths_from_poly_tree(polytree: &PolyTree) -> Paths {
    polytree
        .childs(PolyTree::ROOT)
        .iter()
        .filter(|&&child| polytree.nodes[child].is_open)
        .map(|&child| polytree.nodes[child].polygon.clone())
        .collect()
}
