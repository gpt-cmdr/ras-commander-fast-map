//! Group the unchanged rings of a flat native Clipper result for GeoJSON.
//!
//! The input is a completed flat clipping solution, not arbitrary intersecting
//! polygon soup. Positive rings are exteriors and negative rings are holes.
//! No clipping, coordinate conversion, repair or vertex removal occurs here.
use crate::{MapError, boundary_contour::Limits};
use clipper2_rust::{Path64, Paths64, Point64};

type Result<T> = std::result::Result<T, MapError>;
const NONE: usize = usize::MAX;
const LEAF_SIZE: usize = 8;
const THIN_ISLAND: &str =
    "native thin-hole removal with nested islands has no qualified valid vector representation";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Group {
    pub shell: usize,
    pub holes: Vec<usize>,
}

#[derive(Clone, Copy)]
struct Bounds {
    min_x: i64,
    min_y: i64,
    max_x: i64,
    max_y: i64,
}

impl Bounds {
    fn ring(path: &Path64) -> Self {
        let p = path[0];
        let mut bounds = Self {
            min_x: p.x,
            min_y: p.y,
            max_x: p.x,
            max_y: p.y,
        };
        for p in &path[1..] {
            bounds.min_x = bounds.min_x.min(p.x);
            bounds.min_y = bounds.min_y.min(p.y);
            bounds.max_x = bounds.max_x.max(p.x);
            bounds.max_y = bounds.max_y.max(p.y);
        }
        bounds
    }

    fn union(self, other: Self) -> Self {
        Self {
            min_x: self.min_x.min(other.min_x),
            min_y: self.min_y.min(other.min_y),
            max_x: self.max_x.max(other.max_x),
            max_y: self.max_y.max(other.max_y),
        }
    }

    fn contains(self, other: Self) -> bool {
        self.min_x <= other.min_x
            && self.min_y <= other.min_y
            && self.max_x >= other.max_x
            && self.max_y >= other.max_y
    }

    fn center_key(self, x_axis: bool) -> i128 {
        // Twice the center; neither a floating midpoint nor i64 addition.
        if x_axis {
            i128::from(self.min_x) + i128::from(self.max_x)
        } else {
            i128::from(self.min_y) + i128::from(self.max_y)
        }
    }
}

struct Ring {
    bounds: Bounds,
    area2: i128,
}

struct Node {
    bounds: Bounds,
    first: usize,
    end: usize,
    left: usize,
    right: usize,
}

/// A conservative integer bbox BVH. Its leaves retain original ring indices.
struct Index {
    order: Vec<usize>,
    nodes: Vec<Node>,
}

fn budget() -> MapError {
    MapError::Budget("native ring grouping planned memory exceeds budget".into())
}

fn invalid(message: &str) -> MapError {
    MapError::Invalid(message.into())
}

fn unsupported(message: &str) -> MapError {
    MapError::Unsupported(message.into())
}

fn reserved<T>(capacity: usize) -> Result<Vec<T>> {
    let mut out = Vec::new();
    out.try_reserve_exact(capacity).map_err(|_| budget())?;
    Ok(out)
}

fn admission(vertices: usize, rings: usize, limits: Limits) -> Result<()> {
    // Keep the existing 256 B/vertex contour allowance. 512 B/ring covers
    // Ring metadata, <=2N 64-byte BVH nodes, index/order/query/candidate scratch,
    // parent/count arrays, Group/Vec headers, hole storage and allocator overhead.
    // All Vec capacities are bounded before allocation; hole Vecs are counted
    // before reserving, so none relies on amortized growth. The fixed allowance
    // includes the balanced build/sort call stacks (depth <= usize::BITS).
    let bytes = vertices
        .checked_mul(256)
        .and_then(|n| rings.checked_mul(512).and_then(|r| n.checked_add(r)))
        .and_then(|n| n.checked_add(65_536))
        .ok_or_else(budget)?;
    if bytes > limits.max_working_bytes {
        return Err(budget());
    }
    Ok(())
}

fn cross(a: Point64, b: Point64, p: Point64) -> Result<i128> {
    let bx = i128::from(b.x) - i128::from(a.x);
    let by = i128::from(b.y) - i128::from(a.y);
    let px = i128::from(p.x) - i128::from(a.x);
    let py = i128::from(p.y) - i128::from(a.y);
    bx.checked_mul(py)
        .and_then(|x| by.checked_mul(px).and_then(|y| x.checked_sub(y)))
        .ok_or_else(|| invalid("native ring integer predicate overflow"))
}

fn signed_area2(path: &Path64) -> Result<i128> {
    if path.len() < 3 {
        return Err(invalid("degenerate native contour ring"));
    }
    // A translated triangle fan avoids products of large global coordinates.
    // Checked intermediate sums deliberately refuse unrepresentable geometry.
    let mut area = 0_i128;
    for pair in path[1..].windows(2) {
        area = area
            .checked_add(cross(path[0], pair[0], pair[1])?)
            .ok_or_else(|| invalid("native ring integer area overflow"))?;
    }
    if area == 0 {
        return Err(invalid("zero area native contour ring"));
    }
    Ok(area)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Position {
    Outside,
    Inside,
    Boundary,
}

fn point_in_ring(point: Point64, path: &Path64) -> Result<Position> {
    let mut inside = false;
    let mut previous = path[path.len() - 1];
    for &next in path {
        let min_x = previous.x.min(next.x);
        let max_x = previous.x.max(next.x);
        let in_y = previous.y.min(next.y) <= point.y && point.y <= previous.y.max(next.y);
        let in_x = min_x <= point.x && point.x <= max_x;
        let orientation = if in_x && in_y {
            let value = cross(previous, next, point)?;
            if value == 0 {
                return Ok(Position::Boundary);
            }
            Some(value)
        } else {
            None
        };
        // Half-open vertical crossing parity, entirely in integer arithmetic.
        if (previous.y > point.y) != (next.y > point.y) {
            if point.x < min_x {
                inside = !inside;
            } else if point.x <= max_x {
                let orientation = match orientation {
                    Some(value) => value,
                    None => cross(previous, next, point)?,
                };
                if (orientation > 0) == (next.y > previous.y) {
                    inside = !inside;
                }
            }
        }
        previous = next;
    }
    Ok(if inside {
        Position::Inside
    } else {
        Position::Outside
    })
}

fn ring_in_ring(child: usize, parent: usize, paths: &Paths64, rings: &[Ring]) -> Result<Position> {
    if !rings[parent].bounds.contains(rings[child].bounds) {
        return Ok(Position::Outside);
    }
    // Flat clipping rings do not cross. A first vertex can touch a parent's
    // boundary; continue through stored vertices until a strict relation is
    // available. Identical/boundary-only rings have no defensible parent.
    for &point in &paths[child] {
        let position = point_in_ring(point, &paths[parent])?;
        if position != Position::Boundary {
            return Ok(position);
        }
    }
    Ok(Position::Boundary)
}

impl Index {
    fn new(rings: &[Ring]) -> Result<Self> {
        let mut order = reserved(rings.len())?;
        order.extend(0..rings.len());
        let nodes = reserved(rings.len().checked_mul(2).ok_or_else(budget)?)?;
        let mut index = Self { order, nodes };
        if !rings.is_empty() {
            index.build(0, rings.len(), rings);
        }
        Ok(index)
    }

    fn build(&mut self, first: usize, end: usize, rings: &[Ring]) -> usize {
        let mut bounds = rings[self.order[first]].bounds;
        for &ring in &self.order[first + 1..end] {
            bounds = bounds.union(rings[ring].bounds);
        }
        let id = self.nodes.len();
        self.nodes.push(Node {
            bounds,
            first,
            end,
            left: NONE,
            right: NONE,
        });
        if end - first > LEAF_SIZE {
            let x_axis = i128::from(bounds.max_x) - i128::from(bounds.min_x)
                >= i128::from(bounds.max_y) - i128::from(bounds.min_y);
            self.order[first..end]
                .sort_unstable_by_key(|&i| (rings[i].bounds.center_key(x_axis), i));
            let middle = first + (end - first) / 2;
            let left = self.build(first, middle, rings);
            let right = self.build(middle, end, rings);
            self.nodes[id].left = left;
            self.nodes[id].right = right;
        }
        id
    }

    fn containers(
        &self,
        query: Bounds,
        rings: &[Ring],
        stack: &mut Vec<usize>,
        mut visit: impl FnMut(usize) -> Result<()>,
    ) -> Result<()> {
        stack.clear();
        if self.nodes.is_empty() {
            return Ok(());
        }
        stack.push(0);
        while let Some(id) = stack.pop() {
            let node = &self.nodes[id];
            if !node.bounds.contains(query) {
                continue;
            }
            if node.left == NONE {
                for &ring in &self.order[node.first..node.end] {
                    if rings[ring].bounds.contains(query) {
                        visit(ring)?;
                    }
                }
            } else {
                stack.push(node.right);
                stack.push(node.left);
            }
        }
        Ok(())
    }
}

/// Associate retained CW holes with their nearest containing retained CCW shell.
/// The output refers to input indices; every original vertex remains untouched.
pub(crate) fn group(paths: &Paths64, kept: &[bool], limits: Limits) -> Result<Vec<Group>> {
    if paths.len() != kept.len() {
        return Err(invalid("native contour kept-mask length differs"));
    }
    let vertices = paths
        .iter()
        .try_fold(0_usize, |n, p| n.checked_add(p.len()).ok_or_else(budget))?;
    admission(vertices, paths.len(), limits)?;

    let mut rings = reserved(paths.len())?;
    for path in paths {
        let area2 = signed_area2(path)?;
        rings.push(Ring {
            bounds: Bounds::ring(path),
            area2,
        });
    }
    let index = Index::new(&rings)?;
    let mut stack = reserved(paths.len().checked_mul(2).ok_or_else(budget)?)?;
    let mut parents = reserved(paths.len())?;
    parents.resize(paths.len(), NONE);
    let mut counts = reserved(paths.len())?;
    counts.resize(paths.len(), 0_usize);
    let mut candidates = reserved(paths.len())?;

    for (child, ring) in rings.iter().enumerate() {
        if !kept[child] {
            continue;
        }
        if ring.area2 > 0 {
            index.containers(ring.bounds, &rings, &mut stack, |candidate| {
                if !kept[candidate] && rings[candidate].area2 < 0 {
                    match ring_in_ring(child, candidate, paths, &rings)? {
                        Position::Inside => return Err(unsupported(THIN_ISLAND)),
                        Position::Boundary => {
                            return Err(unsupported("native contour containment is boundary-only"));
                        }
                        Position::Outside => {}
                    }
                }
                Ok(())
            })?;
            continue;
        }

        candidates.clear();
        index.containers(ring.bounds, &rings, &mut stack, |candidate| {
            if !kept[candidate] || rings[candidate].area2 < 0 {
                return Ok(());
            }
            match ring_in_ring(child, candidate, paths, &rings)? {
                Position::Outside => return Ok(()),
                Position::Boundary => {
                    return Err(unsupported("native contour containment is boundary-only"));
                }
                Position::Inside => {}
            }
            candidates.push(candidate);
            Ok(())
        })?;
        if candidates.is_empty() {
            return Err(unsupported(
                "native contour hole has no containing retained shell",
            ));
        }
        candidates.sort_unstable_by_key(|&i| (rings[i].area2, i));
        // Valid flat solutions have a strict chain of enclosing exteriors.
        // Check the whole chain, including larger duplicate ancestors that a
        // nearest-only reduction would overlook after finding a smaller shell.
        for pair in candidates.windows(2) {
            let (inner, outer) = (pair[0], pair[1]);
            if rings[inner].area2 == rings[outer].area2 {
                return Err(unsupported(
                    "native contour hole has ambiguous containing shells",
                ));
            }
            if ring_in_ring(inner, outer, paths, &rings)? != Position::Inside {
                return Err(unsupported(
                    "native contour hole has ambiguous containing shells",
                ));
            }
        }
        let parent = candidates[0];
        parents[child] = parent;
        counts[parent] = counts[parent].checked_add(1).ok_or_else(budget)?;
    }

    let shells = rings
        .iter()
        .enumerate()
        .filter(|(i, ring)| kept[*i] && ring.area2 > 0)
        .count();
    let mut groups = reserved(shells)?;
    let mut group_indices = reserved(paths.len())?;
    group_indices.resize(paths.len(), NONE);
    for (shell, ring) in rings.iter().enumerate() {
        if kept[shell] && ring.area2 > 0 {
            group_indices[shell] = groups.len();
            groups.push(Group {
                shell,
                holes: reserved(counts[shell])?,
            });
        }
    }
    for (hole, &parent) in parents.iter().enumerate() {
        if parent == NONE {
            continue;
        }
        let group = &mut groups[group_indices[parent]];
        if group.holes.len() >= counts[parent] {
            return Err(MapError::Internal(
                "native ring grouping count differs".into(),
            ));
        }
        group.holes.push(hole);
    }
    Ok(groups)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: Limits = Limits {
        max_working_bytes: 32 * 1024 * 1024,
    };

    fn ring(points: &[[i64; 2]]) -> Path64 {
        points.iter().map(|p| Point64::new(p[0], p[1])).collect()
    }

    fn square(x: i64, y: i64, size: i64, positive: bool) -> Path64 {
        let mut path = ring(&[[x, y], [x + size, y], [x + size, y + size], [x, y + size]]);
        if !positive {
            path.reverse();
        }
        path
    }

    #[test]
    fn donut_preserves_input_indices() {
        let paths = vec![square(0, 0, 20, true), square(3, 3, 5, false)];
        assert_eq!(
            group(&paths, &[true, true], LIMITS).unwrap(),
            vec![Group {
                shell: 0,
                holes: vec![1]
            }]
        );
    }

    #[test]
    fn multiple_exteriors_and_hole_order_are_deterministic() {
        let paths = vec![
            square(50, 0, 20, true),
            square(0, 0, 20, true),
            square(52, 2, 3, false),
            square(2, 2, 3, false),
            square(58, 8, 3, false),
        ];
        let expected = vec![
            Group {
                shell: 0,
                holes: vec![2, 4],
            },
            Group {
                shell: 1,
                holes: vec![3],
            },
        ];
        for _ in 0..3 {
            assert_eq!(group(&paths, &[true; 5], LIMITS).unwrap(), expected);
        }
    }

    #[test]
    fn nested_island_hole_chooses_nearest_shell() {
        let paths = vec![
            square(0, 0, 100, true),
            square(10, 10, 80, false),
            square(20, 20, 60, true),
            square(30, 30, 40, false),
        ];
        assert_eq!(
            group(&paths, &[true; 4], LIMITS).unwrap(),
            vec![
                Group {
                    shell: 0,
                    holes: vec![1]
                },
                Group {
                    shell: 2,
                    holes: vec![3]
                }
            ]
        );
    }

    #[test]
    fn many_holes_use_bounded_index_and_complete_counts() {
        let mut paths = vec![square(0, 0, 1000, true)];
        for y in 0..20 {
            for x in 0..20 {
                paths.push(square(10 + x * 40, 10 + y * 40, 5, false));
            }
        }
        let groups = group(&paths, &vec![true; paths.len()], LIMITS).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].holes, (1..paths.len()).collect::<Vec<_>>());
    }

    #[test]
    fn concave_bbox_false_candidate_does_not_capture_hole() {
        let paths = vec![
            ring(&[[0, 0], [10, 0], [10, 3], [3, 3], [3, 10], [0, 10]]),
            square(5, 5, 4, true),
            square(6, 6, 1, false),
        ];
        assert_eq!(
            group(&paths, &[true; 3], LIMITS).unwrap(),
            vec![
                Group {
                    shell: 0,
                    holes: vec![]
                },
                Group {
                    shell: 1,
                    holes: vec![2]
                }
            ]
        );
    }

    #[test]
    fn first_boundary_vertex_is_followed_to_strict_inside() {
        let paths = vec![square(0, 0, 10, true), ring(&[[0, 5], [2, 7], [2, 3]])];
        assert_eq!(group(&paths, &[true; 2], LIMITS).unwrap()[0].holes, vec![1]);
    }

    #[test]
    fn boundary_only_and_identical_parents_refuse() {
        let paths = vec![square(0, 0, 10, true), square(0, 0, 10, false)];
        assert!(matches!(
            group(&paths, &[true; 2], LIMITS),
            Err(MapError::Unsupported(_))
        ));
        let paths = vec![
            square(0, 0, 10, true),
            square(0, 0, 10, true),
            square(2, 2, 3, false),
        ];
        assert!(matches!(
            group(&paths, &[true; 3], LIMITS),
            Err(MapError::Unsupported(_))
        ));
    }

    #[test]
    fn unequal_overlapping_parents_refuse() {
        let paths = vec![
            square(0, 0, 10, true),
            square(5, -5, 11, true),
            square(6, 1, 2, false),
        ];
        assert!(matches!(
            group(&paths, &[true; 3], LIMITS),
            Err(MapError::Unsupported(_))
        ));
    }

    #[test]
    fn duplicate_larger_ancestors_refuse_even_after_a_nearer_shell() {
        let paths = vec![
            square(20, 20, 20, true),
            square(0, 0, 100, true),
            square(0, 0, 100, true),
            square(25, 25, 3, false),
        ];
        assert!(matches!(
            group(&paths, &[true; 4], LIMITS),
            Err(MapError::Unsupported(_))
        ));
    }

    #[test]
    fn orphan_hole_refuses() {
        let paths = vec![square(0, 0, 10, true), square(20, 20, 3, false)];
        assert!(matches!(
            group(&paths, &[true; 2], LIMITS),
            Err(MapError::Unsupported(_))
        ));
    }

    #[test]
    fn dropped_hole_with_kept_island_uses_existing_refusal() {
        let paths = vec![
            square(0, 0, 100, true),
            square(10, 10, 80, false),
            square(20, 20, 60, true),
        ];
        assert_eq!(
            group(&paths, &[true, false, true], LIMITS),
            Err(MapError::Unsupported(THIN_ISLAND.into()))
        );
    }

    #[test]
    fn dropped_hole_without_island_is_omitted() {
        let paths = vec![square(0, 0, 20, true), square(3, 3, 5, false)];
        assert_eq!(
            group(&paths, &[true, false], LIMITS).unwrap(),
            vec![Group {
                shell: 0,
                holes: vec![]
            }]
        );
    }

    #[test]
    fn short_zero_area_and_overflow_refuse() {
        for path in [
            ring(&[[0, 0], [1, 1]]),
            ring(&[[0, 0], [1, 1], [2, 2]]),
            ring(&[
                [i64::MIN, i64::MIN],
                [i64::MAX, i64::MIN],
                [i64::MIN, i64::MAX],
            ]),
        ] {
            assert!(matches!(
                group(&vec![path], &[true], LIMITS),
                Err(MapError::Invalid(_))
            ));
        }
        assert!(
            cross(
                Point64::new(i64::MIN, i64::MIN),
                Point64::new(i64::MAX, i64::MIN),
                Point64::new(i64::MIN, i64::MAX)
            )
            .is_err()
        );
    }

    #[test]
    fn translated_area_accepts_small_rings_near_integer_limit() {
        let paths = vec![
            square(i64::MAX - 20, i64::MAX - 20, 10, true),
            square(i64::MAX - 18, i64::MAX - 18, 3, false),
        ];
        assert_eq!(group(&paths, &[true; 2], LIMITS).unwrap()[0].holes, vec![1]);
    }

    #[test]
    fn memory_is_admitted_before_index_allocation() {
        let paths = vec![square(0, 0, 10, true)];
        let exact = 65_536 + 4 * 256 + 512;
        assert!(matches!(
            group(
                &paths,
                &[true],
                Limits {
                    max_working_bytes: exact - 1
                }
            ),
            Err(MapError::Budget(_))
        ));
        assert!(
            group(
                &paths,
                &[true],
                Limits {
                    max_working_bytes: exact
                }
            )
            .is_ok()
        );
        assert!(matches!(
            admission(usize::MAX, 0, LIMITS),
            Err(MapError::Budget(_))
        ));
        assert!(matches!(
            admission(0, usize::MAX, LIMITS),
            Err(MapError::Budget(_))
        ));
    }

    #[test]
    fn ring_allowance_covers_structs_arrays_headers_and_growth() {
        let per_ring = std::mem::size_of::<Ring>()
            + 2 * std::mem::size_of::<Node>()
            + std::mem::size_of::<Group>()
            // Order, 2N query stack, candidate, parent, count, group index, hole.
            + 8 * std::mem::size_of::<usize>()
            // Conservative allowance for an allocation for every hole Vec.
            + 32;
        assert!(per_ring <= 512);
        assert!(std::mem::size_of::<Index>() + 8 * std::mem::size_of::<Vec<usize>>() < 65_536);
    }

    #[test]
    fn kept_mask_and_empty_inputs_are_checked() {
        assert!(matches!(
            group(&vec![square(0, 0, 10, true)], &[], LIMITS),
            Err(MapError::Invalid(_))
        ));
        assert!(group(&vec![], &[], LIMITS).unwrap().is_empty());
        assert!(
            group(&vec![square(0, 0, 10, true)], &[false], LIMITS)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn every_stored_vertex_including_collinear_and_closure_is_unchanged() {
        let paths = vec![
            ring(&[[0, 0], [5, 0], [10, 0], [10, 10], [0, 10], [0, 0]]),
            ring(&[[2, 2], [2, 4], [2, 6], [6, 6], [6, 2], [2, 2]]),
        ];
        let original = paths.clone();
        let groups = group(&paths, &[true; 2], LIMITS).unwrap();
        assert_eq!(
            groups,
            vec![Group {
                shell: 0,
                holes: vec![1]
            }]
        );
        assert_eq!(paths, original);
    }
}
