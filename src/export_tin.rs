//! RAS Mapper's per-output-tile export TIN (gap fill) for the effective
//! terrain.
//!
//! During a terrain export, RasMapperLib 7.0.1 triangulates, for every
//! 256 x 256 output tile, the edges of valid render blocks that border off
//! blocks, deletes the triangles inside the data and those wider than about
//! two cells, and rasterizes the rest over the tile, overwriting the values
//! sampled from the sources. Gaps of one source cell between valid blocks are
//! bridged this way; wider gaps stay nodata. The stored `Stitch TIN` of the
//! terrain HDF is not used by the export.
//!
//! This module reproduces that step from an interoperability study of
//! RasMapperLib 7.0.1 (prose findings only; no vendor code is used or
//! copied):
//!
//! 1. **Segments.** For each source (priority order) the render blocks the
//!    export visits for the tile are listed per block tile: the blocks under
//!    the tile's pixel centres, skipped indices filled within the same block
//!    tile, and a two-block halo before the first and after the last block
//!    of each axis (stepping into the neighbouring block tile when it
//!    exists). Every valid block (mask bit 0) with off neighbours emits its
//!    edges in mask-bit order 8 (left, D->A), 16 (top, A->B), 4 (right,
//!    B->C), 2 (bottom, C->D), so the block interior lies on the right of
//!    each edge. Corner Z is the binary32 source value at the corner, X/Y the
//!    block tile's recomputed cell geometry, M that block tile's cell size.
//! 2. **Order.** Each source's segments are stable-sorted by a 32-bit hash of
//!    the first point's X (`HASH_TYPE_ID`); the order inside equal hashes
//!    follows the export's parallel collection, modelled with
//!    `COLLECT_WORKERS` range partitions per block tile.
//! 3. **TIN.** Incremental Delaunay insertion (A then B of every segment)
//!    inside an octagonal super polygon, points merged within
//!    min(0.01, 0.1 x smallest cell); then every segment is enforced as a
//!    constraint edge. Triangles touching the super polygon, triangles with
//!    an edge wider or taller than 2.0001 x the largest cell, and the
//!    triangles on the data side of every segment (bounded fans) are deleted.
//! 4. **Rasterization.** Remaining triangles in slot order, edge functions
//!    stepped by repeated binary64 addition, inclusive `>= 0` test,
//!    `z = ((w0 z0 + w1 z1) + w2 z2) / S` rounded to binary32, later
//!    triangles overwrite earlier ones. The caller rounds to 1/32.
use crate::MapError;

/// Mask bit: the render block is valid and owned by this source.
pub(crate) const BLOCK_VALID: u8 = 1;
/// Mask bits 1-4: a neighbour (below 2, right 4, left 8, above 16) is off.
pub(crate) const BLOCK_EDGES: u8 = 2 | 4 | 8 | 16;
pub(crate) const TILE: usize = 256;

/// .NET type identity used in the export's segment hash. The hash of a
/// segment is `K xor (lo32 xor hi32)(bits(A.X))` as a signed 32-bit value,
/// `K = HASH_TYPE_ID * 711650207 + 2506965631 (mod 2^32)`. RAS Mapper's value
/// depends on the process (7 was measured in a fresh export process); it only
/// decides the order of segments, and so only the rare cocircular ties.
pub const HASH_TYPE_ID: u32 = 7;
const HASH_K: u32 = HASH_TYPE_ID
    .wrapping_mul(711_650_207)
    .wrapping_add(2_506_965_631);
/// Range partitions per block tile in the modelled parallel collection
/// (RAS Mapper's `Parallel.For` over block rows on an 8-thread host): block
/// rows are split into ranges of `rows / 8`, and each range's segments are
/// appended as one list, lists ordered by descending offset of their first
/// emitting block within the range (ties: later range first). This only
/// orders segments with equal hash.
pub const COLLECT_WORKERS: usize = 8;

/// Planned bytes of one segment plus its share of the TIN (two points, about
/// four triangles, stacks).
pub(crate) const BYTES_PER_SEGMENT: u64 = 64 + 2 * 32 + 4 * 28 + 64;
/// Planned bytes of one tile's result buffer.
pub(crate) const TILE_RESULT_BYTES: u64 = (TILE * TILE * 4) as u64;

/// A 256 x 256 raster cell geometry as RAS Mapper derives it: the cell size
/// is recomputed from the extent.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TileGrid {
    pub min_x: f64,
    pub max_x: f64,
    pub min_y: f64,
    pub max_y: f64,
    pub cs: f64,
}

impl TileGrid {
    pub(crate) fn new(min_x: f64, max_x: f64, min_y: f64, max_y: f64) -> Self {
        let cs = ((max_x - min_x) / TILE as f64).max((max_y - min_y) / TILE as f64);
        TileGrid {
            min_x,
            max_x,
            min_y,
            max_y,
            cs,
        }
    }

    fn col(&self, x: f64) -> i64 {
        ((x - self.min_x) / self.cs).floor() as i64
    }

    fn row(&self, y: f64) -> i64 {
        ((self.max_y - y) / self.cs).floor() as i64
    }

    pub(crate) fn ccx(&self, c: i64) -> f64 {
        self.min_x + self.cs * (c as f64 + 0.5)
    }

    pub(crate) fn ccy(&self, r: i64) -> f64 {
        self.max_y - self.cs * (r as f64 + 0.5)
    }

    /// `e` = `[left, right, bottom, top]`.
    pub(crate) fn overlaps(&self, e: &[f64; 4]) -> bool {
        let [left, right, bottom, top] = *e;
        !(self.max_x < left || self.min_x > right || self.min_y > top || self.max_y < bottom)
    }
}

/// A source's render-block tiles (see `effective_terrain::SourceLattice`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct BlockLattice {
    pub min_x: f64,
    pub max_y: f64,
    pub pitch: f64,
    pub tiles_x: usize,
    pub tiles_y: usize,
}

impl BlockLattice {
    fn tile(&self, tr: usize, tc: usize) -> TileGrid {
        let p = self.pitch;
        TileGrid::new(
            self.min_x + p * tc as f64,
            self.min_x + p * (tc + 1) as f64,
            self.max_y - p * (tr + 1) as f64,
            self.max_y - p * tr as f64,
        )
    }
}

/// What the export TIN needs from one source.
pub(crate) trait BlockSource: Sync {
    /// `[left, right, bottom, top]` of the source raster.
    fn extent(&self) -> [f64; 4];
    fn lattice(&self) -> BlockLattice;
    /// Whether block tile `tile` has a valid block with an off neighbour.
    fn tile_has_edges(&self, tile: usize) -> bool;
    /// Mask bytes of block rows `[row0, row0 + rows)` of block tile `tile`.
    fn mask_rows(&self, tile: usize, row0: usize, rows: usize) -> Result<Vec<u8>, MapError>;
    /// Raw binary32 cell values of `rows` x `cols` cells from (`row0`,
    /// `col0`), NaN outside the raster.
    fn values(
        &self,
        row0: usize,
        rows: usize,
        col0: usize,
        cols: usize,
    ) -> Result<Vec<f32>, MapError>;
}

/// A stitch segment: points A and B as (x, y, z, cell size).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Seg {
    pub a: [f64; 4],
    pub b: [f64; 4],
}

fn seg_key(ax: f64) -> i32 {
    let b = ax.to_bits();
    (HASH_K ^ (b as u32) ^ ((b >> 32) as u32)) as i32
}

type AxisList = Vec<(usize, Vec<usize>)>;

/// One axis of the visited blocks: per block tile, the block indices in
/// visit order. `centre(i)` is pixel centre `i`, `tile_of` its block tile and
/// `local` its block inside that tile (clamped to the tile).
fn axis_list(
    i0: i64,
    i1: i64,
    centre: impl Fn(i64) -> f64,
    tile_of: impl Fn(f64) -> i64,
    tiles: usize,
    local: impl Fn(usize, f64) -> i64,
) -> AxisList {
    let mut list: AxisList = Vec::new();
    let mut last: Option<i64> = None;
    let mut num: i64 = -1;
    for i in i0..=i1 {
        let v = centre(i);
        let t = tile_of(v);
        if t < 0 || t >= tiles as i64 {
            continue;
        }
        let t = t as usize;
        if list.last().is_none_or(|e| e.0 != t) {
            list.push((t, Vec::new()));
            last = None;
        }
        let c = local(t, v).clamp(0, TILE as i64 - 1);
        let cur = &mut list.last_mut().expect("tile entry").1;
        // Skipped indices are filled within the current block tile only.
        if num != -1 && c != num + 1 {
            for j in num + 1..c {
                cur.push(j as usize);
                last = Some(j);
            }
        }
        if last != Some(c) {
            cur.push(c as usize);
            last = Some(c);
        }
        num = c;
    }
    list
}

/// The two-block halo before the first and after the last visited block.
fn add_halo(list: &mut AxisList, tiles: usize) {
    if list.is_empty() {
        return;
    }
    let last_block = TILE - 1;
    let mut at = 0;
    for _ in 0..2 {
        let Some(&f) = list[at].1.first() else { break };
        if f == 0 {
            if list[at].0 == 0 {
                break;
            }
            let t = list[at].0 - 1;
            list.insert(0, (t, vec![last_block]));
            at = 0;
        } else {
            list[at].1.insert(0, f - 1);
        }
    }
    for _ in 0..2 {
        let at = list.len() - 1;
        let Some(&f) = list[at].1.last() else { break };
        if f == last_block {
            if list[at].0 + 1 >= tiles {
                break;
            }
            let t = list[at].0 + 1;
            list.push((t, vec![0]));
        } else {
            list[at].1.push(f + 1);
        }
    }
}

/// Block tile rows and columns (with block lists) a source visits for the
/// output tile `ras`, or `None` when it is skipped.
fn visit_lists(ext: &[f64; 4], rb: &BlockLattice, ras: &TileGrid) -> Option<(AxisList, AxisList)> {
    if !ras.overlaps(ext) {
        return None;
    }
    let [left, right, bottom, top] = *ext;
    let last = TILE as i64 - 1;
    let c0 = if left <= ras.min_x { 0 } else { ras.col(left) };
    let c1 = if right >= ras.max_x {
        last
    } else {
        ras.col(right)
    };
    let r1 = if bottom <= ras.min_y {
        last
    } else {
        ras.row(bottom)
    };
    let r0 = if top >= ras.max_y { 0 } else { ras.row(top) };
    let mut cols = axis_list(
        c0,
        c1,
        |i| ras.ccx(i),
        |x| ((x - rb.min_x) / rb.pitch).floor() as i64,
        rb.tiles_x,
        |t, x| rb.tile(0, t).col(x),
    );
    let mut rows = axis_list(
        r0,
        r1,
        |i| ras.ccy(i),
        |y| ((rb.max_y - y) / rb.pitch).floor() as i64,
        rb.tiles_y,
        |t, y| rb.tile(t, 0).row(y),
    );
    if rows.is_empty() || cols.is_empty() {
        return None;
    }
    add_halo(&mut cols, rb.tiles_x);
    add_halo(&mut rows, rb.tiles_y);
    Some((rows, cols))
}

/// Running per-tile allocation plan against a byte share.
pub(crate) struct Plan {
    pub share: u64,
    pub used: u64,
}

impl Plan {
    fn add(&mut self, bytes: u64) -> Result<(), MapError> {
        self.used = self.used.saturating_add(bytes);
        if self.used > self.share {
            return Err(MapError::Budget(format!(
                "export TIN tile needs {} bytes over its share {} of max_working_bytes",
                self.used, self.share
            )));
        }
        Ok(())
    }
}

/// The segments of one source for output tile `ras`, in the export's order.
fn collect_source(
    src: &dyn BlockSource,
    ras: &TileGrid,
    plan: &mut Plan,
) -> Result<Vec<Seg>, MapError> {
    let rb = src.lattice();
    let Some((trows, tcols)) = visit_lists(&src.extent(), &rb, ras) else {
        return Ok(Vec::new());
    };
    let tile_index = |tr: usize, tc: usize| tr * rb.tiles_x + tc;
    if !trows.iter().any(|(tr, _)| {
        tcols
            .iter()
            .any(|(tc, _)| src.tile_has_edges(tile_index(*tr, *tc)))
    }) {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for (tr, brl) in &trows {
        let n = brl.len();
        let rs = (n / COLLECT_WORKERS).max(1);
        let nr = n / rs + usize::from(n % rs != 0);
        let (rmin, rmax) = (*brl.iter().min().unwrap(), *brl.iter().max().unwrap());
        for (tc, bcl) in &tcols {
            let ti = tile_index(*tr, *tc);
            if !src.tile_has_edges(ti) {
                continue;
            }
            let mask_rows = rmax - rmin + 1;
            plan.add((mask_rows * TILE) as u64)?;
            let mask = src.mask_rows(ti, rmin, mask_rows)?;
            // (position in the block-row list, block row, block col, mask byte)
            let mut blocks: Vec<(usize, usize, usize, u8)> = Vec::new();
            let mut nseg = 0_u64;
            for (pos, &r) in brl.iter().enumerate() {
                for &c in bcl {
                    let b = mask[(r - rmin) * TILE + c];
                    if b & BLOCK_VALID != 0 && b & BLOCK_EDGES != 0 {
                        blocks.push((pos, r, c, b));
                        nseg += (b & BLOCK_EDGES).count_ones() as u64;
                    }
                }
            }
            plan.used = plan.used.saturating_sub((mask_rows * TILE) as u64);
            if blocks.is_empty() {
                continue;
            }
            let (er0, er1) = blocks
                .iter()
                .fold((usize::MAX, 0), |(a, b), k| (a.min(k.1), b.max(k.1)));
            let (ec0, ec1) = blocks
                .iter()
                .fold((usize::MAX, 0), |(a, b), k| (a.min(k.2), b.max(k.2)));
            let (wr, wc) = (er1 - er0 + 2, ec1 - ec0 + 2);
            plan.add(nseg * BYTES_PER_SEGMENT + (wr * wc * 4) as u64)?;
            let (g_r0, g_c0) = (tr * TILE + er0, tc * TILE + ec0);
            let vals = src.values(g_r0, wr, g_c0, wc)?;
            plan.used = plan.used.saturating_sub((wr * wc * 4) as u64);
            let v = |r: usize, c: usize| vals[(r - er0) * wc + (c - ec0)] as f64;
            let lt = rb.tile(*tr, *tc);
            let cs = lt.cs;
            // Modelled parallel collection: one list per block-row range.
            let mut ranges: Vec<Vec<usize>> = vec![Vec::new(); nr];
            for (k, blk) in blocks.iter().enumerate() {
                ranges[(blk.0 / rs).min(nr - 1)].push(k);
            }
            let mut order: Vec<(i64, usize)> = ranges
                .iter()
                .enumerate()
                .filter(|(_, l)| !l.is_empty())
                .map(|(k, l)| (blocks[l[0]].0 as i64 - (k * rs) as i64, k))
                .collect();
            order.sort_by(|a, b| b.cmp(a));
            for (_, k) in order {
                for &bi in &ranges[k] {
                    let (_, r, c, b) = blocks[bi];
                    let (x0, x1) = (lt.min_x + cs * c as f64, lt.min_x + cs * (c + 1) as f64);
                    let (y0, y1) = (lt.max_y - cs * r as f64, lt.max_y - cs * (r + 1) as f64);
                    let pa = [x0, y0, v(r, c), cs];
                    let pb = [x1, y0, v(r, c + 1), cs];
                    let pc = [x1, y1, v(r + 1, c + 1), cs];
                    let pd = [x0, y1, v(r + 1, c), cs];
                    for (bit, s) in [(8, (pd, pa)), (16, (pa, pb)), (4, (pb, pc)), (2, (pc, pd))] {
                        if b & bit != 0 {
                            out.push(Seg { a: s.0, b: s.1 });
                        }
                    }
                }
            }
        }
    }
    out.sort_by_key(|s| seg_key(s.a[0]));
    Ok(out)
}

/// Why a tile's TIN could not be built (the tile keeps its sampled values).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TinError(pub &'static str);

const DEL: u8 = 64;
const NONE: i32 = -1;

#[derive(Clone, Copy, Debug)]
struct Tri {
    v: [i32; 3],
    n: [i32; 3],
    /// Bits 0-2: constraint edge 0-2; bits 3-5: perimeter edge 0-2; 64: deleted.
    f: u8,
}

#[inline]
fn cross(ax: f64, ay: f64, bx: f64, by: f64, cx: f64, cy: f64) -> f64 {
    (bx - ax) * (cy - ay) - (by - ay) * (cx - ax)
}

#[inline]
pub(crate) fn close_to(px: f64, py: f64, qx: f64, qy: f64, d: f64) -> bool {
    let a = (px - qx).abs();
    if a > d {
        return false;
    }
    let b = (py - qy).abs();
    if b > d {
        return false;
    }
    (a * a + b * b).sqrt() <= d
}

#[inline]
fn dist(ax: f64, ay: f64, bx: f64, by: f64) -> f64 {
    let (dx, dy) = (ax - bx, ay - by);
    (dx * dx + dy * dy).sqrt()
}

/// Delaunay TIN factory with constraint edges.
struct Factory {
    tol: f64,
    x: Vec<f64>,
    y: Vec<f64>,
    z: Vec<f64>,
    m: Vec<f64>,
    t: Vec<Tri>,
    /// Deleted-triangle stack (slots reused last in, first out).
    ds: Vec<i32>,
    /// Start of the point-location walk; persists across calls.
    wt: i32,
    crossings: u64,
}

impl Factory {
    fn new(tol: f64) -> Self {
        Factory {
            tol,
            x: Vec::new(),
            y: Vec::new(),
            z: Vec::new(),
            m: Vec::new(),
            t: Vec::new(),
            ds: Vec::new(),
            wt: 0,
            crossings: 0,
        }
    }

    fn add_point(&mut self, x: f64, y: f64, z: f64, m: f64) -> i32 {
        self.x.push(x);
        self.y.push(y);
        self.z.push(z);
        self.m.push(m);
        (self.x.len() - 1) as i32
    }

    fn add_tri(&mut self, v: [i32; 3], n: [i32; 3]) -> i32 {
        let tri = Tri { v, n, f: 0 };
        if let Some(slot) = self.ds.pop() {
            self.t[slot as usize] = tri;
            slot
        } else {
            self.t.push(tri);
            (self.t.len() - 1) as i32
        }
    }

    #[inline]
    fn tri(&self, t: i32) -> Tri {
        self.t[t as usize]
    }

    #[inline]
    fn mark(&mut self, t: i32) {
        self.t[t as usize].f |= DEL;
    }

    #[inline]
    fn deleted(&self, t: i32) -> bool {
        self.t[t as usize].f & DEL != 0
    }

    fn is_super(&self, t: i32) -> bool {
        self.tri(t).v.iter().any(|&p| p < 8)
    }

    #[inline]
    fn xy(&self, p: i32) -> (f64, f64) {
        (self.x[p as usize], self.y[p as usize])
    }

    /// Walk from the last found triangle towards (`x`, `y`).
    fn find(&mut self, x: f64, y: f64) -> i32 {
        let n = self.t.len() as i64;
        let mut t = self.wt.max(0);
        if t as i64 > n - 1 {
            t = 0;
        }
        let (mut num, mut prev) = (0_i64, NONE);
        loop {
            let tr = self.tri(t);
            let (p0, p1, p2) = (self.xy(tr.v[0]), self.xy(tr.v[1]), self.xy(tr.v[2]));
            let c = [
                cross(p0.0, p0.1, p1.0, p1.1, x, y),
                cross(p1.0, p1.1, p2.0, p2.1, x, y),
                cross(p2.0, p2.1, p0.0, p0.1, x, y),
            ];
            let k = if c[0] < c[1] {
                if c[0] < c[2] { 0 } else { 2 }
            } else if c[1] < c[2] {
                1
            } else {
                2
            };
            if c[k] >= 0.0 {
                self.wt = t;
                return t;
            }
            let nx = tr.n[k];
            if prev == nx {
                self.wt = t;
                return t;
            }
            prev = t;
            t = nx;
            num += 1;
            if t == NONE || num == n {
                self.wt = t;
                return NONE;
            }
        }
    }

    fn closest(&self, x: f64, y: f64, v: [i32; 3]) -> i32 {
        let d = v.map(|p| {
            let (px, py) = self.xy(p);
            dist(x, y, px, py)
        });
        if d[0] < d[1] {
            if d[0] < d[2] { v[0] } else { v[2] }
        } else if d[1] < d[2] {
            v[1]
        } else {
            v[2]
        }
    }

    /// Containing triangle and the vertex within `tol`, if any.
    fn find_tp(&mut self, x: f64, y: f64) -> (i32, i32) {
        let t = self.find(x, y);
        if t == NONE {
            return (NONE, NONE);
        }
        let v = self.tri(t).v;
        let tol = self.tol;
        if v.iter().any(|&p| {
            let (px, py) = self.xy(p);
            close_to(x, y, px, py, tol)
        }) {
            return (t, self.closest(x, y, v));
        }
        (t, NONE)
    }

    fn find_nearest(&mut self, x: f64, y: f64) -> (i32, i32) {
        let t = self.find(x, y);
        if t == NONE {
            return (NONE, NONE);
        }
        (t, self.closest(x, y, self.tri(t).v))
    }

    fn update_zm(&mut self, p: i32, z: f64, m: f64) {
        if !z.is_nan() {
            self.z[p as usize] = z;
        }
        if !m.is_nan() {
            self.m[p as usize] = m;
        }
    }

    /// Whether (`x`, `y`) lies in the circumcircle of live triangle `at`.
    fn in_hole(&self, at: i32, x: f64, y: f64) -> Result<bool, TinError> {
        if at == NONE || self.deleted(at) {
            return Ok(false);
        }
        let tr = self.tri(at);
        let p = tr.v.map(|v| self.xy(v));
        let ((ax, ay), (bx, by), (cx, cy)) = (p[0], p[1], p[2]);
        let (n1, n2) = (bx - ax, by - ay);
        let n3 = n1 * n1 + n2 * n2;
        let (n4, n5) = (cx - ax, cy - ay);
        let n6 = n4 * n4 + n5 * n5;
        let n7 = 2.0 * (n1 * n5 - n2 * n4);
        if n7 == 0.0 {
            return Err(TinError("colinear triangle"));
        }
        let ccx = ax + (n5 * n3 - n2 * n6) / n7;
        let ccy = ay + (n1 * n6 - n4 * n3) / n7;
        let r = dist(ax, ay, ccx, ccy);
        if !close_to(x, y, ccx, ccy, r) {
            return Ok(false);
        }
        for (i, j) in [(0, 1), (0, 2), (1, 2)] {
            let ((ux, uy), (vx, vy)) = (p[i], p[j]);
            if ux == vx && vx == x && ((y > uy && y > vy) || (y < uy && y < vy)) {
                return Ok(false);
            }
        }
        for (i, j) in [(0, 1), (0, 2), (1, 2)] {
            let ((ux, uy), (vx, vy)) = (p[i], p[j]);
            if uy == vy && vy == y && ((x > ux && x > vx) || (x < ux && x < vx)) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Refill the cavity of the deleted stack around point `p`; returns the
    /// boundary edges `[a, b, outside neighbour, new triangle]`.
    fn fill_hole(&mut self, p: i32) -> Vec<[i32; 4]> {
        let mut edges: Vec<[i32; 4]> = Vec::new();
        for &item in self.ds.iter().rev() {
            let tr = self.t[item as usize];
            for e in 0..3 {
                let a = tr.n[e];
                if a == NONE || !self.deleted(a) {
                    edges.push([tr.v[e], tr.v[(e + 1) % 3], a, NONE]);
                }
            }
        }
        // point -> (triangle whose edge starts there, triangle whose edge ends there)
        let mut links: Vec<(i32, i32, i32)> = Vec::with_capacity(edges.len());
        fn link(links: &mut Vec<(i32, i32, i32)>, p: i32) -> &mut (i32, i32, i32) {
            let i = match links.iter().position(|l| l.0 == p) {
                Some(i) => i,
                None => {
                    links.push((p, 0, 0));
                    links.len() - 1
                }
            };
            &mut links[i]
        }
        let (mut fl2, mut fl3) = (false, false);
        for e in edges.iter_mut() {
            let nt = self.add_tri([e[0], e[1], p], [e[2], NONE, NONE]);
            e[3] = nt;
            link(&mut links, e[0]).1 = nt;
            link(&mut links, e[1]).2 = nt;
            if e[2] >= 0 {
                let o = &mut self.t[e[2] as usize];
                for k in 0..3 {
                    if o.v[k] == e[1] {
                        o.n[k] = nt;
                        fl2 = o.f & (1 << k) != 0;
                        fl3 = o.f & (8 << k) != 0;
                        break;
                    }
                }
                if fl2 || fl3 {
                    set_edge(&mut self.t[nt as usize], 0, fl3);
                }
            }
        }
        for e in &edges {
            let right = link(&mut links, e[1]).1;
            let left = link(&mut links, e[0]).2;
            let tr = &mut self.t[e[3] as usize];
            tr.n[1] = right;
            tr.n[2] = left;
        }
        edges
    }

    fn set_edge_type(&mut self, t: i32, pa: i32, pb: i32, perim: bool) {
        let is = |v: &[i32; 3], k: usize| {
            let (a, b) = (v[k], v[(k + 1) % 3]);
            (a == pa && b == pb) || (a == pb && b == pa)
        };
        let tr = &mut self.t[t as usize];
        let mut nb = 0;
        if let Some(k) = (0..3).find(|&k| is(&tr.v, k)) {
            set_edge(tr, k, perim);
            nb = tr.n[k];
        }
        if nb != NONE {
            let tr = &mut self.t[nb as usize];
            if let Some(k) = (0..3).find(|&k| is(&tr.v, k)) {
                set_edge(tr, k, perim);
            }
        }
    }

    fn insert(&mut self, x: f64, y: f64, z: f64, m: f64) -> Result<(), TinError> {
        let (t, p) = self.find_tp(x, y);
        if t == NONE {
            return Err(TinError("point outside the TIN"));
        }
        if p >= 0 {
            self.update_zm(p, z, m);
            return Ok(());
        }
        let p = self.add_point(x, y, z, m);
        let mut stack = vec![t];
        while let Some(t) = stack.pop() {
            self.ds.push(t);
            self.mark(t);
            for a in self.tri(t).n {
                if self.in_hole(a, x, y)? {
                    stack.push(a);
                }
            }
        }
        self.fill_hole(p);
        Ok(())
    }

    /// Rotate around vertex `p` of `t` to the triangle whose opposite edge
    /// faces (`bx`, `by`); returns it, that edge and a copy of it.
    fn wedge(&self, mut t: i32, p: i32, bx: f64, by: f64) -> Result<(i32, usize, Tri), TinError> {
        let (px, py) = self.xy(p);
        for _ in 0..100_000 {
            if t < 0 {
                return Err(TinError("wedge walk left the TIN"));
            }
            let tr = self.tri(t);
            let (e, n1, n2) = if p == tr.v[0] {
                (1, tr.n[0], tr.n[2])
            } else if p == tr.v[1] {
                (2, tr.n[1], tr.n[0])
            } else {
                (0, tr.n[2], tr.n[1])
            };
            let (ea, eb) = (self.xy(tr.v[e]), self.xy(tr.v[(e + 1) % 3]));
            if cross(px, py, ea.0, ea.1, bx, by) < 0.0 {
                t = n1;
                continue;
            }
            let c = cross(px, py, eb.0, eb.1, bx, by);
            // Not strictly left of p->eb (NaN included).
            if c <= 0.0 || c.is_nan() {
                return Ok((t, e, tr));
            }
            t = n2;
        }
        Err(TinError("wedge walk did not end"))
    }

    /// Make segment A->B a constraint edge, splitting crossed edges.
    fn enforce(&mut self, seg: &Seg, off: (f64, f64)) -> Result<(), TinError> {
        let (mut ax, mut ay) = (seg.a[0] + off.0, seg.a[1] + off.1);
        let (bx, by) = (seg.b[0] + off.0, seg.b[1] + off.1);
        let (t, p) = self.find_nearest(bx, by);
        if t == NONE {
            return Err(TinError("constraint end outside the TIN"));
        }
        let (bx, by, bz, bm) = (
            self.x[p as usize],
            self.y[p as usize],
            self.z[p as usize],
            self.m[p as usize],
        );
        let mut t3 = 0.0;
        for _ in 0..10_000 {
            let (t2, p2) = self.find_nearest(ax, ay);
            if t2 == NONE {
                return Err(TinError("constraint start outside the TIN"));
            }
            ax = self.x[p2 as usize];
            ay = self.y[p2 as usize];
            let (az, am) = (self.z[p2 as usize], self.m[p2 as usize]);
            let (t2, e, tr) = self.wedge(t2, p2, bx, by)?;
            if tr.v.contains(&p) {
                self.set_edge_type(t2, p2, p, false);
                return Ok(());
            }
            self.crossings += 1;
            let (ea, eb) = (tr.v[e], tr.v[(e + 1) % 3]);
            let ((sax, say), (sbx, sby)) = (self.xy(ea), self.xy(eb));
            if !(ax.max(bx) + 0.0 < sax.min(sbx)
                || ax.min(bx) - 0.0 > sax.max(sbx)
                || ay.max(by) + 0.0 < say.min(sby)
                || ay.min(by) - 0.0 > say.max(sby))
            {
                let (n1, n2, n3, n4) = (bx - ax, by - ay, sbx - sax, sby - say);
                let n5 = n4 * n1 - n3 * n2;
                if n5 != 0.0 {
                    let (n6, n7) = (ay - say, ax - sax);
                    t3 = (n3 * n6 - n4 * n7) / n5;
                }
            }
            let px = ax + t3 * (bx - ax);
            let py = ay + t3 * (by - ay);
            let pz = if !az.is_nan() && !bz.is_nan() {
                az + t3 * (bz - az)
            } else {
                f64::NAN
            };
            let pm = if !am.is_nan() && !bm.is_nan() {
                am + t3 * (bm - am)
            } else {
                f64::NAN
            };
            let tol = self.tol;
            let snap = if close_to(px, py, sax, say, tol) {
                Some(ea)
            } else if close_to(px, py, sbx, sby, tol) {
                Some(eb)
            } else {
                None
            };
            if let Some(q) = snap {
                self.set_edge_type(t2, p2, q, false);
                self.update_zm(q, pz, pm);
                (ax, ay) = self.xy(q);
                continue;
            }
            let cnt = self.add_point(px, py, pz, pm);
            let n6t = tr.n[e];
            if n6t < 0 {
                return Err(TinError("constraint crosses the TIN hull"));
            }
            let fl = tr.f & (1 << e) != 0;
            let fl2 = tr.f & (8 << e) != 0;
            self.ds.push(t2);
            self.ds.push(n6t);
            self.mark(t2);
            self.mark(n6t);
            let edges = self.fill_hole(cnt);
            if fl || fl2 {
                for it in &edges {
                    self.set_edge_type(it[3], cnt, ea, fl2);
                    self.set_edge_type(it[3], cnt, eb, fl2);
                }
            }
            let t5 = (self.t.len() - 1) as i32;
            self.set_edge_type(t5, p2, cnt, false);
            ax = px;
            ay = py;
        }
        Err(TinError("constraint enforcement did not end"))
    }

    /// Delete the fan around `pt` from `t0`: one way until a breakline, the
    /// hull, the start or a super triangle, then the other way from there.
    fn rotate_delete_pt(&mut self, t0: i32, pt: i32) -> Result<(), TinError> {
        let mut cur = t0;
        self.mark(cur);
        let limit = self.t.len() + 1;
        let mut steps = 0;
        let mut bl;
        loop {
            let tr = self.tri(cur);
            let nx;
            if tr.v[0] == pt {
                bl = tr.f & 9 != 0;
                nx = tr.n[0];
            } else if tr.v[1] == pt {
                bl = tr.f & 18 != 0;
                nx = tr.n[1];
            } else if tr.v[2] == pt {
                bl = tr.f & 36 != 0;
                nx = tr.n[2];
            } else {
                bl = false;
                nx = NONE;
            }
            if bl || nx == NONE || nx == t0 || self.is_super(nx) {
                break;
            }
            cur = nx;
            self.mark(cur);
            steps += 1;
            if steps > limit {
                return Err(TinError("fan deletion did not end"));
            }
        }
        loop {
            let tr = self.tri(cur);
            let nx;
            if tr.v[0] == pt {
                bl = tr.f & 36 != 0;
                nx = tr.n[2];
            } else if tr.v[1] == pt {
                bl = tr.f & 9 != 0;
                nx = tr.n[0];
            } else if tr.v[2] == pt {
                bl = tr.f & 18 != 0;
                nx = tr.n[1];
            } else {
                nx = NONE;
            }
            if !bl && nx != NONE && nx != t0 && !self.is_super(nx) {
                cur = nx;
                self.mark(cur);
                steps += 1;
                if steps > 2 * limit {
                    return Err(TinError("fan deletion did not end"));
                }
                continue;
            }
            return Ok(());
        }
    }

    fn rotate_delete(&mut self, t0: i32) -> Result<(), TinError> {
        for p in self.tri(t0).v {
            self.rotate_delete_pt(t0, p)?;
        }
        Ok(())
    }

    /// Delete the triangles on the right of A->B (the block interior).
    fn delete_right_of(&mut self, seg: &Seg, off: (f64, f64)) -> Result<(), TinError> {
        let (ax, ay) = (seg.a[0] + off.0, seg.a[1] + off.1);
        let (bx, by) = (seg.b[0] + off.0, seg.b[1] + off.1);
        let (mut t, mut p) = self.find_tp(ax, ay);
        if t == NONE {
            return Err(TinError("deletion start outside the TIN"));
        }
        if p == NONE {
            p = self.tri(t).v[1];
        }
        let mut seen: Vec<i32> = Vec::new();
        loop {
            let (t2, e, tr) = self.wedge(t, p, bx, by)?;
            t = t2;
            if seen.contains(&t) {
                return Ok(());
            }
            seen.push(t);
            let (ea, eb) = (tr.v[e], tr.v[(e + 1) % 3]);
            let d3 = seg_dist(ax, ay, bx, by, self.xy(ea));
            let d4 = seg_dist(ax, ay, bx, by, self.xy(eb));
            if d3 < d4 {
                p = ea;
                let n5 = tr.n[(e + 2) % 3];
                if n5 < 0 {
                    return Err(TinError("deletion crosses the TIN hull"));
                }
                self.mark(n5);
                self.rotate_delete(n5)?;
            } else {
                p = eb;
                self.mark(t);
                self.rotate_delete(t)?;
            }
            let (px, py) = self.xy(p);
            if close_to(px, py, bx, by, self.tol) {
                return Ok(());
            }
        }
    }
}

fn set_edge(tr: &mut Tri, e: usize, perim: bool) {
    tr.f |= if perim { 8 << e } else { 1 << e };
}

/// Distance from `p` to segment A-B.
fn seg_dist(ax: f64, ay: f64, bx: f64, by: f64, (px, py): (f64, f64)) -> f64 {
    let l = dist(ax, ay, bx, by);
    if l == 0.0 {
        return dist(ax, ay, px, py);
    }
    let (n1, n2) = (px - ax, py - ay);
    let n3 = ((bx - ax) * n1 + (by - ay) * n2) / l;
    if n3 < 0.0 {
        return dist(ax, ay, px, py);
    }
    if n3 > l {
        return dist(bx, by, px, py);
    }
    let n4 = dist(ax, ay, px, py);
    let n5 = n4 * n4 - n3 * n3;
    if n5 > 0.0 { n5.sqrt() } else { 0.0 }
}

/// A clean TIN: points (x, y, z) and live triangles in slot order.
pub(crate) struct CleanTin {
    pub points: Vec<[f64; 3]>,
    pub triangles: Vec<[i32; 3]>,
    pub crossings: u64,
}

fn bbox(segs: &[Seg]) -> (f64, f64, f64, f64) {
    let mut e = (
        f64::NEG_INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::INFINITY,
    );
    for s in segs {
        for p in [&s.a, &s.b] {
            e = (e.0.max(p[0]), e.1.min(p[0]), e.2.max(p[1]), e.3.min(p[1]));
        }
    }
    e
}

fn widen(e: (f64, f64, f64, f64), w: f64, h: f64) -> (f64, f64, f64, f64) {
    let (mut mx_x, mut mn_x, mut mx_y, mut mn_y) = e;
    if w > 0.0 && (mx_x - mn_x) < w {
        let c = (mx_x + mn_x) * 0.5;
        (mx_x, mn_x) = (c + w / 2.0, c - w / 2.0);
    }
    if h > 0.0 && (mx_y - mn_y) < h {
        let c = (mx_y + mn_y) * 0.5;
        (mx_y, mn_y) = (c + h / 2.0, c - h / 2.0);
    }
    (mx_x, mn_x, mx_y, mn_y)
}

/// Triangulate `segs` (non-empty), enforce them, delete and clean.
pub(crate) fn build_tin(segs: &[Seg]) -> Result<CleanTin, TinError> {
    let min_m = segs.iter().map(|s| s.a[3]).fold(f64::INFINITY, f64::min);
    let tol = 0.01_f64.min(min_m * 0.1);
    let sb = bbox(segs);
    let (cx, cy) = ((sb.0 + sb.1) * 0.5, (sb.2 + sb.3) * 0.5);
    let mbr = (
        cx + (sb.0 - cx) * 1.5,
        cx - (cx - sb.1) * 1.5,
        cy + (sb.2 - cy) * 1.5,
        cy - (cy - sb.3) * 1.5,
    );
    let mut e = widen(sb, tol * 10.0, tol * 10.0);
    let (dx, dy) = (e.0 - e.1, e.2 - e.3);
    if dx > dy {
        let nn = dx * 0.125;
        if dy < nn {
            e = widen(e, 0.0, nn);
        }
    } else {
        let nn = dy * 0.125;
        if dx < nn {
            e = widen(e, nn, 0.0);
        }
    }
    let (mx_x, mn_x, mx_y, mn_y) = (
        e.0.max(mbr.0),
        e.1.min(mbr.1),
        e.2.max(mbr.2),
        e.3.min(mbr.3),
    );
    let off = (-mn_x, -mn_y);
    let (ex1, ex0, ey1, ey0) = (mx_x + off.0, mn_x + off.0, mx_y + off.1, mn_y + off.1);
    let num = (ex1 - ex0).max(ey1 - ey0) * 0.05;
    let (midx, midy) = ((ex0 + ex1) * 0.5, (ey1 + ey0) * 0.5);
    let mut f = Factory::new(tol);
    for (x, y) in [
        (midx + 0.0, ey1 + num * 2.0),
        (ex1 + num, ey1 + num),
        (ex1 + num * 2.0, midy + 0.0),
        (ex1 + num, ey0 + (0.0 - num)),
        (midx + 0.0, ey0 + (0.0 - num) * 2.0),
        (ex0 + (0.0 - num), ey0 + (0.0 - num)),
        (ex0 + (0.0 - num) * 2.0, midy + 0.0),
        (ex0 + (0.0 - num), ey1 + num),
    ] {
        f.add_point(x, y, f64::NAN, f64::NAN);
    }
    let start: [([i32; 3], [i32; 3]); 6] = if (mx_x - mn_x) > (mx_y - mn_y) {
        [
            ([0, 7, 6], [-1, -1, 4]),
            ([6, 5, 4], [-1, -1, 4]),
            ([4, 3, 2], [-1, -1, 5]),
            ([2, 1, 0], [-1, -1, 5]),
            ([0, 6, 4], [0, 1, 5]),
            ([0, 4, 2], [4, 2, 3]),
        ]
    } else {
        [
            ([0, 7, 6], [-1, -1, 4]),
            ([6, 5, 4], [-1, -1, 5]),
            ([4, 3, 2], [-1, -1, 5]),
            ([2, 1, 0], [-1, -1, 4]),
            ([0, 6, 2], [0, 5, 3]),
            ([6, 4, 2], [1, 2, 4]),
        ]
    };
    for (v, n) in start {
        f.add_tri(v, n);
    }
    for s in segs {
        f.insert(s.a[0] + off.0, s.a[1] + off.1, s.a[2] + 0.0, s.a[3] + 0.0)?;
        f.insert(s.b[0] + off.0, s.b[1] + off.1, s.b[2] + 0.0, s.b[3] + 0.0)?;
    }
    for s in segs {
        f.enforce(s, off)?;
    }
    // 1. Triangles touching the super polygon.
    for t in f.t.iter_mut() {
        if t.v.iter().any(|&p| p < 8) {
            t.f |= DEL;
        }
    }
    // 2. Size: any edge wider or taller than 2.0001 x the largest cell.
    let max_m = segs.iter().map(|s| s.a[3]).fold(0.0_f64, f64::max);
    let lim = max_m * 2.0001;
    for i in 0..f.t.len() {
        let tr = f.t[i];
        if tr.f & DEL != 0 {
            continue;
        }
        let too_big = (0..3).any(|k| {
            let (a, b) = (tr.v[k] as usize, tr.v[(k + 1) % 3] as usize);
            (f.x[a] - f.x[b]).abs() > lim || (f.y[a] - f.y[b]).abs() > lim
        });
        if too_big {
            f.t[i].f |= DEL;
        }
    }
    // 3. The data side of every segment.
    for s in segs {
        f.delete_right_of(s, off)?;
    }
    let points = (0..f.x.len())
        .map(|i| [f.x[i] + mn_x, f.y[i] + mn_y, f.z[i] + 0.0])
        .collect();
    let triangles = f.t.iter().filter(|t| t.f & DEL == 0).map(|t| t.v).collect();
    Ok(CleanTin {
        points,
        triangles,
        crossings: f.crossings,
    })
}

/// Rasterize `tin` over tile `ras` into `buf` (256 x 256, NaN = not
/// written); returns the number of writes.
pub(crate) fn rasterize(tin: &CleanTin, ras: &TileGrid, buf: &mut [f32]) -> u64 {
    let (rows, cols) = (TILE as i64, TILE as i64);
    let cs = ras.cs;
    let mut writes = 0;
    for tri in &tin.triangles {
        let [p0, p1, p2] = tri.map(|i| tin.points[i as usize]);
        let ([x0, y0, z0], [x1, y1, z1], [x2, y2, z2]) = (p0, p1, p2);
        if (y0 == y1 && y1 == y2) || (x0 == x1 && x1 == x2) {
            continue;
        }
        let mnx = x0.min(x1).min(x2);
        let mxx = x0.max(x1).max(x2);
        let mny = y0.min(y1).min(y2);
        let mxy = y0.max(y1).max(y2);
        if mnx > ras.max_x || mxx < ras.min_x || mny > ras.max_y || mxy < ras.min_y {
            continue;
        }
        let c0 = if mnx < ras.min_x { 0 } else { ras.col(mnx) };
        let rb = if mny < ras.min_y {
            rows - 1
        } else {
            ras.row(mny)
        };
        let c1 = if mxx > ras.max_x {
            cols - 1
        } else {
            ras.col(mxx)
        };
        let rt = if mxy > ras.max_y { 0 } else { ras.row(mxy) };
        let (vx, vy) = (ras.ccx(c0), ras.ccy(rb));
        let (q0x, q0y) = (x0 - vx, y0 - vy);
        let (q1x, q1y) = (x1 - vx, y1 - vy);
        let (q2x, q2y) = (x2 - vx, y2 - vy);
        // edge (u, v): step x = u.y - v.y, step y = v.x - u.x, start = u.x v.y - u.y v.x
        let (a_dx, a_dy) = (q1y - q2y, q2x - q1x);
        let mut a_s = (a_dx * 0.0 + a_dy * 0.0) + (q1x * q2y - q1y * q2x);
        let (b_dx, b_dy) = (q2y - q0y, q0x - q2x);
        let mut b_s = (b_dx * 0.0 + b_dy * 0.0) + (q2x * q0y - q2y * q0x);
        let (c_dx, c_dy) = (q0y - q1y, q1x - q0x);
        let mut c_s = (c_dx * 0.0 + c_dy * 0.0) + (q0x * q1y - q0y * q1x);
        let (sx0, sx1, sx2) = (a_dx * cs, b_dx * cs, c_dx * cs);
        let (sy0, sy1, sy2) = (a_dy * cs, b_dy * cs, c_dy * cs);
        let s = a_s + b_s + c_s;
        let mut i = rb;
        while i >= rt {
            let (mut w0, mut w1, mut w2) = (a_s, b_s, c_s);
            for j in c0..=c1 {
                if w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0 {
                    let z = ((w0 * z0 + w1 * z1) + w2 * z2) / s;
                    // Writes are clipped to the tile (and, by the caller,
                    // to the export grid).
                    if (0..rows).contains(&i) && (0..cols).contains(&j) && !z.is_nan() {
                        buf[(i * cols + j) as usize] = z as f32;
                        writes += 1;
                    }
                }
                w0 += sx0;
                w1 += sx1;
                w2 += sx2;
            }
            a_s += sy0;
            b_s += sy1;
            c_s += sy2;
            i -= 1;
        }
    }
    writes
}

/// One output tile's export TIN.
#[derive(Default)]
pub(crate) struct TileOutcome {
    /// 256 x 256 binary32 values, NaN where the TIN wrote nothing.
    pub values: Option<Vec<f32>>,
    pub segments: u64,
    pub triangles: u64,
    pub writes: u64,
    pub crossings: u64,
    pub error: Option<TinError>,
}

/// Collect, triangulate and rasterize output tile `ras`.
pub(crate) fn tile(
    sources: &[&dyn BlockSource],
    ras: &TileGrid,
    plan: &mut Plan,
) -> Result<TileOutcome, MapError> {
    let mut segs = Vec::new();
    for s in sources {
        segs.extend(collect_source(*s, ras, plan)?);
    }
    if segs.is_empty() {
        return Ok(TileOutcome::default());
    }
    let mut out = TileOutcome {
        segments: segs.len() as u64,
        ..Default::default()
    };
    plan.add(TILE_RESULT_BYTES)?;
    match build_tin(&segs) {
        Ok(tin) => {
            let mut buf = vec![f32::NAN; TILE * TILE];
            out.writes = rasterize(&tin, ras, &mut buf);
            out.triangles = tin.triangles.len() as u64;
            out.crossings = tin.crossings;
            out.values = Some(buf);
        }
        Err(e) => out.error = Some(e),
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unit tile over [0, 256] x [0, 256], pixel size 1.
    fn unit_tile() -> TileGrid {
        TileGrid::new(0.0, 256.0, 0.0, 256.0)
    }

    fn p(x: f64, y: f64, z: f64, m: f64) -> [f64; 4] {
        [x, y, z, m]
    }

    /// Edges of a valid region [x0, x1] x [y0, y1] of unit blocks (z = x):
    /// one segment per block edge on the region's boundary, interior on the
    /// right (clockwise, y up).
    fn region(x0: i32, x1: i32, y0: i32, y1: i32, m: f64) -> Vec<Seg> {
        let mut s = Vec::new();
        let pt = |x: i32, y: i32| p(x as f64 * m, y as f64 * m, x as f64, m);
        for x in x0..x1 {
            s.push(Seg {
                a: pt(x, y1),
                b: pt(x + 1, y1),
            });
            s.push(Seg {
                a: pt(x + 1, y0),
                b: pt(x, y0),
            });
        }
        for y in y0..y1 {
            s.push(Seg {
                a: pt(x1, y + 1),
                b: pt(x1, y),
            });
            s.push(Seg {
                a: pt(x0, y),
                b: pt(x0, y + 1),
            });
        }
        s
    }

    fn render(segs: &[Seg]) -> Vec<f32> {
        let tin = build_tin(segs).unwrap();
        let mut buf = vec![f32::NAN; TILE * TILE];
        rasterize(&tin, &unit_tile(), &mut buf);
        buf
    }

    /// Pixel (row, col) of the unit tile has centre (col + 0.5, 255.5 - row).
    fn at(buf: &[f32], x: f64, y: f64) -> f32 {
        let (c, r) = (x.floor() as usize, (256.0 - y).floor() as usize);
        buf[r * TILE + c]
    }

    #[test]
    fn hash_matches_the_measured_type_id() {
        assert_eq!(
            HASH_K,
            7_u32.wrapping_mul(711_650_207).wrapping_add(2_506_965_631)
        );
        let b = 3.5_f64.to_bits();
        assert_eq!(
            seg_key(3.5),
            (HASH_K ^ (b as u32) ^ ((b >> 32) as u32)) as i32
        );
    }

    #[test]
    fn one_cell_gap_is_bridged() {
        // Two valid regions with a two-block (one source cell) gap between
        // x = 12 and x = 14 (a block spans two cell centres).
        let mut segs = region(10, 12, 10, 14, 1.0);
        segs.extend(region(14, 16, 10, 14, 1.0));
        let buf = render(&segs);
        for y in [10.5, 11.5, 12.5, 13.5] {
            assert_eq!(at(&buf, 12.5, y), 12.5, "y {y}");
            assert_eq!(at(&buf, 13.5, y), 13.5, "y {y}");
        }
        // Inside the data and outside the regions nothing is written.
        assert!(at(&buf, 11.5, 12.5).is_nan());
        assert!(at(&buf, 15.5, 12.5).is_nan());
        assert!(at(&buf, 12.5, 14.5).is_nan());
        assert!(at(&buf, 5.5, 5.5).is_nan());
    }

    #[test]
    fn two_cell_gap_is_not_bridged() {
        let mut segs = region(10, 12, 10, 14, 1.0);
        segs.extend(region(15, 17, 10, 14, 1.0));
        let buf = render(&segs);
        assert!(buf.iter().all(|v| v.is_nan()));
    }

    #[test]
    fn size_rule_uses_the_largest_cell() {
        // Gap 3 units: closed when one source's cell size is 1.5 (limit
        // 2.0001 x 1.5), not with unit cells only.
        let mut segs = region(10, 12, 10, 14, 1.0);
        segs.extend(region(15, 17, 10, 14, 1.0));
        segs.push(Seg {
            a: p(100.0, 100.0, 0.0, 1.5),
            b: p(101.5, 100.0, 0.0, 1.5),
        });
        let buf = render(&segs);
        assert_eq!(at(&buf, 13.5, 12.5), 13.5);
    }

    #[test]
    fn cocircular_square_is_triangulated_once() {
        // A one-block hole in the middle of a 3 x 3 block region: the four
        // hole corners are cocircular; the hole is covered with z = x.
        let mut segs = Vec::new();
        let pt = |x: f64, y: f64| p(x, y, x, 1.0);
        segs.extend(region(20, 23, 20, 23, 1.0));
        // Hole block [21, 22] x [21, 22]: its neighbours' edges facing it,
        // interior (the data) on the right.
        segs.push(Seg {
            a: pt(22.0, 22.0),
            b: pt(21.0, 22.0),
        });
        segs.push(Seg {
            a: pt(21.0, 21.0),
            b: pt(22.0, 21.0),
        });
        segs.push(Seg {
            a: pt(21.0, 22.0),
            b: pt(21.0, 21.0),
        });
        segs.push(Seg {
            a: pt(22.0, 21.0),
            b: pt(22.0, 22.0),
        });
        let tin = build_tin(&segs).unwrap();
        let mut buf = vec![f32::NAN; TILE * TILE];
        rasterize(&tin, &unit_tile(), &mut buf);
        assert_eq!(at(&buf, 21.5, 21.5), 21.5);
        // Exactly two live triangles cover the hole, none the data.
        assert_eq!(tin.triangles.len(), 2);
    }

    #[test]
    fn visit_lists_fill_skips_and_add_a_halo() {
        // Source blocks twice as fine as the output pixels.
        let rb = BlockLattice {
            min_x: 0.0,
            max_y: 512.0,
            pitch: 128.0,
            tiles_x: 4,
            tiles_y: 4,
        };
        let ras = TileGrid::new(0.0, 256.0, 256.0, 512.0);
        let (rows, cols) = visit_lists(&[0.0, 512.0, 0.0, 512.0], &rb, &ras).unwrap();
        assert_eq!(cols.len(), 3);
        // Pixel centres hit odd blocks; the even ones between are filled,
        // and the halo adds block 0 of the first tile.
        assert_eq!(cols[0], (0, (0..256).collect::<Vec<_>>()));
        // Not filled across a block-tile change.
        assert_eq!(cols[1], (1, (1..256).collect::<Vec<_>>()));
        assert_eq!(cols[2], (2, vec![0, 1]));
        assert_eq!(rows[0].1[0], 0);
        assert_eq!(rows.last().unwrap(), &(2, vec![0, 1]));
    }
}
