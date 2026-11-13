//! RAS Mapper's effective terrain of a registered multi-source terrain,
//! without HEC-RAS (`rasmapper-effective-terrain/1`).
//!
//! Reproduces the float32 grid that `RasTerrain.export_rasmapper_terrain()`
//! obtains from RasMapperLib (HEC-RAS 7.0.1) for an unmodified terrain, from
//! the terrain's own source TIFFs and level-0 tile masks. The rule below
//! describes that exported output; its qualification on real HEC-RAS 7.0.1
//! exports is recorded in `method_record`.
//!
//! The rule follows RasMapperLib's export path as established by an
//! interoperability study of HEC-RAS 7.0.1 (prose findings only; no vendor
//! code is used or copied):
//!
//! 1. **Output lattice.** The export is written in 256 x 256 tiles. The tile
//!    pitch and each tile's pixel size are recomputed from tile extents, and
//!    pixel centres are `tile_min + pixel * (i + 0.5)` within each tile.
//!    The default cell size is the finest source's extent-derived cell size
//!    (extent width / columns or height / rows, whichever is larger), not
//!    its GeoTIFF cell size.
//! 2. **Render blocks.** A source is sampled by render block: the quad
//!    between four adjacent source cell centres, grouped in 256 x 256 block
//!    tiles that start half a cell inside the source extent. The block and
//!    its binary32 fractions follow the same recomputed tile geometry; a
//!    pixel centre that coincides with a source cell centre is assigned by
//!    that arithmetic, not by a tolerance.
//! 3. **Validity.** A block answers only when bit 0 of its level-0 terrain-HDF
//!    tile `Mask` byte is set (all four corners valid and the block owned by
//!    this source; terrain creation clears blocks under higher-priority
//!    sources). Sources are tried in priority order; the first to answer a
//!    pixel wins.
//! 4. **Value.** The block is split along both diagonals into four
//!    triangles. Arithmetic is binary64 on the binary32 corner values and
//!    fractions, with the block-centre term held in binary32.
//! 5. **Export TIN (gap fill).** After sampling, every 256-pixel output tile
//!    triangulates the edges of valid render blocks that border off blocks
//!    (all sources, within a two-block halo), deletes triangles on the data
//!    side and triangles wider or taller than 2.0001 cells, and rasterizes
//!    the rest over the tile, overwriting sampled values (`export_tin`).
//!    One-cell gaps between valid blocks and source seams are bridged. The
//!    terrain HDF's stored `Stitch TIN` is not used: RAS Mapper's export
//!    does not use it either. The request's `stitch_tin` is still accepted
//!    and validated for schema compatibility, and ignored.
//!
//! Values are held in binary32 through the export TIN and the terrain
//! modifications, and rounded to the nearest 1/32 (ties to even) once at the
//! end, as RAS Mapper's TIFF writer does.
//!
//! Terrain modifications (schema 2) are applied after the mosaic by
//! `terrain_mods`; unqualified modification forms are refused.
use crate::{
    MapError,
    export_tin::{self, BlockLattice, BlockSource, TileGrid},
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
/// The earlier unmodified-terrain rule. Schema 1 requests now run the /2
/// rule and report /2.
pub const METHOD_ID_V1: &str = "rasmapper-effective-terrain/1";
pub const SCHEMA: &str = "ras-commander-effective-terrain-request/2";
pub const SCHEMA_V1: &str = "ras-commander-effective-terrain-request/1";
/// Nodata of the written terrain, as in RAS Mapper's export.
pub const NODATA: f32 = -9999.0;
const TILE: usize = 256;

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
/// `Terrain/Stitch TIN Triangles` (`<i4` `[m, 3]`). Accepted for request
/// compatibility; the rule does not use it (see the module docs, item 5).
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
    /// EPSG code of the sources and the output. 0 or absent with `crs_geokeys`.
    #[serde(default)]
    pub epsg: u32,
    /// The output CRS as verbatim GeoKey tags, for a CRS without an EPSG code
    /// (see [`crate::crs`]). Sources are identified by `crs_identity`.
    #[serde(default)]
    pub crs_geokeys: Option<crate::crs::GeoKeys>,
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
    /// HEC-RAS release whose modification rules to follow (omitted: the
    /// 7.0.1 rule). One of the labels in
    /// [`crate::terrain_mods::HECRAS_VERSIONS`]; any other label is refused
    /// before any read.
    #[serde(default)]
    pub hecras_version: Option<String>,
    pub threads: usize,
    pub max_working_bytes: u64,
}

pub fn method_record() -> Value {
    let mut record = json!({
        "method_id": METHOD_ID,
        "request_schema": SCHEMA,
        "request_schemas": [SCHEMA_V1, SCHEMA],
        "reference": "RasTerrain.export_rasmapper_terrain(downsample_factor=1, rasterize_modifications=False) for schema 1; rasterize_modifications=True with modification_layers (schema 2); HEC-RAS 7.0.1, or another release named by hecras_version (see hecras_versions)",
        "hecras_versions": crate::terrain_mods::hecras_versions_record(),
        "receipt_method_id": "rasmapper-effective-terrain/2 for both request schemas: schema 1 (no modifications) is still accepted, but its values follow the /2 rule, which differs numerically from /1 engines",
        "modifications": crate::terrain_mods::method_record(),
        "lattice": "finest source by extent-derived cell size (ties: priority), union of source extents snapped outward; or a supplied grid. Pixel centres follow the export's 256-pixel tiles with recomputed tile pitch and pixel size",
        "order": "sources in priority order, first to answer wins; then the per-tile export TIN overwrites the pixels it covers",
        "source_test": "render block (quad between four source cell centres, 256-block tiles half a cell inside the source extent) located with recomputed tile geometry; the block answers when bit 0 of its level-0 mask byte is set",
        "interpolation": "four triangles on both block diagonals; binary32 fractions and corners, binary64 arithmetic, block-centre term held in binary32",
        "export_tin": "per 256-pixel output tile: edges of valid render blocks with off neighbours (mask bits 1-4) from every source the tile visits (two-block halo), each source's segments stable-sorted by hash of A.X (type id 7) with equal-hash order modelled as 8 range partitions per block tile; constrained Delaunay TIN (merge tolerance min(0.01, 0.1 x smallest cell)); triangles touching the super polygon, wider or taller than 2.0001 x largest cell, or on the data side of a segment are deleted; the rest rasterized over the tile (inclusive edge functions, binary64 stepping, later triangles win), then rounded like every value. One-cell gaps and source seams are bridged",
        "stitch_tin": "the terrain HDF's stored stitch TIN is accepted in the request for compatibility and not used (RAS Mapper's export does not use it)",
        "rounding": "binary32, then nearest 1/32 with ties to even",
        "nodata": NODATA,
        "qualified": {
            "real terrains B1-B4 (HEC-RAS 7.0.1, 1-2 sources, 254.7M valid px each, modifications off, whole terrain vs export a)": "B3 bit-identical; B4 1 pixel nodata where the export is valid; B1 and B2 1 such pixel plus 2 pixels valid where the export is nodata; 0 value differences on all four",
            "real terrain A (HEC-RAS 7.0.1, 5 sources, 164.3M valid px, modifications off)": "0 mask differences; 354 value differences vs export a and 406 vs export b (the two exports differ on 160 px); all inside export-TIN gap fills",
            "synthetic HEC-RAS 7.0.1 terrains T1-T5 (1-3 sources, one export each)": "valid mask identical on all five; values: T1 identical, T2 and T4 2 px, T3 and T5 4 px (max 1.53 ft), all in export-TIN gap fills",
            "BaldEagleCrkMulti2D Terrain50 (2 sources, 20 ft + 36.5 ft)": "not re-run since the render-block rule"
        },
        "known_differences": "inside export-TIN gap fills: the order of segments with equal hash comes from RAS Mapper's parallel collection and its hash seed varies by process, which decides cocircular Delaunay ties; this engine fixes one order (type id 7, 8 range partitions). On terrain A another seed order (type id 23) leaves 82 value differences in an independent prototype, consistent with the remaining differences being ordering ties. RAS Mapper's own repeated exports also disagree on some of these pixels",
        "refused": "modification forms listed in modifications.refused, non-float32/rotated/non-square sources, sources in different CRSs"
    });
    crate::crs::advertise(&mut record);
    record
}

impl Request {
    /// The output CRS: `epsg`, or `crs_geokeys` (validated).
    pub fn output_crs(&self) -> Result<crate::crs::OutputCrs, MapError> {
        crate::crs::OutputCrs::resolve(self.epsg, self.crs_geokeys.as_ref())
    }
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
        // An unqualified release is refused under either schema, before any read.
        crate::terrain_mods::RasVersion::parse(self.hecras_version.as_deref())?;
        if self.sources.is_empty() {
            return Err(invalid("at least one source is required"));
        }
        // Validates crs_geokeys. `epsg: 0` without keys keeps the 0.19.0
        // behaviour (same output bytes, labelled ProjectedCSTypeGeoKey 0, which
        // GDAL reads as a local CRS); `run` reports it as `crs_warning`.
        self.output_crs()?;
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

/// RAS Mapper's terrain value rounding: binary32, then 1/32 with ties to even.
#[inline]
pub fn quantize(v: f64) -> f32 {
    (((v as f32) as f64 * 32.0).round_ties_even() / 32.0) as f32
}

/// One source with its render-block mask file and lattice.
struct SourceData {
    terrain: NativeTerrain,
    mask_path: std::path::PathBuf,
    tiles_x: usize,
    lattice: SourceLattice,
    /// Per block tile: some valid block has an off neighbour (export TIN).
    edges: Vec<bool>,
}

impl SourceData {
    /// Which block tiles have valid blocks with off neighbours, one block-tile
    /// row (`tiles_x` x 64 KiB) per task.
    fn scan_edges(&self) -> Result<Vec<bool>, MapError> {
        let tiles_y = self.terrain.height.div_ceil(TILE);
        let rows: Vec<Result<Vec<bool>, MapError>> = (0..tiles_y)
            .into_par_iter()
            .map(|ty| {
                let mut file = File::open(&self.mask_path)?;
                let mut buf = vec![0_u8; self.tiles_x * TILE * TILE];
                file.seek(SeekFrom::Start((ty * buf.len()) as u64))?;
                file.read_exact(&mut buf)?;
                Ok(buf
                    .as_chunks::<{ TILE * TILE }>()
                    .0
                    .iter()
                    .map(|t| {
                        t.iter().any(|b| {
                            b & export_tin::BLOCK_VALID != 0 && b & export_tin::BLOCK_EDGES != 0
                        })
                    })
                    .collect())
            })
            .collect();
        let mut out = Vec::with_capacity(tiles_y * self.tiles_x);
        for r in rows {
            out.extend(r?);
        }
        Ok(out)
    }
}

impl BlockSource for SourceData {
    fn extent(&self) -> [f64; 4] {
        let g = &self.terrain.gt;
        let (w, h) = (self.terrain.width as f64, self.terrain.height as f64);
        [g[0], g[0] + w * g[1], g[3] + h * g[5], g[3]]
    }

    fn lattice(&self) -> BlockLattice {
        let l = &self.lattice;
        BlockLattice {
            min_x: l.min_x,
            max_y: l.max_y,
            pitch: l.pitch,
            tiles_x: l.tiles_x,
            tiles_y: l.tiles_y,
        }
    }

    fn tile_has_edges(&self, tile: usize) -> bool {
        self.edges.get(tile).copied().unwrap_or(false)
    }

    fn mask_rows(&self, tile: usize, row0: usize, rows: usize) -> Result<Vec<u8>, MapError> {
        let mut file = File::open(&self.mask_path)?;
        file.seek(SeekFrom::Start((tile * TILE * TILE + row0 * TILE) as u64))?;
        let mut buf = vec![0_u8; rows * TILE];
        file.read_exact(&mut buf)?;
        Ok(buf)
    }

    fn values(
        &self,
        row0: usize,
        rows: usize,
        col0: usize,
        cols: usize,
    ) -> Result<Vec<f32>, MapError> {
        let t = &self.terrain;
        let mut out = vec![f32::NAN; rows * cols];
        let (r1, c1) = ((row0 + rows).min(t.height), (col0 + cols).min(t.width));
        if row0 >= r1 || col0 >= c1 {
            return Ok(out);
        }
        let (bw, bh) = t.block;
        for by in row0 / bh..=(r1 - 1) / bh {
            for bx in col0 / bw..=(c1 - 1) / bw {
                let win = t.block_window(bx, by);
                let block = t.get_block(bx, by)?;
                let (xa, xb) = (col0.max(win.x), c1.min(win.x + win.width));
                for r in row0.max(win.y)..r1.min(win.y + win.height) {
                    let src = &block[(r - win.y) * win.width..][..win.width];
                    out[(r - row0) * cols + xa - col0..][..xb - xa]
                        .copy_from_slice(&src[xa - win.x..xb - win.x]);
                }
            }
        }
        Ok(out)
    }
}

/// A source's render-block lattice. A render block is the quad between four
/// adjacent source cell centres, so the block grid starts half a cell inside
/// the source extent; blocks are grouped in 256 x 256 tiles. Sizes are
/// recomputed from extents the way RAS Mapper derives them (not taken from
/// the GeoTIFF cell size): which block a pixel centre that coincides with a
/// source cell centre falls into depends on those last bits.
#[derive(Clone, Copy, Debug)]
struct SourceLattice {
    /// Cell size as RAS Mapper derives it: max of extent width / columns and
    /// extent height / rows.
    cell: f64,
    min_x: f64,
    max_y: f64,
    /// Block-tile pitch (about 256 source cells).
    pitch: f64,
    tiles_x: usize,
    tiles_y: usize,
    /// Block height of a tile in block-tile row 0 (used for every column).
    q_row0: f64,
    /// Block width of a tile in block-tile column 0 (used for every row).
    q_col0: f64,
}

impl SourceLattice {
    fn new(gt: &[f64; 6], width: usize, height: usize) -> Self {
        let (w, h) = (width as f64, height as f64);
        let (left, top) = (gt[0], gt[3]);
        let (right, bottom) = (left + w * gt[1], top + h * gt[5]);
        let cell = ((right - left) / w).max((top - bottom) / h);
        let (tiles_x, tiles_y) = (width.div_ceil(TILE), height.div_ceil(TILE));
        let tile_span = TILE as f64 * cell;
        let min_x = left + cell * 0.5;
        let max_y = top - cell * 0.5;
        let max_x = min_x + tiles_x as f64 * tile_span;
        let min_y = max_y - tiles_y as f64 * tile_span;
        let pitch = ((max_x - min_x) / tiles_x as f64).max((max_y - min_y) / tiles_y as f64);
        SourceLattice {
            cell,
            min_x,
            max_y,
            pitch,
            tiles_x,
            tiles_y,
            q_row0: (max_y - (max_y - pitch)) / TILE as f64,
            q_col0: ((min_x + pitch) - min_x) / TILE as f64,
        }
    }

    /// Global block column and binary32 fraction for `x`, or `None` outside
    /// the block tiles.
    #[inline]
    fn col(&self, x: f64) -> Option<(usize, f32)> {
        let t = ((x - self.min_x) / self.pitch).floor();
        if !(t >= 0.0 && t < self.tiles_x as f64) {
            return None;
        }
        let lo = self.min_x + self.pitch * t;
        let q = (((self.min_x + self.pitch * (t + 1.0)) - lo) / TILE as f64).max(self.q_row0);
        let c = ((x - lo) / q).floor();
        let (c, f) = if c < 0.0 {
            (0.0, 0.0)
        } else if c >= TILE as f64 {
            (TILE as f64 - 1.0, 1.0)
        } else {
            (c, ((x - (lo + q * c)) / q) as f32)
        };
        Some((t as usize * TILE + c as usize, f))
    }

    /// Global block row and binary32 fraction (downward) for `y`.
    #[inline]
    fn row(&self, y: f64) -> Option<(usize, f32)> {
        let t = ((self.max_y - y) / self.pitch).floor();
        if !(t >= 0.0 && t < self.tiles_y as f64) {
            return None;
        }
        let hi = self.max_y - self.pitch * t;
        let q = self
            .q_col0
            .max((hi - (self.max_y - self.pitch * (t + 1.0))) / TILE as f64);
        let r = ((hi - y) / q).floor();
        let (r, f) = if r < 0.0 {
            (0.0, 0.0)
        } else if r >= TILE as f64 {
            (TILE as f64 - 1.0, 1.0)
        } else {
            (r, (((hi - q * r) - y) / q) as f32)
        };
        Some((t as usize * TILE + r as usize, f))
    }
}

/// The export's output lattice: 256-pixel tiles whose pitch and pixel size
/// RAS Mapper recomputes from tile extents, so pixel centres differ from
/// `origin + (i + 0.5) * cell` in the last bits.
pub(crate) struct OutputLattice {
    min_x: f64,
    max_y: f64,
    pitch: f64,
    tile_w: Vec<f64>,
    tile_h: Vec<f64>,
}

impl OutputLattice {
    pub(crate) fn new(grid: &Grid) -> Self {
        let (tx, ty) = (grid.width.div_ceil(TILE), grid.height.div_ceil(TILE));
        let (min_x, max_y, cs) = (grid.origin_x, grid.origin_y, grid.cell_size);
        let max_x = min_x + tx as f64 * cs * TILE as f64;
        let min_y = max_y - ty as f64 * cs * TILE as f64;
        let pitch = ((max_x - min_x) / tx as f64).max((max_y - min_y) / ty as f64);
        let span = |a: f64, b: f64| (b - a) / TILE as f64;
        OutputLattice {
            min_x,
            max_y,
            pitch,
            tile_w: (0..tx)
                .map(|m| span(min_x + pitch * m as f64, min_x + pitch * (m + 1) as f64))
                .collect(),
            tile_h: (0..ty)
                .map(|l| span(max_y - pitch * (l + 1) as f64, max_y - pitch * l as f64))
                .collect(),
        }
    }

    /// Pixel size of output tile (`l`, `m`).
    fn pixel(&self, l: usize, m: usize) -> f64 {
        self.tile_w[m].max(self.tile_h[l])
    }

    fn x(&self, m: usize, p: f64, i: usize) -> f64 {
        (self.min_x + self.pitch * m as f64) + p * (i as f64 + 0.5)
    }

    fn y(&self, l: usize, p: f64, j: usize) -> f64 {
        (self.max_y - self.pitch * l as f64) - p * (j as f64 + 0.5)
    }

    /// Full 256 x 256 geometry of output tile (`l`, `m`).
    pub(crate) fn tile_grid(&self, l: usize, m: usize) -> TileGrid {
        let p = self.pitch;
        TileGrid::new(
            self.min_x + p * m as f64,
            self.min_x + p * (m + 1) as f64,
            self.max_y - p * (l + 1) as f64,
            self.max_y - p * l as f64,
        )
    }
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

    /// Source rows `[row0, row0 + rows)` needed by output rows whose pixel
    /// centres lie between `y_top` and `y_bottom` (each block and the row
    /// below it, with margin).
    fn rows_for(&self, y_top: f64, y_bottom: f64) -> Option<(usize, usize)> {
        let l = &self.lattice;
        let cell = l.pitch / TILE as f64;
        let lo = ((l.max_y - y_top) / cell).floor() - 2.0;
        let hi = ((l.max_y - y_bottom) / cell).floor() + 3.0;
        let h = self.terrain.height as f64;
        let (lo, hi) = (lo.max(0.0), hi.min(h - 1.0));
        (hi >= lo).then_some((lo as usize, (hi - lo) as usize + 1))
    }
}

/// RAS Mapper's render-block interpolation at block (`row`, `col`) with
/// binary32 fractions `fx` (rightward) and `fy` (downward). `None` when the
/// block is not valid and owned by this source (bit 0 of its tile mask).
/// Corners a (top-left), b (top-right), c (bottom-right), d (bottom-left);
/// the block is split along both diagonals. Arithmetic is binary64 except
/// the block-centre term, which is held in binary32.
#[inline]
fn interpolate(band: &Band, row: usize, col: usize, fx: f32, fy: f32) -> Option<f64> {
    if row < band.row0 || row + 1 >= band.row0 + band.rows || col + 1 >= band.width {
        return None;
    }
    let i = (row - band.row0) * band.width + col;
    if !band.footprint[i] {
        return None;
    }
    let j = i + band.width;
    let (a, b, c, d) = (
        band.values[i],
        band.values[i + 1],
        band.values[j + 1],
        band.values[j],
    );
    if a.is_nan() || b.is_nan() || c.is_nan() || d.is_nan() {
        return None;
    }
    let (a, b, c, d) = (a as f64, b as f64, c as f64, d as f64);
    let e2 = ((((a + b) + c) + d) * 0.5) as f32 as f64;
    let (fx, fy) = (fx as f64, fy as f64);
    let ny = 1.0 - fy;
    Some(if fx > fy {
        if fx < ny {
            a + (b - a) * fx + (e2 - (a + b)) * fy
        } else {
            b + (e2 - (b + c)) * (1.0 - fx) + (c - b) * fy
        }
    } else if fx > ny {
        d + (c - d) * fx + (e2 - (c + d)) * ny
    } else {
        a + (e2 - (d + a)) * fx + (d - a) * fy
    })
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
    let crs = req.output_crs()?;
    let started = Instant::now();
    let cache = (req.max_working_bytes / 4 / req.sources.len() as u64).max(1 << 20);
    let mut sources = Vec::with_capacity(req.sources.len());
    for s in &req.sources {
        let path = model::resolve(base, &s.path);
        let terrain = NativeTerrain::open_crs(
            path.to_str()
                .ok_or_else(|| invalid("source path is not UTF-8"))?,
            &crs,
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
        let lattice = SourceLattice::new(&terrain.gt, terrain.width, terrain.height);
        sources.push(SourceData {
            terrain,
            mask_path,
            tiles_x,
            lattice,
            edges: Vec::new(),
        });
    }
    let grid = match req.grid {
        Some(g) => g,
        None => {
            // Finest source by RAS Mapper's extent-derived cell size; the
            // first (highest priority) wins ties.
            let auth = sources
                .iter()
                .min_by(|a, b| a.lattice.cell.total_cmp(&b.lattice.cell))
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
            snap_grid(extent, g[0], g[3], auth.lattice.cell)
                .ok_or_else(|| invalid("snapped grid is empty or too large"))?
        }
    };
    // Planned memory: output values, the source block caches
    // (max_working_bytes / 4 across sources), `batch` export-TIN tile results
    // held between parallel batches, and one export-TIN share per worker for
    // its segments, mask rows, value windows and TIN (checked per tile
    // before each read). Source row bands of the sampler come on top.
    let out_bytes = (grid.width as u64)
        .checked_mul(grid.height as u64)
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| MapError::Budget("output size overflows".into()))?;
    if out_bytes > req.max_working_bytes {
        return Err(MapError::Budget(format!(
            "output grid {}x{} needs {out_bytes} bytes over max_working_bytes {}",
            grid.width, grid.height, req.max_working_bytes
        )));
    }
    let threads = req.threads as u64;
    let remaining = req
        .max_working_bytes
        .saturating_sub(out_bytes + req.max_working_bytes / 4);
    // Each tile in flight holds its result (TILE_RESULT_BYTES) until the
    // batch is merged, and each running worker needs a share large enough
    // for a tile result plus its segments and TIN. Shrink the batch (and so
    // the number of concurrent workers) until that holds, or refuse before
    // any read.
    let tr = export_tin::TILE_RESULT_BYTES;
    let min_share = 2 * tr;
    let mut batch = (4 * threads).min(remaining / (2 * tr));
    let tin_share = loop {
        if batch == 0 {
            return Err(MapError::Budget(format!(
                "export TIN needs at least {} bytes beyond the output grid and source caches; max_working_bytes is {}",
                tr + min_share,
                req.max_working_bytes
            )));
        }
        let workers = threads.min(batch);
        let share = remaining.saturating_sub(batch * tr) / workers;
        if share >= min_share {
            break share;
        }
        batch -= 1;
    };
    let mut values = vec![NODATA; grid.width * grid.height];

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(req.threads)
        .build()
        .map_err(|e| MapError::Internal(format!("thread pool: {e}")))?;
    for s in sources.iter_mut() {
        s.edges = pool.install(|| s.scan_edges())?;
    }
    // Strips stay inside one 256-row output tile, so each strip has a
    // single output tile row.
    const STRIP: usize = 64;
    let width = grid.width;
    let lattice = OutputLattice::new(&grid);
    let counts: Vec<Result<Vec<u64>, MapError>> = pool.install(|| {
        values
            .par_chunks_mut(STRIP * width)
            .enumerate()
            .map(|(strip, out)| {
                let r0 = strip * STRIP;
                let r1 = r0 + out.len() / width;
                let l = r0 / TILE;
                let y_top = grid.origin_y - r0 as f64 * grid.cell_size;
                let y_bottom = grid.origin_y - r1 as f64 * grid.cell_size;
                let mut bands = Vec::with_capacity(sources.len());
                for s in &sources {
                    bands.push(match s.rows_for(y_top, y_bottom) {
                        Some((row0, rows)) => Some(s.band(row0, rows)?),
                        None => None,
                    });
                }
                let mut counts = vec![0_u64; sources.len()];
                let mut rows_at: Vec<Option<(usize, f32)>> = vec![None; r1 - r0];
                let mut cols_at: Vec<Option<(usize, f32)>> = vec![None; TILE];
                for m in 0..width.div_ceil(TILE) {
                    let p = lattice.pixel(l, m);
                    let (c0, c1) = (m * TILE, ((m + 1) * TILE).min(width));
                    // Sources in priority order; the first to answer a pixel wins.
                    for (k, (s, band)) in sources.iter().zip(&bands).enumerate() {
                        let Some(band) = band else { continue };
                        for (j, slot) in rows_at.iter_mut().enumerate() {
                            *slot = s.lattice.row(lattice.y(l, p, r0 + j - l * TILE));
                        }
                        for (i, slot) in cols_at[..c1 - c0].iter_mut().enumerate() {
                            *slot = s.lattice.col(lattice.x(m, p, i));
                        }
                        for (j, rowf) in rows_at.iter().enumerate() {
                            let Some((br, fy)) = *rowf else { continue };
                            for (i, colf) in cols_at[..c1 - c0].iter().enumerate() {
                                let o = (r0 + j) * width + c0 + i;
                                if out[o - r0 * width] != NODATA {
                                    continue;
                                }
                                let Some((bc, fx)) = *colf else { continue };
                                if let Some(v) = interpolate(band, br, bc, fx, fy) {
                                    // Held in binary32 like RAS Mapper's tile
                                    // buffer; rounded to 1/32 at the end.
                                    out[o - r0 * width] = v as f32;
                                    counts[k] += 1;
                                }
                            }
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
    let sample_seconds = started.elapsed().as_secs_f64();

    // Export TIN per output tile, in parallel batches; each tile writes only
    // its own pixels, clipped to the grid.
    let tin_started = Instant::now();
    let srcs: Vec<&dyn BlockSource> = sources.iter().map(|s| s as &dyn BlockSource).collect();
    let (tiles_x, tiles_y) = (width.div_ceil(TILE), grid.height.div_ceil(TILE));
    let tiles: Vec<(usize, usize)> = (0..tiles_y)
        .flat_map(|l| (0..tiles_x).map(move |m| (l, m)))
        .collect();
    let (mut n_tiles, mut n_segs, mut n_tris, mut n_px, mut n_cross) =
        (0_u64, 0_u64, 0_u64, 0_u64, 0_u64);
    let mut failures = Vec::new();
    for chunk in tiles.chunks(batch as usize) {
        let results: Vec<Result<export_tin::TileOutcome, MapError>> = pool.install(|| {
            chunk
                .par_iter()
                .map(|&(l, m)| {
                    let mut plan = export_tin::Plan {
                        share: tin_share,
                        used: 0,
                    };
                    export_tin::tile(&srcs, &lattice.tile_grid(l, m), &mut plan)
                })
                .collect()
        });
        for (&(l, m), r) in chunk.iter().zip(results) {
            let o = r?;
            if o.segments == 0 {
                continue;
            }
            n_tiles += 1;
            n_segs += o.segments;
            n_tris += o.triangles;
            n_cross += o.crossings;
            if let Some(e) = o.error {
                failures.push(json!({"tile": [l, m], "error": e.0}));
            }
            let Some(buf) = o.values else { continue };
            for i in 0..TILE.min(grid.height - l * TILE) {
                let row = (l * TILE + i) * width + m * TILE;
                for j in 0..TILE.min(width - m * TILE) {
                    let z = buf[i * TILE + j];
                    if !z.is_nan() {
                        values[row + j] = z;
                        n_px += 1;
                    }
                }
            }
        }
    }
    let failed_tiles = failures.len();
    failures.truncate(20);
    let tin = json!({
        "tiles": n_tiles, "segments": n_segs, "triangles": n_tris, "pixels": n_px,
        "constraint_crossings": n_cross, "failed_tiles": failed_tiles, "failures": failures,
        "hash_type_id": export_tin::HASH_TYPE_ID, "collect_workers": export_tin::COLLECT_WORKERS,
        "seconds": tin_started.elapsed().as_secs_f64(),
    });

    let mods_started = Instant::now();
    let version = crate::terrain_mods::RasVersion::parse(req.hecras_version.as_deref())?;
    let mods =
        crate::terrain_mods::apply_version(&req.modification_layers, &grid, &mut values, version)?;
    // The TIFF writer's rounding, once, after the modifications.
    for v in values.iter_mut() {
        if *v != NODATA {
            *v = quantize(*v as f64);
        }
    }
    let mods_seconds = mods_started.elapsed().as_secs_f64();
    let valid = values.iter().filter(|v| **v != NODATA).count() as u64;
    // Both request schemas run the /2 rule, so every receipt names it; a
    // schema 1 request is the /2 rule with no modification layers.
    let method_id = METHOD_ID;
    let report = json!({
        "method_id": method_id,
        "grid": {"origin_x": grid.origin_x, "origin_y": grid.origin_y, "cell_size": grid.cell_size,
                  "width": grid.width, "height": grid.height},
        "sources": sources.iter().zip(&per_source).map(|(s, n)| json!({
            "path": s.terrain.path, "geo_transform": s.terrain.gt,
            "width": s.terrain.width, "height": s.terrain.height, "pixels": n,
            "cache": s.terrain.stats().cache,
        })).collect::<Vec<_>>(),
        "sample_seconds": sample_seconds,
        "export_tin": tin,
        "stitch_tin": {"supplied": req.stitch_tin.is_some(), "used": false},
        "modifications": {"layers": mods, "seconds": mods_seconds,
                          "hecras_version": req.hecras_version, "hecras_rule": version.rule()},
        "valid_pixels": valid,
        "seconds": started.elapsed().as_secs_f64(),
    });
    Ok(Effective {
        grid,
        values,
        report,
    })
}

/// Report note for `epsg: 0` without `crs_geokeys` (accepted as in 0.19.0).
pub const EPSG_ZERO_WARNING: &str = "epsg 0 without crs_geokeys: the output is labelled ProjectedCSTypeGeoKey 0, which GDAL reads as a local CRS in metres; give crs_geokeys (any CRS) or a real EPSG code for a correct label";

/// Write `effective` as a tiled float32 GeoTIFF with nodata -9999.
pub fn write_geotiff(path: &Path, effective: Effective, epsg: u32) -> Result<(), MapError> {
    write_geotiff_crs(path, effective, &crate::crs::OutputCrs::Epsg(epsg))
}

/// [`write_geotiff`] with an EPSG code or verbatim GeoKeys (added after terrio
/// closes the file; pixels and the other tags are unchanged).
pub fn write_geotiff_crs(
    path: &Path,
    effective: Effective,
    crs: &crate::crs::OutputCrs,
) -> Result<(), MapError> {
    use wbraster::{DataType, Raster, raster::RasterData};
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
        crs: crate::geotiff::crs_info(crs),
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
    let text = path
        .to_str()
        .ok_or_else(|| invalid("output path is not UTF-8"))?;
    crate::terrio_pool::call(|| terrio::write_raster_dataset(&dataset, text, &options))?
        .map_err(|e| invalid(format!("effective terrain GeoTIFF write: {e}")))?;
    crate::geotiff::finish_crs(path, crs)
}

/// CLI entry: compute and write `effective_terrain.tif` and `report.json`.
pub fn run(req: &Request, base: &Path, out: &Path) -> Result<Value, MapError> {
    let crs = req.output_crs()?;
    let effective = compute(req, base)?;
    std::fs::create_dir_all(out)?;
    let mut report = effective.report.clone();
    let tif = out.join("effective_terrain.tif");
    let partial = out.join("effective_terrain.partial.tif");
    let write_started = Instant::now();
    write_geotiff_crs(&partial, effective, &crs)?;
    std::fs::rename(&partial, &tif)?;
    if crs.geokeys().is_some() {
        report["output_crs"] = crs.echo();
    }
    if crs == crate::crs::OutputCrs::Epsg(0) {
        report["crs_warning"] = EPSG_ZERO_WARNING.into();
    }
    report["status"] = "complete".into();
    report["output"] = tif.display().to_string().into();
    report["write_seconds"] = write_started.elapsed().as_secs_f64().into();
    report["terrio_pool"] = crate::terrio_pool::status();
    crate::sink::write_json(&out.join("report.json"), &report)?;
    Ok(report)
}
