//! Window planning on the terrain lattice: `_horizontal.plan_grid(native=True)`
//! and the cell validation `_horizontal.prepare_grid(read_terrain=False)`.
//!
//! Affine arithmetic follows rasterio 1.5 / affine 3 operation order exactly:
//! `(col * a + row * b) + c` for points and `Affine.translation(x - c, y - f) * t`
//! for window transforms.

use crate::cells::Cells;
use crate::error::{Error, Result};
use crate::geom::{self, Envelope};
use geos::{Geom, Geometry};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// rasterio `Affine(a, b, c, d, e, f)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub e: f64,
    pub f: f64,
}

impl Affine {
    pub fn from_gdal(gt: [f64; 6]) -> Self {
        Self {
            a: gt[1],
            b: gt[2],
            c: gt[0],
            d: gt[4],
            e: gt[5],
            f: gt[3],
        }
    }
    pub fn to_gdal(self) -> [f64; 6] {
        [self.c, self.a, self.b, self.f, self.d, self.e]
    }
    /// `transform * (vx, vy)`.
    pub fn apply(self, vx: f64, vy: f64) -> (f64, f64) {
        (
            vx * self.a + vy * self.b + self.c,
            vx * self.d + vy * self.e + self.f,
        )
    }
    /// `self * other` (matrix product).
    pub fn compose(self, o: Affine) -> Affine {
        Affine {
            a: self.a * o.a + self.b * o.d,
            b: self.a * o.b + self.b * o.e,
            c: self.a * o.c + self.b * o.f + self.c,
            d: self.d * o.a + self.e * o.d,
            e: self.d * o.b + self.e * o.e,
            f: self.d * o.c + self.e * o.f + self.f,
        }
    }
    /// `rasterio.windows.transform(window, self)`.
    pub fn window_transform(self, col: i64, row: i64) -> Affine {
        let (x, y) = self.apply(col as f64, row as f64);
        let t = Affine {
            a: 1.0,
            b: 0.0,
            c: x - self.c,
            d: 0.0,
            e: 1.0,
            f: y - self.f,
        };
        t.compose(self)
    }
}

/// The terrain facts plan_grid checks (rasterio header reads).
pub struct Terrain {
    pub path: String,
    pub transform: Affine,
    pub width: i64,
    pub height: i64,
    pub block: (i64, i64),
    pub epsg: u32,
}

/// Internal mask or alpha (rasterio `MaskFlags.per_dataset` / `alpha`).
fn tiff_mask_or_alpha(path: &Path) -> Result<bool> {
    let mut f = std::fs::File::open(path)?;
    let mut head = [0u8; 16];
    f.read_exact(&mut head[..8])?;
    let little = match &head[..2] {
        b"II" => true,
        b"MM" => false,
        _ => return Err(Error::unsupported("terrain is not a TIFF")),
    };
    let u16_ = |b: &[u8]| {
        if little {
            u16::from_le_bytes([b[0], b[1]])
        } else {
            u16::from_be_bytes([b[0], b[1]])
        }
    };
    let u32_ = |b: &[u8]| {
        if little {
            u32::from_le_bytes(b[..4].try_into().unwrap())
        } else {
            u32::from_be_bytes(b[..4].try_into().unwrap())
        }
    };
    let u64_ = |b: &[u8]| {
        if little {
            u64::from_le_bytes(b[..8].try_into().unwrap())
        } else {
            u64::from_be_bytes(b[..8].try_into().unwrap())
        }
    };
    let big = match u16_(&head[2..4]) {
        42 => false,
        43 => {
            f.read_exact(&mut head[8..16])?;
            true
        }
        _ => return Err(Error::unsupported("terrain is not a TIFF")),
    };
    let mut next = if big {
        u64_(&head[8..16])
    } else {
        u64::from(u32_(&head[4..8]))
    };
    let mut first = true;
    let mut guard = 0;
    while next != 0 && guard < 4096 {
        guard += 1;
        f.seek(SeekFrom::Start(next))?;
        let mut nb = [0u8; 8];
        let count = if big {
            f.read_exact(&mut nb)?;
            u64_(&nb)
        } else {
            f.read_exact(&mut nb[..2])?;
            u64::from(u16_(&nb[..2]))
        };
        let size = if big { 20 } else { 12 };
        let mut entries = vec![0u8; (count as usize).min(1 << 16) * size];
        f.read_exact(&mut entries)?;
        for e in entries.chunks_exact(size) {
            let tag = u16_(&e[0..2]);
            let value = if big {
                u64_(&e[12..20])
            } else {
                u64::from(u32_(&e[8..12]))
            };
            let typ = u16_(&e[2..4]);
            let short = if typ == 3 {
                u64::from(u16_(if big { &e[12..14] } else { &e[8..10] }))
            } else {
                value
            };
            if tag == 254 && !first && short & 4 != 0 {
                return Ok(true);
            }
            if tag == 338 && first && (short == 1 || short == 2) {
                return Ok(true);
            }
        }
        let mut nn = [0u8; 8];
        next = if big {
            f.read_exact(&mut nn)?;
            u64_(&nn)
        } else {
            f.read_exact(&mut nn[..4])?;
            u64::from(u32_(&nn[..4]))
        };
        first = false;
    }
    Ok(false)
}

impl Terrain {
    /// plan_grid's terrain checks (single-band float32, nodata declared, no
    /// dataset/alpha mask, north-up square pixels, EPSG-coded CRS).
    pub fn open(path: &Path) -> Result<Self> {
        let p = path.to_string_lossy().to_string();
        let info = ras_commander_fast_map::terrain::inspect(&p)?;
        if info.band_count != 1 || info.data_type != "F32" {
            return Err(Error::unsupported(
                "horizontal mapping requires single-band float32 terrain",
            ));
        }
        let msk = format!("{p}.msk");
        if Path::new(&msk).exists() || tiff_mask_or_alpha(path)? {
            return Err(Error::unsupported(
                "horizontal terrain with a dataset/alpha mask is unsupported; export nodata-tagged terrain",
            ));
        }
        if info.nodata.is_none() {
            return Err(Error::unsupported(
                "horizontal terrain must declare its nodata value",
            ));
        }
        let t = Affine::from_gdal(info.geo_transform);
        let close = (t.a - (-t.e)).abs() <= 1e-12 * (-t.e).abs();
        if t.b != 0.0 || t.d != 0.0 || t.a <= 0.0 || t.e >= 0.0 || !close {
            return Err(Error::unsupported(
                "horizontal mapping requires north-up square terrain pixels",
            ));
        }
        let epsg = info.epsg.ok_or_else(|| {
            Error::unsupported(
                "terrain CRS has no EPSG code; this preparation requires an EPSG-coded terrain",
            )
        })?;
        Ok(Self {
            path: p,
            transform: t,
            width: info.width as i64,
            height: info.height as i64,
            // rasterio block_shapes[0] is (rows, columns); terrio reports (columns, rows).
            block: (info.block.1 as i64, info.block.0 as i64),
            epsg,
        })
    }
}

pub struct GridPlan {
    pub window: [i64; 4],
    pub transform: Affine,
    pub source: Affine,
    pub positions: Vec<usize>,
    pub planned_bytes: u64,
    pub transport: u64,
}

impl GridPlan {
    pub fn shape(&self) -> (i64, i64) {
        (self.window[3], self.window[2])
    }
}

fn total_bounds(cells: &Cells) -> Envelope {
    let mut e = Envelope {
        min: [f64::INFINITY; 2],
        max: [f64::NEG_INFINITY; 2],
    };
    for r in &cells.rows {
        e.min[0] = e.min[0].min(r.env.min[0]);
        e.min[1] = e.min[1].min(r.env.min[1]);
        e.max[0] = e.max[0].max(r.env.max[0]);
        e.max[1] = e.max[1].max(r.env.max[1]);
    }
    e
}

fn interior_rings(g: &Geometry) -> Result<usize> {
    Ok(geom::polygon_rings(g)?
        .iter()
        .map(|p| p.len().saturating_sub(1))
        .sum())
}

/// `_horizontal.plan_grid(cells, terrain, aoi, max_working_mib, native=True)`.
pub fn plan_grid(
    cells: &Cells,
    terrain: &Terrain,
    aoi: &Geometry,
    max_working_mib: u64,
) -> Result<GridPlan> {
    if cells.rows.len() as u64 >= i32::MAX as u64 {
        return Err(Error::unsupported(
            "horizontal mapping has too many cells for int32 ownership",
        ));
    }
    let b = total_bounds(cells);
    let mut bounds = [b.min[0], b.min[1], b.max[0], b.max[1]];
    if !bounds.iter().all(|v| v.is_finite()) {
        return Err(Error::unsupported(
            "horizontal cell coordinates must be finite",
        ));
    }
    let t = terrain.transform;
    let (bh, bw) = terrain.block;
    let block_bytes = (bh * bw * 4) as u64;
    if block_bytes > 64 << 20 {
        return Err(Error::unsupported(
            "horizontal terrain block exceeds 64 MiB; export tiled terrain",
        ));
    }
    let transient = block_bytes * 4
        + (((terrain.width + bw - 1) / bw) * ((terrain.height + bh - 1) / bh) * 16) as u64;
    let (fc0, fc1) = crate::aoi::center_index_range(bounds[0], bounds[2], t.c, t.a, terrain.width);
    let (fr0, fr1) = crate::aoi::center_index_range(bounds[1], bounds[3], t.f, t.e, terrain.height);
    let ab = geom::envelope_any(aoi)?;
    bounds = [
        bounds[0].max(ab.min[0]),
        bounds[1].max(ab.min[1]),
        bounds[2].min(ab.max[0]),
        bounds[3].min(ab.max[1]),
    ];
    let (c0, c1, r0, r1) = if bounds[0] >= bounds[2] || bounds[1] >= bounds[3] {
        (0, 0, 0, 0)
    } else {
        let (c0, c1) = crate::aoi::index_range(bounds[0], bounds[2], t.c, t.a, terrain.width);
        let (r0, r1) = crate::aoi::index_range(bounds[1], bounds[3], t.f, t.e, terrain.height);
        (c0.max(fc0), c1.min(fc1), r0.max(fr0), r1.min(fr1))
    };
    let mut window = [c0, r0, (c1 - c0).max(0), (r1 - r0).max(0)];
    let transform = t.window_transform(window[0], window[1]);
    let mut positions = Vec::new();
    if window[2] > 0 && window[3] > 0 {
        let (left, bottom) = t.apply(window[0] as f64, (window[1] + window[3]) as f64);
        let (right, top) = t.apply((window[0] + window[2]) as f64, window[1] as f64);
        let rect = geom::rect(left, bottom, right, top)?;
        positions = cells
            .index
            .query_intersects(&rect, |i| &cells.rows[i].geom)?;
        positions.sort_unstable();
        if positions.is_empty() {
            window = [c0, r0, 0, 0];
        }
    }
    let pixels = (window[2] * window[3]) as u64;
    let mut planned = if pixels > 0 {
        pixels * 40 + positions.len() as u64 * 64 + (64 << 20) + transient
    } else {
        0
    };
    let mut transport = 0u64;
    if pixels > 0 {
        for &i in &positions {
            let g = &cells.rows[i].geom;
            transport += geom::count_coordinates(g)? as u64 * 16 + 128;
            transport += interior_rings(g)? as u64 * 4;
        }
        for part in geom::parts(aoi)? {
            transport += geom::count_coordinates(&part)? as u64 * 16 + 128;
            transport += interior_rings(&part)? as u64 * 4;
        }
        planned += transport * 12;
    }
    if planned > max_working_mib * (1 << 20) {
        return Err(Error::unsupported(format!(
            "horizontal window planned {planned} bytes exceeds max_working_mib={max_working_mib}; use a smaller bbox or polygon"
        )));
    }
    Ok(GridPlan {
        window,
        transform,
        source: t,
        positions,
        planned_bytes: planned,
        transport,
    })
}

/// `np.spacing(x)` for a positive finite double: the gap to the next double.
fn spacing(x: f64) -> f64 {
    let x = x.abs();
    f64::from_bits(x.to_bits() + 1) - x
}

/// `_numerical_overlap_without_sample(intersection, plan)`.
fn residue_without_sample(intersection: &Geometry, plan: &GridPlan) -> Result<bool> {
    let e = geom::envelope_any(intersection)?;
    let bounds = [e.min[0], e.min[1], e.max[0], e.max[1]];
    let scale = bounds.iter().fold(1.0f64, |m, v| m.max(v.abs()));
    let envelope = 8.0 * spacing(scale).abs() * intersection.length()?.max(1.0);
    if !envelope.is_finite() || intersection.area()? > envelope {
        return Ok(false);
    }
    let t = plan.transform;
    let (h, w) = plan.shape();
    let c0 = (((e.min[0] - t.c) / t.a - 0.5).ceil() as i64 - 1).max(0);
    let c1 = (((e.max[0] - t.c) / t.a - 0.5).floor() as i64 + 1).min(w - 1);
    let r0 = (((e.max[1] - t.f) / t.e - 0.5).ceil() as i64 - 1).max(0);
    let r1 = (((e.min[1] - t.f) / t.e - 0.5).floor() as i64 + 1).min(h - 1);
    if c1 < c0 || r1 < r0 {
        return Ok(true);
    }
    if (c1 - c0 + 1) * (r1 - r0 + 1) > 4096 {
        return Ok(false);
    }
    let point = |x: f64, y: f64| -> Result<Geometry> {
        geom::multipoint(&[[x, y]]).and_then(|m| Ok(geom::parts(&m)?.remove(0)))
    };
    for r in r0..=r1 {
        for c in c0..=c1 {
            let p = point(t.c + (c as f64 + 0.5) * t.a, t.f + (r as f64 + 0.5) * t.e)?;
            if intersection.contains(&p)? {
                return Ok(false);
            }
        }
    }
    let s = plan.source;
    for r in r0..=r1 {
        for c in c0..=c1 {
            let p = point(
                s.c + (c as f64 + plan.window[0] as f64 + 0.5) * s.a,
                s.f + (r as f64 + plan.window[1] as f64 + 0.5) * s.e,
            )?;
            if intersection.contains(&p)? {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

/// `prepared._validate_grid(plan)` -> `prepare_grid(cells, terrain, plan, False)`:
/// every window cell is a valid non-empty Polygon and no two have overlapping
/// interiors except roundoff residue holding no pixel centre. Returns the
/// number of tolerated residues.
pub fn validate_grid(cells: &Cells, plan: &GridPlan) -> Result<usize> {
    if plan.window[2] == 0 || plan.window[3] == 0 {
        return Ok(0);
    }
    let geoms: Vec<&Geometry> = plan
        .positions
        .iter()
        .map(|&i| &cells.rows[i].geom)
        .collect();
    for g in &geoms {
        if !geom::is_polygon(*g)? || g.is_empty()? || !g.is_valid()? {
            return Err(Error::unsupported(
                "horizontal mapping requires valid, non-empty cell Polygons",
            ));
        }
    }
    let envs: Vec<Envelope> = plan.positions.iter().map(|&i| cells.rows[i].env).collect();
    let local = geom::Index::new(&envs);
    let mut tolerated = 0;
    for (left, env) in envs.iter().enumerate() {
        let mut candidates = local.envelope_hits(env);
        candidates.sort_unstable();
        for right in candidates {
            if right <= left {
                continue;
            }
            let o = &envs[right];
            let disjoint = env.min[0].max(o.min[0]) >= env.max[0].min(o.max[0])
                || env.min[1].max(o.min[1]) >= env.max[1].min(o.max[1]);
            if disjoint {
                continue;
            }
            if !geoms[left].relate_pattern(geoms[right], "T********")? {
                continue;
            }
            let inter = geoms[left].intersection(geoms[right])?;
            if inter.area()? > 0.0 {
                if !residue_without_sample(&inter, plan)? {
                    return Err(Error::unsupported(
                        "horizontal cells have overlapping interiors",
                    ));
                }
                tolerated += 1;
            }
        }
    }
    Ok(tolerated)
}
