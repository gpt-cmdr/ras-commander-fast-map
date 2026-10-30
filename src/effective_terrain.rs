//! RAS Mapper's effective terrain of a registered multi-source terrain,
//! without HEC-RAS (`rasmapper-effective-terrain/1`).
//!
//! Reproduces the float32 grid that `RasTerrain.export_rasmapper_terrain()`
//! obtains from RasMapperLib (HEC-RAS 7.0.1) for an unmodified terrain, from
//! the terrain's own source TIFFs, level-0 tile masks and stitch TIN. The rule
//! below was reverse-engineered from exports alone (no vendor code), and is
//! qualified pixel-for-pixel on BaldEagleCrkMulti2D Terrain50 (two sources,
//! 197,179,058 valid pixels) and five synthetic terrains built by HEC-RAS
//! 7.0.1 (one to three sources, coarse-over-fine, fine-over-coarse,
//! non-commensurate lattices, nodata holes).
//!
//! Per output pixel centre `p`:
//! 1. **Stitch TIN first.** If `p` lies in a stitch TIN triangle (inclusive,
//!    barycentric weights `>= -1e-9`, later triangles overwrite earlier ones),
//!    the value is the triangle's linear interpolation.
//! 2. Otherwise each source is tried in priority order. Its *quad* is the four
//!    pixel centres around `p`. A source answers when all four are valid and,
//!    once a higher-priority source has a valid pixel *containing* `p`, at
//!    least one of the four is in the source's footprint (bit 0 of its level-0
//!    terrain-HDF tile `Mask`). The value is the mean of the two diagonal
//!    triangulations of the quad (equivalently four triangles around the quad
//!    centre); the fractional offsets are rounded to binary32, the
//!    arithmetic is binary64.
//! 3. Otherwise nodata.
//!
//! Every value is rounded to binary32 and then to the nearest 1/32 (ties to
//! even), as RAS Mapper stores terrain values.
//!
//! The output lattice defaults to the export's: the finest source (ties by
//! priority), over the union of the source extents snapped outward.
//!
//! Terrain modifications (schema 2) are applied after the mosaic by
//! `terrain_mods`. Not reproduced: unqualified modification forms (refused), and
//! a number of pixels in stitch-TIN
//! corners where RAS Mapper itself is not stable (see `method_record`).
use crate::{
    MapError,
    model::{self, CrsIdentity, NodataPolicy, QueryConvention},
    terrain::NativeTerrain,
};
use rayon::prelude::*;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
    time::Instant,
};

pub const METHOD_ID: &str = "rasmapper-effective-terrain/2";
/// The unmodified-terrain contract, still accepted.
pub const METHOD_ID_V1: &str = "rasmapper-effective-terrain/1";
pub const SCHEMA: &str = "ras-commander-effective-terrain-request/2";
pub const SCHEMA_V1: &str = "ras-commander-effective-terrain-request/1";
/// Nodata of the written terrain, as in RAS Mapper's export.
pub const NODATA: f32 = -9999.0;
const TILE: usize = 256;
const TIN_INCLUSION: f64 = -1e-9;

/// A raw little-endian array file staged by the caller.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArrayFile {
    pub path: String,
    pub dtype: String,
    pub shape: Vec<usize>,
}

/// One registered source, in priority order (highest first).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    /// The source's float32 terrain TIFF as written by RAS Mapper.
    pub path: String,
    /// Level-0 tile mask (`Terrain/<source>/0/Mask`), `|u1` `[tiles, 65536]`,
    /// tiles row-major over the source raster.
    pub mask: ArrayFile,
    /// Caller-verified CRS identity of this source when its stored WKT text
    /// differs from the first source's but the caller has proven the CRSs
    /// equal. Default: the request's `crs_identity`.
    #[serde(default)]
    pub crs_identity: Option<CrsIdentity>,
}

/// `Terrain/Stitch TIN Points` (`<f8` `[n, 4]`: x, y, z, cell size) and
/// `Terrain/Stitch TIN Triangles` (`<i4` `[m, 3]`).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StitchTin {
    pub points: ArrayFile,
    pub triangles: ArrayFile,
}

/// Output lattice: north-up, square cells.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Grid {
    pub origin_x: f64,
    pub origin_y: f64,
    pub cell_size: f64,
    pub width: usize,
    pub height: usize,
}

impl Grid {
    pub fn transform(&self) -> [f64; 6] {
        [
            self.origin_x,
            self.cell_size,
            0.0,
            self.origin_y,
            0.0,
            -self.cell_size,
        ]
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub schema: String,
    pub sources: Vec<Source>,
    #[serde(default)]
    pub stitch_tin: Option<StitchTin>,
    pub epsg: u32,
    pub crs_identity: CrsIdentity,
    /// Explicit lattice. Default: the finest source's lattice over `extent`.
    #[serde(default)]
    pub grid: Option<Grid>,
    /// `[xmin, ymin, xmax, ymax]`, snapped outward to the lattice. Default:
    /// the union of the source extents.
    #[serde(default)]
    pub extent: Option<[f64; 4]>,
    /// Number of modification layers in the terrain HDF. Schema 1: must be 0.
    /// Schema 2: must equal `modification_layers.len()`, so a caller cannot
    /// drop a layer silently.
    pub modifications: usize,
    /// Schema 2: every modification layer of the terrain (see `terrain_mods`).
    #[serde(default)]
    pub modification_layers: Vec<crate::terrain_mods::Layer>,
    pub threads: usize,
    pub max_working_bytes: u64,
}

pub fn method_record() -> Value {
    json!({
        "method_id": METHOD_ID,
        "compatible_method_ids": [METHOD_ID_V1],
        "request_schema": SCHEMA,
        "request_schemas": [SCHEMA_V1, SCHEMA],
        "reference": "RasTerrain.export_rasmapper_terrain(downsample_factor=1, rasterize_modifications=False), HEC-RAS 7.0.1; with modification_layers, rasterize_modifications=True",
        "modifications": crate::terrain_mods::method_record(),
        "lattice": "finest source (ties: priority), union of source extents snapped outward; or a supplied grid",
        "order": "stitch TIN first; then sources in priority order",
        "source_test": "all four quad centres valid; after a higher-priority source has a valid pixel containing the point, at least one quad centre must be in the source footprint (mask bit 0 and valid)",
        "interpolation": "mean of the two diagonal triangulations of the quad; offsets rounded to binary32, binary64 arithmetic",
        "stitch_tin": "linear in the containing triangle, weights >= -1e-9, later triangles win",
        "rounding": "binary32, then nearest 1/32 with ties to even",
        "nodata": NODATA,
        "qualified": {
            "BaldEagleCrkMulti2D Terrain50 (2 sources, 20 ft + 36.5 ft)": "197,179,058 valid px; valid mask identical; 8 values differ, all in stitch-TIN corners",
            "synthetic HEC-RAS 7.0.1 terrains (1-3 sources)": "4 of 5 bit-identical; 5 stitch-TIN corner px differ on the fifth (687,908 valid)"
        },
        "known_differences": "a few pixels where a stitch TIN triangle meets a nodata corner; RAS Mapper's own exports of one terrain disagree there (2 px of T2 change when an unrelated modification is rasterized)",
        "refused": "unqualified modification types (see modifications.refused), non-float32/rotated/non-square sources, sources in different CRSs"
    })
}

fn invalid(msg: impl Into<String>) -> MapError {
    MapError::Invalid(msg.into())
}

impl Request {
    pub fn validate(&self) -> Result<(), MapError> {
        if self.schema == SCHEMA_V1 {
            if self.modifications != 0 || !self.modification_layers.is_empty() {
                return Err(MapError::Unsupported(
                    "terrain modifications need request schema ras-commander-effective-terrain-request/2".into(),
                ));
            }
        } else if self.schema == SCHEMA {
            if self.modifications != self.modification_layers.len() {
                return Err(invalid(format!(
                    "modifications ({}) must equal the number of modification_layers ({})",
                    self.modifications,
                    self.modification_layers.len()
                )));
            }
            crate::terrain_mods::validate(&self.modification_layers)?;
        } else {
            return Err(invalid(format!("schema must be {SCHEMA} or {SCHEMA_V1}")));
        }
        if self.sources.is_empty() {
            return Err(invalid("at least one source is required"));
        }
        if self.threads == 0 || self.max_working_bytes == 0 {
            return Err(invalid("threads and max_working_bytes must be positive"));
        }
        for s in &self.sources {
            if s.mask.dtype != "|u1" || s.mask.shape.len() != 2 || s.mask.shape[1] != TILE * TILE {
                return Err(invalid("source masks must be |u1 [tiles, 65536]"));
            }
        }
        if let Some(t) = &self.stitch_tin
            && (t.points.dtype != "<f8"
                || t.points.shape.len() != 2
                || t.points.shape[1] != 4
                || t.triangles.dtype != "<i4"
                || t.triangles.shape.len() != 2
                || t.triangles.shape[1] != 3)
        {
            return Err(invalid(
                "stitch_tin needs <f8 [n, 4] points and <i4 [m, 3] triangles",
            ));
        }
        if let Some(g) = &self.grid
            && (!(g.cell_size.is_finite() && g.cell_size > 0.0)
                || !g.origin_x.is_finite()
                || !g.origin_y.is_finite()
                || g.width == 0
                || g.height == 0)
        {
            return Err(invalid(
                "grid needs a finite origin, positive cell size and size",
            ));
        }
        if let Some(e) = &self.extent
            && (!e.iter().all(|v| v.is_finite()) || e[2] <= e[0] || e[3] <= e[1])
        {
            return Err(invalid("extent must be finite [xmin, ymin, xmax, ymax]"));
        }
        Ok(())
    }
}

fn near_integer(v: f64) -> f64 {
    let r = v.round();
    if (v - r).abs() <= 1e-10_f64.max(1e-12 * r.abs()) {
        r
    } else {
        v
    }
}

/// Snap `extent` outward to the lattice through (`origin_x`, `origin_y`).
pub fn snap_grid(extent: [f64; 4], origin_x: f64, origin_y: f64, cell: f64) -> Option<Grid> {
    let left = near_integer((extent[0] - origin_x) / cell).floor();
    let right = near_integer((extent[2] - origin_x) / cell).ceil();
    let top = near_integer((origin_y - extent[3]) / cell).floor();
    let bottom = near_integer((origin_y - extent[1]) / cell).ceil();
    let (w, h) = (right - left, bottom - top);
    if !(w >= 1.0 && h >= 1.0 && w < u32::MAX as f64 && h < u32::MAX as f64) {
        return None;
    }
    Some(Grid {
        origin_x: origin_x + left * cell,
        origin_y: origin_y - top * cell,
        cell_size: cell,
        width: w as usize,
        height: h as usize,
    })
}

fn read_exact_len(path: &Path, expected: u64) -> Result<Vec<u8>, MapError> {
    let len = std::fs::metadata(path)?.len();
    if len != expected {
        return Err(invalid(format!(
            "{}: {len} bytes, expected {expected}",
            path.display()
        )));
    }
    let mut buf = Vec::with_capacity(expected as usize);
    File::open(path)?.read_to_end(&mut buf)?;
    Ok(buf)
}

struct Tin {
    points: Vec<[f64; 4]>,
    triangles: Vec<[u32; 3]>,
}

fn load_tin(base: &Path, t: &StitchTin) -> Result<Tin, MapError> {
    let (n, m) = (t.points.shape[0], t.triangles.shape[0]);
    let pb = read_exact_len(&model::resolve(base, &t.points.path), (n * 32) as u64)?;
    let tb = read_exact_len(&model::resolve(base, &t.triangles.path), (m * 12) as u64)?;
    let points: Vec<[f64; 4]> = pb
        .as_chunks::<32>()
        .0
        .iter()
        .map(|p| {
            let (q, _) = p.as_chunks::<8>();
            [0, 1, 2, 3].map(|k| f64::from_le_bytes(q[k]))
        })
        .collect();
    let mut triangles = Vec::with_capacity(m);
    for c in tb.as_chunks::<12>().0 {
        let mut tri = [0_u32; 3];
        for (v, b) in tri.iter_mut().zip(c.as_chunks::<4>().0) {
            let i = i32::from_le_bytes(*b);
            if i < 0 || i as usize >= n {
                return Err(invalid(format!("stitch TIN index {i} outside {n} points")));
            }
            *v = i as u32;
        }
        triangles.push(tri);
    }
    if points
        .iter()
        .any(|p| !(p[0].is_finite() && p[1].is_finite() && p[2].is_finite()))
    {
        return Err(invalid("stitch TIN points must be finite"));
    }
    Ok(Tin { points, triangles })
}

/// RAS Mapper's terrain value rounding: binary32, then 1/32 with ties to even.
#[inline]
pub fn quantize(v: f64) -> f32 {
    (((v as f32) as f64 * 32.0).round_ties_even() / 32.0) as f32
}

/// Rasterize the stitch TIN into `out`; returns the per-pixel hit bitmap.
fn rasterize_tin(tin: &Tin, grid: &Grid, out: &mut [f32]) -> (Vec<u64>, u64) {
    let (w, h) = (grid.width, grid.height);
    let (ox, oy, cs) = (grid.origin_x, grid.origin_y, grid.cell_size);
    let mut hit = vec![0_u64; (w * h).div_ceil(64)];
    let mut count = 0_u64;
    for t in &tin.triangles {
        let [p0, p1, p2] = t.map(|i| tin.points[i as usize]);
        let (x0, y0, z0) = (p0[0], p0[1], p0[2]);
        let (x1, y1, z1) = (p1[0], p1[1], p1[2]);
        let (x2, y2, z2) = (p2[0], p2[1], p2[2]);
        let det = (y1 - y2) * (x0 - x2) + (x2 - x1) * (y0 - y2);
        if det == 0.0 {
            continue;
        }
        let (xmin, xmax) = (x0.min(x1).min(x2), x0.max(x1).max(x2));
        let (ymin, ymax) = (y0.min(y1).min(y2), y0.max(y1).max(y2));
        let c0 = ((xmin - ox) / cs - 0.5).floor().max(0.0);
        let c1 = ((xmax - ox) / cs - 0.5).ceil().min(w as f64 - 1.0);
        let r0 = ((oy - ymax) / cs - 0.5).floor().max(0.0);
        let r1 = ((oy - ymin) / cs - 0.5).ceil().min(h as f64 - 1.0);
        if c1 < c0 || r1 < r0 {
            continue;
        }
        for r in r0 as usize..=r1 as usize {
            let py = oy - (r as f64 + 0.5) * cs;
            for c in c0 as usize..=c1 as usize {
                let px = ox + (c as f64 + 0.5) * cs;
                let l0 = ((y1 - y2) * (px - x2) + (x2 - x1) * (py - y2)) / det;
                let l1 = ((y2 - y0) * (px - x2) + (x0 - x2) * (py - y2)) / det;
                let l2 = 1.0 - l0 - l1;
                if l0 >= TIN_INCLUSION && l1 >= TIN_INCLUSION && l2 >= TIN_INCLUSION {
                    let i = r * w + c;
                    out[i] = quantize(l0 * z0 + l1 * z1 + l2 * z2);
                    if hit[i / 64] & (1 << (i % 64)) == 0 {
                        hit[i / 64] |= 1 << (i % 64);
                        count += 1;
                    }
                }
            }
        }
    }
    (hit, count)
}

/// One source with its footprint mask file.
struct SourceData {
    terrain: NativeTerrain,
    mask_path: std::path::PathBuf,
    tiles_x: usize,
}

/// A row band of one source: values (NaN = invalid) and footprint flags.
struct Band {
    row0: usize,
    rows: usize,
    width: usize,
    values: Vec<f32>,
    footprint: Vec<bool>,
}

impl SourceData {
    /// Rows `[row0, row0 + rows)` with validity folded into NaN.
    fn band(&self, row0: usize, rows: usize) -> Result<Band, MapError> {
        let t = &self.terrain;
        let width = t.width;
        let mut values = vec![f32::NAN; width * rows];
        let (bw, bh) = t.block;
        for by in row0 / bh..=(row0 + rows - 1) / bh {
            for bx in 0..t.blocks().0 {
                let win = t.block_window(bx, by);
                let block = t.get_block(bx, by)?;
                for r in row0.max(win.y)..(row0 + rows).min(win.y + win.height) {
                    let src = &block[(r - win.y) * win.width..(r - win.y + 1) * win.width];
                    let dst = &mut values[(r - row0) * width + bx * bw..][..win.width];
                    for (d, s) in dst.iter_mut().zip(src) {
                        *d = match t.classify(*s) {
                            Some(v) if v.is_finite() => *s,
                            _ => f32::NAN,
                        };
                    }
                }
            }
        }
        let mut footprint = vec![false; width * rows];
        let mut file = File::open(&self.mask_path)?;
        let mut tile_row = vec![0_u8; self.tiles_x * TILE * TILE];
        for ty in row0 / TILE..=(row0 + rows - 1) / TILE {
            file.seek(SeekFrom::Start((ty * self.tiles_x * TILE * TILE) as u64))?;
            file.read_exact(&mut tile_row)?;
            for r in row0.max(ty * TILE)..(row0 + rows).min(ty * TILE + TILE) {
                let lr = r - ty * TILE;
                for c in 0..width {
                    let m = tile_row[(c / TILE) * TILE * TILE + lr * TILE + c % TILE];
                    let i = (r - row0) * width + c;
                    footprint[i] = m & 1 != 0 && !values[i].is_nan();
                }
            }
        }
        Ok(Band {
            row0,
            rows,
            width,
            values,
            footprint,
        })
    }

    /// Source rows needed by output rows `[r0, r1)` (containing pixel and quad).
    fn rows_for(&self, grid: &Grid, r0: usize, r1: usize) -> Option<(usize, usize)> {
        let gt = self.terrain.gt;
        let gy = |r: usize| (gt[3] - (grid.origin_y - (r as f64 + 0.5) * grid.cell_size)) / -gt[5];
        let lo = (gy(r0) - 0.5).floor() - 1.0;
        let hi = gy(r1 - 1).floor() + 2.0;
        let h = self.terrain.height as f64;
        let (lo, hi) = (lo.max(0.0), hi.min(h - 1.0));
        (hi >= lo).then_some((lo as usize, (hi - lo) as usize + 1))
    }
}

#[derive(Clone, Copy)]
struct Sample {
    containing_valid: bool,
    quad_valid: bool,
    quad_footprint: bool,
    value: f64,
}

#[inline]
fn sample(band: &Band, gt: &[f64; 6], height: usize, x: f64, y: f64) -> Sample {
    let mut s = Sample {
        containing_valid: false,
        quad_valid: false,
        quad_footprint: false,
        value: 0.0,
    };
    let gx = (x - gt[0]) / gt[1];
    let gy = (gt[3] - y) / -gt[5];
    let w = band.width;
    let at = |row: f64, col: f64| -> Option<usize> {
        let (r, c) = (row as isize - band.row0 as isize, col as isize);
        (r >= 0 && (r as usize) < band.rows && c >= 0 && (c as usize) < w)
            .then(|| r as usize * w + c as usize)
    };
    let (nx, ny) = (gx.floor(), gy.floor());
    if nx >= 0.0
        && ny >= 0.0
        && (nx as usize) < w
        && (ny as usize) < height
        && let Some(i) = at(ny, nx)
    {
        s.containing_valid = !band.values[i].is_nan();
    }
    let (fx, fy) = (gx - 0.5, gy - 0.5);
    let (i0, j0) = (fx.floor(), fy.floor());
    if !(i0 >= 0.0 && j0 >= 0.0 && (i0 as usize) + 1 < w && (j0 as usize) + 1 < height) {
        return s;
    }
    let (Some(a), Some(c)) = (at(j0, i0), at(j0 + 1.0, i0)) else {
        return s;
    };
    let (v00, v01, v10, v11) = (
        band.values[a],
        band.values[a + 1],
        band.values[c],
        band.values[c + 1],
    );
    if v00.is_nan() || v01.is_nan() || v10.is_nan() || v11.is_nan() {
        return s;
    }
    s.quad_valid = true;
    s.quad_footprint =
        band.footprint[a] || band.footprint[a + 1] || band.footprint[c] || band.footprint[c + 1];
    let (v00, v01, v10, v11) = (v00 as f64, v01 as f64, v10 as f64, v11 as f64);
    let tx = ((fx - i0) as f32) as f64;
    let ty = ((fy - j0) as f32) as f64;
    let t0 = if tx >= ty {
        v00 + tx * (v01 - v00) + ty * (v11 - v01)
    } else {
        v00 + ty * (v10 - v00) + tx * (v11 - v10)
    };
    let t1 = if tx + ty <= 1.0 {
        v00 + tx * (v01 - v00) + ty * (v10 - v00)
    } else {
        v11 + (1.0 - tx) * (v10 - v11) + (1.0 - ty) * (v01 - v11)
    };
    s.value = (t0 + t1) / 2.0;
    s
}

/// The computed effective terrain and its provenance.
pub struct Effective {
    pub grid: Grid,
    pub values: Vec<f32>,
    pub report: Value,
}

/// Compute the effective terrain in memory.
pub fn compute(req: &Request, base: &Path) -> Result<Effective, MapError> {
    req.validate()?;
    let started = Instant::now();
    let cache = (req.max_working_bytes / 4 / req.sources.len() as u64).max(1 << 20);
    let mut sources = Vec::with_capacity(req.sources.len());
    for s in &req.sources {
        let path = model::resolve(base, &s.path);
        let terrain = NativeTerrain::open(
            path.to_str()
                .ok_or_else(|| invalid("source path is not UTF-8"))?,
            req.epsg,
            s.crs_identity.as_ref().unwrap_or(&req.crs_identity),
            QueryConvention::PixelCenter,
            NodataPolicy::Dry,
            cache,
        )?;
        if terrain.gt[1] != -terrain.gt[5] {
            return Err(MapError::Unsupported(format!(
                "{}: non-square cells",
                terrain.path
            )));
        }
        let (tiles_x, tiles_y) = (terrain.width.div_ceil(TILE), terrain.height.div_ceil(TILE));
        if s.mask.shape[0] != tiles_x * tiles_y {
            return Err(invalid(format!(
                "{}: mask has {} tiles, source needs {}",
                terrain.path,
                s.mask.shape[0],
                tiles_x * tiles_y
            )));
        }
        let mask_path = model::resolve(base, &s.mask.path);
        if std::fs::metadata(&mask_path)?.len() != (s.mask.shape[0] * TILE * TILE) as u64 {
            return Err(invalid("source mask length mismatch"));
        }
        sources.push(SourceData {
            terrain,
            mask_path,
            tiles_x,
        });
    }
    let grid = match req.grid {
        Some(g) => g,
        None => {
            // Finest source; the first (highest priority) wins ties.
            let auth = sources
                .iter()
                .min_by(|a, b| a.terrain.gt[1].total_cmp(&b.terrain.gt[1]))
                .expect("sources");
            let extent = req.extent.unwrap_or_else(|| {
                sources.iter().fold(
                    [
                        f64::INFINITY,
                        f64::INFINITY,
                        f64::NEG_INFINITY,
                        f64::NEG_INFINITY,
                    ],
                    |e, s| {
                        let g = s.terrain.gt;
                        let (w, h) = (s.terrain.width as f64, s.terrain.height as f64);
                        [
                            e[0].min(g[0]),
                            e[1].min(g[3] + h * g[5]),
                            e[2].max(g[0] + w * g[1]),
                            e[3].max(g[3]),
                        ]
                    },
                )
            });
            let g = auth.terrain.gt;
            snap_grid(extent, g[0], g[3], g[1])
                .ok_or_else(|| invalid("snapped grid is empty or too large"))?
        }
    };
    // Output values plus the stitch-TIN bitmap; source bands and the block
    // cache (max_working_bytes / 4 across sources) come on top.
    let out_bytes = (grid.width as u64)
        .checked_mul(grid.height as u64)
        .and_then(|n| n.checked_mul(4).map(|b| b + n / 8 + 8))
        .ok_or_else(|| MapError::Budget("output size overflows".into()))?;
    if out_bytes > req.max_working_bytes {
        return Err(MapError::Budget(format!(
            "output grid {}x{} needs {out_bytes} bytes over max_working_bytes {}",
            grid.width, grid.height, req.max_working_bytes
        )));
    }
    let mut values = vec![NODATA; grid.width * grid.height];
    let tin_started = Instant::now();
    let (hit, tin_pixels, tin_triangles) = match &req.stitch_tin {
        Some(t) => {
            let tin = load_tin(base, t)?;
            let (hit, n) = rasterize_tin(&tin, &grid, &mut values);
            (hit, n, tin.triangles.len())
        }
        None => (vec![0_u64; (grid.width * grid.height).div_ceil(64)], 0, 0),
    };
    let tin_seconds = tin_started.elapsed().as_secs_f64();

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(req.threads)
        .build()
        .map_err(|e| MapError::Internal(format!("thread pool: {e}")))?;
    const STRIP: usize = 64;
    let width = grid.width;
    let counts: Vec<Result<Vec<u64>, MapError>> = pool.install(|| {
        values
            .par_chunks_mut(STRIP * width)
            .enumerate()
            .map(|(strip, out)| {
                let r0 = strip * STRIP;
                let r1 = r0 + out.len() / width;
                let mut bands = Vec::with_capacity(sources.len());
                for s in &sources {
                    bands.push(match s.rows_for(&grid, r0, r1) {
                        Some((row0, rows)) => Some(s.band(row0, rows)?),
                        None => None,
                    });
                }
                let mut counts = vec![0_u64; sources.len()];
                for r in r0..r1 {
                    let y = grid.origin_y - (r as f64 + 0.5) * grid.cell_size;
                    for c in 0..width {
                        let i = r * width + c;
                        if hit[i / 64] & (1 << (i % 64)) != 0 {
                            continue;
                        }
                        let x = grid.origin_x + (c as f64 + 0.5) * grid.cell_size;
                        let mut covered = false;
                        for (k, (s, band)) in sources.iter().zip(&bands).enumerate() {
                            let Some(band) = band else { continue };
                            let sm = sample(band, &s.terrain.gt, s.terrain.height, x, y);
                            if sm.quad_valid && (!covered || sm.quad_footprint) {
                                out[i - r0 * width] = quantize(sm.value);
                                counts[k] += 1;
                                break;
                            }
                            covered |= sm.containing_valid;
                        }
                    }
                }
                Ok(counts)
            })
            .collect()
    });
    let mut per_source = vec![0_u64; sources.len()];
    for c in counts {
        for (t, v) in per_source.iter_mut().zip(c?) {
            *t += v;
        }
    }
    let mods_started = Instant::now();
    let mods = crate::terrain_mods::apply(&req.modification_layers, &grid, &mut values)?;
    let mods_seconds = mods_started.elapsed().as_secs_f64();
    let valid = values.iter().filter(|v| **v != NODATA).count() as u64;
    let report = json!({
        "method_id": METHOD_ID,
        "grid": {"origin_x": grid.origin_x, "origin_y": grid.origin_y, "cell_size": grid.cell_size,
                  "width": grid.width, "height": grid.height},
        "sources": sources.iter().zip(&per_source).map(|(s, n)| json!({
            "path": s.terrain.path, "geo_transform": s.terrain.gt,
            "width": s.terrain.width, "height": s.terrain.height, "pixels": n,
            "cache": s.terrain.stats().cache,
        })).collect::<Vec<_>>(),
        "stitch_tin": {"triangles": tin_triangles, "pixels": tin_pixels, "seconds": tin_seconds},
        "modifications": {"layers": mods, "seconds": mods_seconds},
        "valid_pixels": valid,
        "seconds": started.elapsed().as_secs_f64(),
    });
    Ok(Effective {
        grid,
        values,
        report,
    })
}

/// Write `effective` as a tiled float32 GeoTIFF with nodata -9999.
pub fn write_geotiff(path: &Path, effective: Effective, epsg: u32) -> Result<(), MapError> {
    use wbraster::{DataType, Raster, crs_info::CrsInfo, raster::RasterData};
    let g = effective.grid;
    let transform = g.transform();
    let raster = Raster {
        cols: g.width,
        rows: g.height,
        bands: 1,
        x_min: transform[0],
        y_min: transform[3] + transform[5] * g.height as f64,
        cell_size_x: transform[1],
        cell_size_y: -transform[5],
        nodata: NODATA as f64,
        data_type: DataType::F32,
        crs: CrsInfo::from_epsg(epsg),
        metadata: Vec::new(),
        data: RasterData::F32(effective.values),
    };
    let dataset =
        terrio::RasterDataset::with_driver(raster, terrio::formats::RasterDriver::GTiff, true);
    let options = terrio::RasterWriteOptions {
        compression: Some("DEFLATE".into()),
        geo_transform: Some(transform),
        creation_options: vec![
            ("TILED".into(), "YES".into()),
            ("BLOCKXSIZE".into(), "256".into()),
            ("BLOCKYSIZE".into(), "256".into()),
        ],
        ..Default::default()
    };
    terrio::write_raster_dataset(
        &dataset,
        path.to_str()
            .ok_or_else(|| invalid("output path is not UTF-8"))?,
        &options,
    )
    .map_err(|e| invalid(format!("effective terrain GeoTIFF write: {e}")))
}

/// CLI entry: compute and write `effective_terrain.tif` and `report.json`.
pub fn run(req: &Request, base: &Path, out: &Path) -> Result<Value, MapError> {
    let effective = compute(req, base)?;
    std::fs::create_dir_all(out)?;
    let mut report = effective.report.clone();
    let tif = out.join("effective_terrain.tif");
    let partial = out.join("effective_terrain.partial.tif");
    let write_started = Instant::now();
    write_geotiff(&partial, effective, req.epsg)?;
    std::fs::rename(&partial, &tif)?;
    report["status"] = "complete".into();
    report["output"] = tif.display().to_string().into();
    report["write_seconds"] = write_started.elapsed().as_secs_f64().into();
    crate::sink::write_json(&out.join("report.json"), &report)?;
    Ok(report)
}
