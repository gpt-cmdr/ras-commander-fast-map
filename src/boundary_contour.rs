//! Deterministic zero-depth contours. No claim of native vector parity.
//! Terrain sampling/surface reconstruction and AOI halos belong to the caller.
use crate::MapError;
use clipper2_rust::{ClipType, Clipper64, FillRule, Path64, Paths64, Point64, PolyTree64};
use serde::Serialize;

pub const METHOD_ID: &str = "deterministic-zero-depth-contours/1";
pub const PRECISION: f64 = 1000.;
pub type Result<T> = std::result::Result<T, MapError>;
/// Planned admission only; not a hard process RSS limit. Includes clipping overhead
/// estimated as 256 bytes per vertex, 128 per ring and a 64 KiB fixed allowance.
#[derive(Clone, Copy)]
pub struct Limits {
    pub max_working_bytes: usize,
}
impl Limits {
    fn admit(self, vertices: usize, paths: usize) -> Result<()> {
        let estimate = vertices
            .checked_mul(256)
            .and_then(|n| paths.checked_mul(128).and_then(|m| n.checked_add(m)))
            .and_then(|n| n.checked_add(65536))
            .ok_or_else(|| {
                MapError::Budget("boundary contour planned memory exceeds budget".into())
            })?;
        if estimate > self.max_working_bytes {
            Err(MapError::Budget(
                "boundary contour planned memory exceeds budget".into(),
            ))
        } else {
            Ok(())
        }
    }
}
#[derive(Clone, Copy)]
pub struct Grid {
    pub cols: usize,
    pub rows: usize,
    pub transform: [f64; 6],
}
impl Grid {
    fn validate(self, n: usize) -> Result<()> {
        let t = self.transform;
        if self.cols < 2
            || self.rows < 2
            || self.cols.checked_mul(self.rows) != Some(n)
            || t.iter().any(|v| !v.is_finite())
            || t[1] <= 0.
            || t[5] >= 0.
            || t[2] != 0.
            || t[4] != 0.
        {
            return Err(MapError::Invalid("invalid contour sample grid".into()));
        }
        Ok(())
    }
    fn point(self, r: usize, c: usize) -> [f64; 2] {
        [
            self.transform[0] + (c as f64 + 0.5) * self.transform[1],
            self.transform[3] + (r as f64 + 0.5) * self.transform[5],
        ]
    }
    fn center(self, r: usize, c: usize) -> [f64; 2] {
        [
            self.transform[0] + (c + 1) as f64 * self.transform[1],
            self.transform[3] + (r + 1) as f64 * self.transform[5],
        ]
    }
}
#[derive(Clone, Copy)]
struct Sample {
    xy: [f64; 2],
    z: f32,
}
#[derive(Clone, Debug)]
pub struct Unit {
    pub sequence: u64,
    paths: Paths64,
}
impl Unit {
    pub fn vertex_count(&self) -> usize {
        self.paths
            .iter()
            .fold(0usize, |n, p| n.saturating_add(p.len()))
    }
    pub fn ring_count(&self) -> usize {
        self.paths.len()
    }
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
    fn push(&mut self, mut p: Path64) -> Result<()> {
        if p.len() < 3 {
            return Ok(());
        }
        // Raw fragments denote wet regions irrespective of stored TIN winding.
        // Holes arise from union, not from triangle winding.
        let area = area2_points(&p)?;
        if area == 0 {
            return Ok(());
        }
        if area < 0 {
            p.reverse();
        }
        let n = self.vertices.checked_add(p.len()).ok_or_else(|| {
            MapError::Budget("boundary contour planned memory exceeds budget".into())
        })?;
        self.limits.admit(n, self.paths.len() + 1)?;
        self.vertices = n;
        self.paths.push(p);
        Ok(())
    }
    fn complete(self, sequence: u64) -> Result<Unit> {
        let paths = union(&self.paths, self.limits)?;
        Ok(Unit { sequence, paths })
    }
}
fn quantize(p: [f64; 2]) -> Result<Point64> {
    let q = [
        (p[0] * PRECISION).round_ties_even(),
        (p[1] * PRECISION).round_ties_even(),
    ];
    if q.iter()
        .any(|v| !v.is_finite() || v.abs() >= ((1_u64 << 61) as f64))
    {
        return Err(MapError::Invalid(
            "contour coordinate outside integer clipping range".into(),
        ));
    }
    Ok(Point64::new(q[0] as i64, q[1] as i64))
}
fn check_field(values: &[Option<f32>]) -> Result<()> {
    if values.iter().any(|v| v.is_some_and(|v| !v.is_finite())) {
        Err(MapError::Invalid("nonfinite contour sample".into()))
    } else {
        Ok(())
    }
}
fn edge(a: Sample, b: Sample, out: &mut Vec<[f64; 2]>) -> Result<()> {
    let inside = |z: f32| z >= 0.;
    if inside(a.z) {
        out.push(a.xy)
    }
    if (a.z >= 0.) != (b.z >= 0.) {
        let span = b.z - a.z;
        if !span.is_finite() {
            return Err(MapError::Invalid("zero-contour depth span overflow".into()));
        }
        let t = f64::from((0f32 - a.z) / span);
        if !t.is_finite() {
            return Err(MapError::Invalid(
                "nonfinite zero-contour interpolation".into(),
            ));
        }
        out.push([
            a.xy[0] + (b.xy[0] - a.xy[0]) * t,
            a.xy[1] + (b.xy[1] - a.xy[1]) * t,
        ]);
    }
    Ok(())
}
fn triangle(v: [Sample; 3], out: &mut Fragments) -> Result<()> {
    let mut coords = Vec::with_capacity(4);
    for i in 0..3 {
        edge(v[i], v[(i + 1) % 3], &mut coords)?
    }
    let mut p = coords
        .into_iter()
        .map(quantize)
        .collect::<Result<Path64>>()?;
    p.dedup();
    if p.len() > 1 && p.first() == p.last() {
        p.pop();
    }
    out.push(p)
}
fn block(v: [Option<Sample>; 4], xy: [f64; 2], out: &mut Fragments) -> Result<()> {
    let count = v.iter().flatten().count();
    if count < 3 {
        return Ok(());
    }
    let sum = v.iter().flatten().fold(0f32, |a, b| a + b.z);
    let z = if count == 4 {
        (f64::from(sum) * 0.25) as f32
    } else {
        sum / 3f32
    };
    if !z.is_finite() {
        return Err(MapError::Invalid("contour center depth overflow".into()));
    }
    let mid = Sample { xy, z };
    for i in 0..4 {
        let j = (i + 1) % 4;
        if let (Some(a), Some(b)) = (v[i], v[j]) {
            triangle([b, a, mid], out)?
        }
    }
    Ok(())
}
fn sample(xy: [f64; 2], z: Option<f32>) -> Option<Sample> {
    z.map(|z| Sample { xy, z })
}
/// One haloed native sample tile. Input depths must retain valid negative samples.
/// Fully wet blocks are coalesced along rows to bound clipping work.
/// Missing samples are `None`; a valid negative value, including -9999, remains numeric.
pub fn grid(sequence: u64, grid: Grid, depth: &[Option<f32>], limits: Limits) -> Result<Unit> {
    grid.validate(depth.len())?;
    quantize(grid.point(0, 0))?;
    quantize(grid.point(grid.rows - 1, grid.cols - 1))?;
    limits.admit(0, 0)?;
    check_field(depth)?;
    let mut fragments = Fragments::new(limits);
    for r in 0..grid.rows - 1 {
        let mut c = 0;
        while c < grid.cols - 1 {
            let wet = |c: usize| {
                let i = r * grid.cols + c;
                [i, i + 1, i + grid.cols + 1, i + grid.cols]
                    .iter()
                    .all(|&i| depth[i].is_some_and(|v| v >= 0.))
            };
            if wet(c) {
                let start = c;
                while c + 1 < grid.cols - 1 && wet(c + 1) {
                    c += 1
                }
                let mut p = [
                    grid.point(r, start),
                    grid.point(r + 1, start),
                    grid.point(r + 1, c + 1),
                    grid.point(r, c + 1),
                ]
                .into_iter()
                .map(quantize)
                .collect::<Result<Path64>>()?;
                p.dedup();
                fragments.push(p)?;
                c += 1;
                continue;
            }
            let i = r * grid.cols + c;
            block(
                [
                    sample(grid.point(r, c), depth[i]),
                    sample(grid.point(r, c + 1), depth[i + 1]),
                    sample(grid.point(r + 1, c + 1), depth[i + grid.cols + 1]),
                    sample(grid.point(r + 1, c), depth[i + grid.cols]),
                ],
                grid.center(r, c),
                &mut fragments,
            )?;
            c += 1;
        }
    }
    fragments.complete(sequence)
}
/// Terrain stitch triangles, using original point XY and reconstructed signed depth.
/// Missing samples are explicit; no numeric nodata sentinel is used for depths.
pub fn tin(
    sequence: u64,
    xy: &[[f64; 2]],
    depth: &[Option<f32>],
    triangles: &[[u32; 3]],
    limits: Limits,
) -> Result<Unit> {
    limits.admit(0, 0)?;
    if xy.len() != depth.len() {
        return Err(MapError::Invalid(
            "stitch contour field length mismatch".into(),
        ));
    }
    check_field(depth)?;
    for &point in xy {
        quantize(point)?;
    }
    let mut out = Fragments::new(limits);
    for t in triangles {
        let mut v = [None; 3];
        for i in 0..3 {
            let k = t[i] as usize;
            if k >= xy.len() {
                return Err(MapError::Invalid(
                    "stitch contour point out of range".into(),
                ));
            }
            v[i] = sample(xy[k], depth[k]);
        }
        if let [Some(a), Some(b), Some(c)] = v {
            triangle([a, b, c], &mut out)?
        }
    }
    out.complete(sequence)
}

fn count_vertices(paths: &Paths64) -> Result<usize> {
    paths.iter().try_fold(0usize, |n, p| {
        n.checked_add(p.len())
            .ok_or_else(|| MapError::Budget("contour vertex count overflow".into()))
    })
}

fn union(paths: &Paths64, limits: Limits) -> Result<Paths64> {
    limits.admit(count_vertices(paths)?, paths.len())?;
    if paths.is_empty() {
        return Ok(vec![]);
    }
    let mut c = Clipper64::new();
    c.set_preserve_collinear(false);
    c.add_subject(paths);
    let mut out = vec![];
    if !c.execute(ClipType::Union, FillRule::NonZero, &mut out, None) {
        return Err(MapError::Invalid("contour union failed".into()));
    }
    limits.admit(count_vertices(&out)?, out.len())?;
    Ok(out)
}
fn area2_points(p: &Path64) -> Result<i128> {
    let a = p[0];
    let mut sum = 0i128;
    for (p, q) in p.iter().zip(p.iter().cycle().skip(1)).take(p.len()) {
        let (px, py) = (p.x as i128 - a.x as i128, p.y as i128 - a.y as i128);
        let (qx, qy) = (q.x as i128 - a.x as i128, q.y as i128 - a.y as i128);
        let cross = px
            .checked_mul(qy)
            .and_then(|a| qx.checked_mul(py).and_then(|b| a.checked_sub(b)))
            .ok_or_else(|| MapError::Invalid("contour area overflow".into()))?;
        sum = sum
            .checked_add(cross)
            .ok_or_else(|| MapError::Invalid("contour area overflow".into()))?;
    }
    Ok(sum)
}

fn area2(p: &[[i64; 2]]) -> Result<i128> {
    let a = p[0];
    let mut sum = 0i128;
    for (p, q) in p.iter().zip(p.iter().cycle().skip(1)).take(p.len()) {
        let (px, py) = (p[0] as i128 - a[0] as i128, p[1] as i128 - a[1] as i128);
        let (qx, qy) = (q[0] as i128 - a[0] as i128, q[1] as i128 - a[1] as i128);
        sum = sum
            .checked_add(
                px.checked_mul(qy)
                    .and_then(|a| qx.checked_mul(py).and_then(|b| a.checked_sub(b)))
                    .ok_or_else(|| MapError::Invalid("contour area overflow".into()))?,
            )
            .ok_or_else(|| MapError::Invalid("contour area overflow".into()))?;
    }
    Ok(sum)
}
/// Booth minimal rotation; stable in linear time even with duplicate vertices.
fn rotate_min(p: &mut [[i64; 2]]) {
    let n = p.len();
    if n < 2 {
        return;
    }
    let (mut i, mut j, mut k) = (0, 1, 0);
    while i < n && j < n && k < n {
        let a = p[(i + k) % n];
        let b = p[(j + k) % n];
        if a == b {
            k += 1;
            continue;
        }
        if a > b {
            i += k + 1;
            if i == j {
                i += 1
            }
        } else {
            j += k + 1;
            if i == j {
                j += 1
            }
        }
        k = 0;
    }
    p.rotate_left(i.min(j) % n);
}
fn canonical(path: &Path64, ccw: bool) -> Result<Vec<[i64; 2]>> {
    let mut p: Vec<_> = path.iter().map(|p| [p.x, p.y]).collect();
    p.dedup();
    if p.len() > 1 && p.first() == p.last() {
        p.pop();
    }
    if p.len() < 3 {
        return Err(MapError::Invalid("degenerate contour ring".into()));
    }
    let area = area2(&p)?;
    if area == 0 {
        return Err(MapError::Invalid("zero area contour ring".into()));
    }
    if (area > 0) != ccw {
        p.reverse()
    }
    rotate_min(&mut p);
    Ok(p)
}
pub type MultiPolygon = Vec<Vec<Vec<[f64; 2]>>>;
#[derive(Serialize, Debug, PartialEq)]
pub struct Output {
    pub method_id: &'static str,
    pub coordinate_precision: f64,
    pub coordinates: MultiPolygon,
}
fn aoi_paths(aoi: &MultiPolygon, limits: Limits) -> Result<Paths64> {
    let vertices = aoi.iter().flatten().try_fold(0usize, |n, r| {
        n.checked_add(r.len())
            .ok_or_else(|| MapError::Budget("AOI contour vertex count overflow".into()))
    })?;
    let rings = aoi.iter().try_fold(0usize, |n, p| {
        n.checked_add(p.len())
            .ok_or_else(|| MapError::Budget("AOI contour ring count overflow".into()))
    })?;
    limits.admit(vertices, rings)?;
    let mut paths = vec![];
    for polygon in aoi {
        if polygon.is_empty() {
            return Err(MapError::Invalid("empty AOI polygon".into()));
        }
        for (i, ring) in polygon.iter().enumerate() {
            let path = ring
                .iter()
                .copied()
                .map(quantize)
                .collect::<Result<Path64>>()?;
            let p = canonical(&path, i == 0)?;
            paths.push(p.into_iter().map(|p| Point64::new(p[0], p[1])).collect());
        }
    }
    Ok(paths)
}
/// Deterministic input sequence, all-subject NonZero union, then vector AOI clip.
/// AOI does not change sampling, connectivity, tile traversal or zero-crossings.
pub fn finish(units: &[Unit], aoi: Option<&MultiPolygon>, limits: Limits) -> Result<Output> {
    limits.admit(0, units.len())?;
    let mut order: Vec<_> = units.iter().collect();
    order.sort_by_key(|u| u.sequence);
    if order.windows(2).any(|w| w[0].sequence == w[1].sequence) {
        return Err(MapError::Invalid("duplicate contour unit sequence".into()));
    }
    let vertices = order.iter().try_fold(0usize, |n, u| {
        n.checked_add(u.vertex_count()).ok_or_else(|| {
            MapError::Budget("boundary contour planned memory exceeds budget".into())
        })
    })?;
    let count = order.iter().try_fold(0usize, |n, u| {
        n.checked_add(u.paths.len())
            .ok_or_else(|| MapError::Budget("contour ring count overflow".into()))
    })?;
    limits.admit(vertices, count)?;
    let paths = order
        .into_iter()
        .flat_map(|u| u.paths.iter().cloned())
        .collect();
    let paths = union(&paths, limits)?;
    if paths.is_empty() {
        if let Some(aoi) = aoi {
            aoi_paths(aoi, limits)?;
        }
        return Ok(Output {
            method_id: METHOD_ID,
            coordinate_precision: 1. / PRECISION,
            coordinates: vec![],
        });
    }
    let mut c = Clipper64::new();
    c.set_preserve_collinear(false);
    c.add_subject(&paths);
    let operation = if let Some(aoi) = aoi {
        let clips = aoi_paths(aoi, limits)?;
        let clipped_vertices = vertices
            .checked_add(count_vertices(&clips)?)
            .ok_or_else(|| MapError::Budget("AOI contour vertex count overflow".into()))?;
        let clipped_count = count
            .checked_add(clips.len())
            .ok_or_else(|| MapError::Budget("AOI contour ring count overflow".into()))?;
        limits.admit(clipped_vertices, clipped_count)?;
        c.add_clip(&clips);
        ClipType::Intersection
    } else {
        ClipType::Union
    };
    let mut tree = PolyTree64::new();
    let mut open = vec![];
    if !c.execute_tree(operation, FillRule::NonZero, &mut tree, &mut open) {
        return Err(MapError::Invalid("contour topology assembly failed".into()));
    }
    let output_vertices = tree.nodes.iter().try_fold(0usize, |n, p| {
        n.checked_add(p.polygon().len())
            .ok_or_else(|| MapError::Budget("contour output vertex count overflow".into()))
    })?;
    limits.admit(output_vertices, tree.nodes.len())?;
    let mut polygons = vec![];
    for i in 1..tree.nodes.len() {
        if tree.is_hole(i) {
            continue;
        }
        let node = &tree.nodes[i];
        let exterior = canonical(node.polygon(), true)?;
        let mut holes = node
            .children()
            .iter()
            .map(|&j| canonical(tree.nodes[j].polygon(), false))
            .collect::<Result<Vec<_>>>()?;
        holes.sort();
        let mut polygon = vec![exterior];
        polygon.extend(holes);
        polygons.push(polygon);
    }
    polygons.sort();
    let coordinates = polygons
        .into_iter()
        .map(|p| {
            p.into_iter()
                .map(|ring| {
                    let mut r: Vec<_> = ring
                        .into_iter()
                        .map(|p| {
                            [
                                p[0] as f64 * (1. / PRECISION),
                                p[1] as f64 * (1. / PRECISION),
                            ]
                        })
                        .collect();
                    r.push(r[0]);
                    r
                })
                .collect()
        })
        .collect();
    Ok(Output {
        method_id: METHOD_ID,
        coordinate_precision: 1. / PRECISION,
        coordinates,
    })
}
