//! Serial native contour semantics, isolated from deterministic contour method /1.
//! This method retains ordered Clipper 6 operands; native qualification is scoped
//! separately from this transport/kernel implementation.
use crate::boundary_clip_legacy::{ClipType, Clipper, PolyFillType, PolyType};
use crate::{MapError, boundary_contour as contour};
use clipper2_rust::{Path64, Paths64};
use contour::{Grid, Limits, MultiPolygon, Output, Unit};

pub const METHOD_ID: &str = "rasmapper-serial-zero-depth-contours/1";
type Result<T> = std::result::Result<T, MapError>;

fn invalid(message: &str) -> MapError {
    MapError::Invalid(message.into())
}
fn engine_error(error: impl std::fmt::Display) -> MapError {
    MapError::Invalid(format!("native contour union failed: {error}"))
}
fn add_partition(clipper: &mut Clipper, paths: &Paths64) -> Result<bool> {
    let mut any = false;
    for (index, path) in paths.iter().enumerate() {
        // A rejected first path still consumes the native subject position.
        any |= clipper
            .add_path(
                path,
                if index == 0 {
                    PolyType::Subject
                } else {
                    PolyType::Clip
                },
                true,
            )
            .map_err(engine_error)?;
    }
    Ok(any)
}
fn union(paths: &Paths64, limits: Limits) -> Result<Paths64> {
    limits.admit(contour::count_vertices(paths)?, paths.len())?;
    let mut clipper = Clipper::new();
    let any = add_partition(&mut clipper, paths)?;
    let mut output = vec![];
    let success = clipper
        .execute(
            ClipType::Union,
            &mut output,
            PolyFillType::NonZero,
            PolyFillType::NonZero,
        )
        .map_err(engine_error)?;
    if any && !success {
        return Err(invalid(
            "native contour union could not order intersections",
        ));
    }
    limits.admit(contour::count_vertices(&output)?, output.len())?;
    Ok(output)
}

#[derive(Clone, Copy)]
struct Sample {
    xy: [f64; 2],
    z: f32,
}
struct Fragments {
    paths: Paths64,
    vertices: usize,
    limits: Limits,
}
impl Fragments {
    fn new(limits: Limits) -> Self {
        Self {
            paths: vec![],
            vertices: 0,
            limits,
        }
    }
    fn push(&mut self, coordinates: &[[f64; 2]]) -> Result<()> {
        self.vertices = self
            .vertices
            .checked_add(coordinates.len())
            .ok_or_else(|| MapError::Budget("native contour vertex count overflow".into()))?;
        self.limits.admit(self.vertices, self.paths.len() + 1)?;
        let path = coordinates
            .iter()
            .copied()
            .map(contour::quantize)
            .collect::<Result<Path64>>()?;
        // Retain triangle winding, rejected-path positions, and emission order.
        self.paths.push(path);
        Ok(())
    }
    fn complete(self, sequence: u64) -> Result<Unit> {
        Ok(Unit {
            sequence,
            paths: union(&self.paths, self.limits)?,
        })
    }
}
fn sample(xy: [f64; 2], z: Option<f32>) -> Option<Sample> {
    z.filter(|z| *z != -9999.).map(|z| Sample { xy, z })
}
fn check_field(values: &[Option<f32>]) -> Result<()> {
    if values.iter().flatten().any(|value| !value.is_finite()) {
        return Err(invalid("nonfinite native contour sample"));
    }
    Ok(())
}
fn edge(a: Sample, b: Sample, coordinates: &mut Vec<[f64; 2]>) -> Result<()> {
    if a.z >= 0. {
        coordinates.push(a.xy);
    }
    if (a.z >= 0.) != (b.z >= 0.) {
        let span = b.z - a.z;
        let fraction = f64::from((0f32 - a.z) / span);
        if !span.is_finite() || !fraction.is_finite() {
            return Err(invalid("native contour crossing overflow"));
        }
        coordinates.push([
            a.xy[0] + (b.xy[0] - a.xy[0]) * fraction,
            a.xy[1] + (b.xy[1] - a.xy[1]) * fraction,
        ]);
    }
    Ok(())
}
fn triangle(values: [Sample; 3], fragments: &mut Fragments) -> Result<()> {
    let min = values.iter().map(|v| v.z).fold(f32::INFINITY, f32::min);
    let max = values.iter().map(|v| v.z).fold(f32::NEG_INFINITY, f32::max);
    if max < 0. || min >= f32::MAX {
        return Ok(());
    }
    let mut coordinates = Vec::with_capacity(4);
    for index in 0..3 {
        edge(values[index], values[(index + 1) % 3], &mut coordinates)?;
    }
    fragments.push(&coordinates)
}
fn block(values: [Option<Sample>; 4], center: [f64; 2], fragments: &mut Fragments) -> Result<()> {
    let count = values.iter().flatten().count();
    if count < 3 {
        return Ok(());
    }
    if count == 4 && values.iter().flatten().all(|v| v.z < 0.) {
        return Ok(());
    }
    if values.iter().flatten().all(|v| v.z == f32::MAX) {
        return Ok(());
    }
    let sum = if let [Some(a), Some(b), Some(c), Some(d)] = values {
        a.z + b.z + c.z + d.z
    } else {
        values
            .iter()
            .flatten()
            .fold(0f32, |sum, value| sum + value.z)
    };
    let z = if count == 4 {
        (f64::from(sum) * 0.25) as f32
    } else {
        sum / 3f32
    };
    if !z.is_finite() {
        return Err(invalid("native contour center overflow"));
    }
    let middle = Sample { xy: center, z };
    for index in 0..4 {
        if let (Some(a), Some(b)) = (values[index], values[(index + 1) % 4]) {
            triangle([b, a, middle], fragments)?;
        }
    }
    Ok(())
}

/// One serial native terrain tile. Numeric -9999 samples are native nodata.
/// Partially wet triangles precede all fully wet runs within each row.
pub fn grid(sequence: u64, grid: Grid, depth: &[Option<f32>], limits: Limits) -> Result<Unit> {
    grid.validate(depth.len())?;
    check_field(depth)?;
    contour::quantize(grid.point(0, 0))?;
    contour::quantize(grid.point(grid.rows - 1, grid.cols - 1))?;
    limits.admit(0, 0)?;
    let mut fragments = Fragments::new(limits);
    for row in 0..grid.rows - 1 {
        let mut runs = vec![];
        let mut column = 0;
        let wet = |column: usize| {
            let index = row * grid.cols + column;
            [index, index + 1, index + grid.cols + 1, index + grid.cols]
                .iter()
                .all(|&index| depth[index].is_some_and(|v| (0. ..f32::MAX).contains(&v)))
        };
        while column < grid.cols - 1 {
            if wet(column) {
                let start = column;
                while column + 1 < grid.cols - 1 && wet(column + 1) {
                    column += 1;
                }
                limits.admit(fragments.vertices, fragments.paths.len() + runs.len() + 1)?;
                runs.push((start, column));
            } else {
                let index = row * grid.cols + column;
                block(
                    [
                        sample(grid.point(row, column), depth[index]),
                        sample(grid.point(row, column + 1), depth[index + 1]),
                        sample(
                            grid.point(row + 1, column + 1),
                            depth[index + grid.cols + 1],
                        ),
                        sample(grid.point(row + 1, column), depth[index + grid.cols]),
                    ],
                    grid.center(row, column),
                    &mut fragments,
                )?;
            }
            column += 1;
        }
        for (first, last) in runs {
            fragments.push(&[
                grid.point(row, first),
                grid.point(row + 1, first),
                grid.point(row + 1, last + 1),
                grid.point(row, last + 1),
            ])?;
        }
    }
    fragments.complete(sequence)
}

#[derive(Clone, Copy)]
struct TinBins {
    rows: usize,
    cols: usize,
    left: f64,
    right: f64,
    top: f64,
    bottom: f64,
    step: f64,
}
impl TinBins {
    fn new(xy: &[[f64; 2]], count: usize) -> Result<Self> {
        let mut lo = [f64::INFINITY; 2];
        let mut hi = [f64::NEG_INFINITY; 2];
        for point in xy {
            for axis in 0..2 {
                lo[axis] = lo[axis].min(point[axis]);
                hi[axis] = hi[axis].max(point[axis]);
            }
        }
        let mut maximum = ((count as f64 / 1024. / 4.).min(64.))
            .round_ties_even()
            .max(1.) as usize;
        if lo == hi {
            maximum = 1;
        }
        for axis in 0..2 {
            if lo[axis] == hi[axis] {
                if lo[axis] == 0. {
                    hi[axis] += 0.01;
                    lo[axis] -= 0.01;
                } else if lo[axis] > 0. {
                    hi[axis] *= 1.01;
                    lo[axis] *= 0.99;
                } else {
                    hi[axis] *= 0.99;
                    lo[axis] *= 1.01;
                }
            }
        }
        let width = hi[0] - lo[0];
        let height = hi[1] - lo[1];
        let aspect = width / height;
        if !aspect.is_finite() || aspect <= 0. {
            return Err(invalid("invalid native stitch bin aspect"));
        }
        let (rows, cols) = if maximum <= 1 {
            (1, 1)
        } else {
            let root = (maximum as f64 / aspect).sqrt();
            if !root.is_finite() {
                return Err(invalid("native stitch bin dimensions overflow"));
            }
            let rows = root.floor() as usize;
            let cols = (root * aspect).floor() as usize;
            if rows == 0 {
                (1, maximum)
            } else if cols == 0 {
                (maximum, 1)
            } else {
                (rows, cols)
            }
        };
        if rows > maximum || cols > maximum || rows * cols > 64 {
            return Err(invalid("invalid native stitch bin dimensions"));
        }
        let step = (height / rows as f64).max(width / cols as f64) * 1.01;
        let center_x = lo[0] + 0.5 * width;
        let center_y = lo[1] + 0.5 * height;
        let half_x = 0.5 * cols as f64 * step;
        let half_y = 0.5 * rows as f64 * step;
        let (left, right, top, bottom) = (
            center_x - half_x,
            center_x + half_x,
            center_y + half_y,
            center_y - half_y,
        );
        // Native Raster.Assign derives its step again from recentered bounds.
        let step = ((right - left) / cols as f64).max((top - bottom) / rows as f64);
        if !step.is_finite() || step <= 0. {
            return Err(invalid("invalid native stitch bin step"));
        }
        Ok(Self {
            rows,
            cols,
            left,
            right,
            top,
            bottom,
            step,
        })
    }
    fn range(self, xy: &[[f64; 2]], triangle: &[u32; 3]) -> Result<[usize; 4]> {
        let mut lo = [f64::INFINITY; 2];
        let mut hi = [f64::NEG_INFINITY; 2];
        for &index in triangle {
            let point = xy
                .get(index as usize)
                .ok_or_else(|| invalid("native stitch triangle index out of range"))?;
            for axis in 0..2 {
                lo[axis] = lo[axis].min(point[axis]);
                hi[axis] = hi[axis].max(point[axis]);
            }
        }
        let c0 = if lo[0] <= self.left {
            0
        } else {
            ((lo[0] - self.left) / self.step).floor() as usize
        };
        let c1 = if hi[0] >= self.right {
            self.cols - 1
        } else {
            ((hi[0] - self.left) / self.step).floor() as usize
        };
        let r0 = if hi[1] >= self.top {
            0
        } else {
            ((self.top - hi[1]) / self.step).floor() as usize
        };
        let r1 = if lo[1] <= self.bottom {
            self.rows - 1
        } else {
            ((self.top - lo[1]) / self.step).floor() as usize
        };
        if c0 > c1 || r0 > r1 || c1 >= self.cols || r1 >= self.rows {
            return Err(invalid("native stitch bin range outside layout"));
        }
        Ok([r0, r1, c0, c1])
    }
}

/// Native stitch TIN spatial bins: original triangle order in every overlapping
/// bbox bin, row-major bins, separate per-bin unions and no extra unit union.
pub fn tin(
    sequence: u64,
    xy: &[[f64; 2]],
    depth: &[Option<f32>],
    triangles: &[[u32; 3]],
    limits: Limits,
) -> Result<Unit> {
    limits.admit(0, 0)?;
    if xy.len() != depth.len() {
        return Err(invalid("native stitch field length mismatch"));
    }
    check_field(depth)?;
    for &point in xy {
        contour::quantize(point)?;
    }
    if triangles.is_empty() {
        return Ok(Unit {
            sequence,
            paths: vec![],
        });
    }
    let layout = TinBins::new(xy, triangles.len())?;
    let mut memberships = 0usize;
    for indices in triangles {
        let [r0, r1, c0, c1] = layout.range(xy, indices)?;
        memberships = memberships
            .checked_add((r1 - r0 + 1) * (c1 - c0 + 1))
            .ok_or_else(|| MapError::Budget("native stitch membership overflow".into()))?;
    }
    // Bin memberships are usize addresses, not contour vertices. Include Vec
    // growth and each bin's minimum capacity before allocating any membership.
    let bin_count = layout.rows * layout.cols;
    let membership_bytes = memberships
        .checked_mul(2 * std::mem::size_of::<usize>())
        .and_then(|n| {
            bin_count
                .checked_mul(std::mem::size_of::<Vec<usize>>() + 4 * std::mem::size_of::<usize>())
                .and_then(|headers| n.checked_add(headers))
        })
        .ok_or_else(|| MapError::Budget("native stitch membership budget overflow".into()))?;
    Limits {
        max_working_bytes: limits
            .max_working_bytes
            .checked_sub(membership_bytes)
            .ok_or_else(|| MapError::Budget("native stitch memberships exceed budget".into()))?,
    }
    .admit(0, 0)?;
    let mut bins = vec![vec![]; layout.rows * layout.cols];
    for (index, indices) in triangles.iter().enumerate() {
        let [r0, r1, c0, c1] = layout.range(xy, indices)?;
        for row in r0..=r1 {
            for col in c0..=c1 {
                bins[row * layout.cols + col].push(index);
            }
        }
    }
    let mut paths: Paths64 = vec![];
    for bin in bins {
        if bin.is_empty() {
            continue;
        }
        let retained = contour::count_vertices(&paths)?
            .checked_mul(256)
            .and_then(|n| paths.len().checked_mul(128).and_then(|m| n.checked_add(m)))
            .and_then(|n| n.checked_add(membership_bytes))
            .ok_or_else(|| MapError::Budget("native stitch retained budget overflow".into()))?;
        let remaining = limits
            .max_working_bytes
            .checked_sub(retained)
            .ok_or_else(|| MapError::Budget("native stitch retained data exceeds budget".into()))?;
        let mut fragments = Fragments::new(Limits {
            max_working_bytes: remaining,
        });
        for index in bin {
            let indices = triangles[index];
            if let [Some(a), Some(b), Some(c)] =
                indices.map(|point| sample(xy[point as usize], depth[point as usize]))
            {
                triangle([a, b, c], &mut fragments)?;
            }
        }
        paths.extend(fragments.complete(0)?.paths);
        limits.admit(contour::count_vertices(&paths)?, paths.len())?;
    }
    Ok(Unit { sequence, paths })
}

/// Final native serial first-subject/rest-clip union, exactly once. Optional AOI
/// intersects the completed geometry using an all-subject, orientation-aware set.
pub fn finish(units: &[Unit], aoi: Option<&MultiPolygon>, limits: Limits) -> Result<Output> {
    limits.admit(0, units.len())?;
    let mut ordered: Vec<_> = units.iter().collect();
    ordered.sort_by_key(|unit| unit.sequence);
    if ordered
        .windows(2)
        .any(|pair| pair[0].sequence == pair[1].sequence)
    {
        return Err(invalid("duplicate native contour unit sequence"));
    }
    let vertices = ordered.iter().try_fold(0usize, |sum, unit| {
        sum.checked_add(unit.vertex_count())
            .ok_or_else(|| MapError::Budget("native contour vertex count overflow".into()))
    })?;
    let rings = ordered.iter().try_fold(0usize, |sum, unit| {
        sum.checked_add(unit.ring_count())
            .ok_or_else(|| MapError::Budget("native contour ring count overflow".into()))
    })?;
    limits.admit(vertices, rings)?;
    let paths: Paths64 = ordered
        .into_iter()
        .flat_map(|unit| unit.paths.iter().cloned())
        .collect();
    let mut clipper = Clipper::new_global_merge();
    let any = add_partition(&mut clipper, &paths)?;
    // Native final merging uses the flat Execute overload. PolyTree changes
    // the clipping workload and its FirstLeft lifecycle; hole assignment below
    // is serialization of the unchanged flat result, not another union.
    let mut merged = vec![];
    let success = clipper
        .execute(
            ClipType::Union,
            &mut merged,
            PolyFillType::NonZero,
            PolyFillType::NonZero,
        )
        .map_err(engine_error)?;
    if any && !success {
        return Err(invalid("native final contour union failed"));
    }
    drop(clipper);
    limits.admit(contour::count_vertices(&merged)?, merged.len())?;
    // Evaluate native thin-ring metrics in the original raw ring order, before
    // canonicalization. The grouping check also refuses removed holes that
    // would leave overlapping retained islands.
    let mut retained = merged
        .iter()
        .map(keep_native_ring)
        .collect::<Result<Vec<_>>>()?;
    let mut groups = crate::boundary_ring_groups::group(&merged, &retained, limits)?;
    if let Some(aoi) = aoi {
        let clips = contour::aoi_paths(aoi, limits)?;
        let polygons: Paths64 = merged
            .iter()
            .zip(&retained)
            .filter(|(_, keep)| **keep)
            .map(|(path, _)| path.clone())
            .collect();
        let vertices = contour::count_vertices(&polygons)?
            .checked_add(contour::count_vertices(&clips)?)
            .ok_or_else(|| MapError::Budget("native AOI vertex count overflow".into()))?;
        limits.admit(vertices, polygons.len() + clips.len())?;
        let mut clipping = Clipper::new_global_merge();
        let subject_added = clipping
            .add_paths(&polygons, PolyType::Subject, true)
            .map_err(engine_error)?;
        let clip_added = clipping
            .add_paths(&clips, PolyType::Clip, true)
            .map_err(engine_error)?;
        let mut clipped = vec![];
        let success = clipping
            .execute(
                ClipType::Intersection,
                &mut clipped,
                PolyFillType::NonZero,
                PolyFillType::NonZero,
            )
            .map_err(engine_error)?;
        if (subject_added || clip_added) && !success {
            return Err(invalid("native contour AOI intersection failed"));
        }
        merged = clipped;
        retained = vec![true; merged.len()];
        groups = crate::boundary_ring_groups::group(&merged, &retained, limits)?;
    }
    limits.admit(contour::count_vertices(&merged)?, merged.len())?;
    let mut polygons = vec![];
    for group in groups {
        let exterior = contour::canonical(&merged[group.shell], true)?;
        let mut holes = group
            .holes
            .iter()
            .map(|&hole| contour::canonical(&merged[hole], false))
            .collect::<Result<Vec<_>>>()?;
        holes.sort();
        let mut polygon = vec![exterior];
        polygon.extend(holes);
        polygons.push(polygon);
    }
    polygons.sort();
    let coordinates = polygons
        .into_iter()
        .map(|polygon| {
            polygon
                .into_iter()
                .map(|ring| {
                    let mut coordinates: Vec<_> = ring
                        .into_iter()
                        .map(|point| [point[0] as f64 * 0.001, point[1] as f64 * 0.001])
                        .collect();
                    coordinates.push(coordinates[0]);
                    coordinates
                })
                .collect()
        })
        .collect();
    Ok(Output {
        method_id: METHOD_ID,
        coordinate_precision: 0.001,
        coordinates,
    })
}

fn keep_native_ring(path: &Path64) -> Result<bool> {
    if path.is_empty() {
        return Ok(true);
    }
    let mut area = 0.;
    let mut perimeter = 0.;
    for index in 1..=path.len() {
        let current = path[index % path.len()];
        let previous = path[index - 1];
        let a = [current.x as f64 * 0.001, current.y as f64 * 0.001];
        let b = [previous.x as f64 * 0.001, previous.y as f64 * 0.001];
        // PointMs.ComputeArea: closed-ring segments (current, previous), in
        // source order, including the final closing edge.
        area += 0.5 * (a[1] + b[1]) * (b[0] - a[0]);
        // PointM.DistanceTo uses Pow(Pow(dx,2)+Pow(dy,2),0.5).
        perimeter += ((a[0] - b[0]).powf(2.) + (a[1] - b[1]).powf(2.)).powf(0.5);
    }
    if !area.is_finite() || !perimeter.is_finite() {
        return Err(invalid("native contour ring metric overflow"));
    }
    Ok(area >= 0. || (area / perimeter).abs() > 0.001)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clipper2_rust::Point64;
    fn path(values: &[[i64; 2]]) -> Path64 {
        values.iter().map(|[x, y]| Point64::new(*x, *y)).collect()
    }
    const LIMITS: Limits = Limits {
        max_working_bytes: 16 * 1024 * 1024,
    };
    #[test]
    fn native_partition_changes_hole_when_first_operand_changes() {
        let outer = path(&[[0, 0], [10000, 0], [10000, 10000], [0, 10000]]);
        let hole = path(&[[2000, 2000], [2000, 8000], [8000, 8000], [8000, 2000]]);
        let remote = path(&[[20000, 0], [30000, 0], [30000, 10000], [20000, 10000]]);
        assert_eq!(
            union(&vec![outer.clone(), hole.clone(), remote.clone()], LIMITS)
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            union(&vec![hole.clone(), outer.clone(), remote.clone()], LIMITS)
                .unwrap()
                .len(),
            2
        );
        assert_eq!(union(&vec![remote, outer, hole], LIMITS).unwrap().len(), 3);
    }
    #[test]
    fn classic_fractional_intersection_matches_both_pinned_native_versions() {
        let a = path(&[[0, 0], [11, 0], [0, 9]]);
        let b = path(&[[3, -2], [8, -2], [8, 8], [3, 8]]);
        assert_eq!(
            union(&vec![a, b], LIMITS).unwrap(),
            vec![path(&[
                [8, 0],
                [11, 0],
                [8, 2],
                [8, 8],
                [3, 8],
                [3, 7],
                [0, 9],
                [0, 0],
                [3, 0],
                [3, -2],
                [8, -2]
            ])]
        );
    }
    #[test]
    fn native_tin_bin_dimensions_and_inclusive_bbox_memberships() {
        let xy = [[0., 0.], [10., 0.], [0., 1.], [10., 1.]];
        let small = TinBins::new(&xy, 6143).unwrap();
        assert_eq!((small.rows, small.cols), (1, 1));
        let larger = TinBins::new(&xy, 6144).unwrap();
        assert_eq!((larger.rows, larger.cols), (1, 2));
        assert_eq!(larger.range(&xy, &[0, 1, 2]).unwrap(), [0, 0, 0, 1]);
        let square = TinBins::new(&[[0., 0.], [10., 10.]], 14336).unwrap();
        assert_eq!((square.rows, square.cols), (2, 2));
    }
    #[test]
    fn native_thin_hole_filter_uses_float_area_and_perimeter_before_ring_canonicalization() {
        assert!(
            !keep_native_ring(&path(&[
                [2000, 2000],
                [2000, 2002],
                [2002, 2002],
                [2002, 2000]
            ]))
            .unwrap()
        );
        assert!(
            keep_native_ring(&path(&[
                [2000, 2000],
                [2000, 2010],
                [2010, 2010],
                [2010, 2000]
            ]))
            .unwrap()
        );
        let native_unit = Unit {
            sequence: 0,
            paths: vec![
                path(&[[20000, 0], [30000, 0], [30000, 10000], [20000, 10000]]),
                path(&[[0, 0], [10000, 0], [10000, 10000], [0, 10000]]),
                path(&[[2000, 2000], [2000, 2002], [2002, 2002], [2002, 2000]]),
            ],
        };
        let output = finish(&[native_unit], None, LIMITS).unwrap();
        assert_eq!(output.coordinates.len(), 2);
        assert!(output.coordinates.iter().all(|polygon| polygon.len() == 1));
    }
}
