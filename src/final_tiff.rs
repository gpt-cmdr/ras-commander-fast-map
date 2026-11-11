//! Final delivery GeoTIFF written in one pass from finalized Float32 product rows.
//!
//! The file is a single-band Float32 GeoTIFF over a full delivery grid: the product
//! window sits at an integer row/column offset and every other pixel is nodata. It is
//! tiled, zlib DEFLATE compressed (predictor 1) and carries optional overviews with
//! GDAL's `AVERAGE` arithmetic. Pixel values are the unchanged product values; NaN
//! (the engine's invalid marker) becomes the nodata value.
//!
//! Memory is bounded by one row of tiles per level, never by the grid. Tiles of one
//! tile row are compressed on the caller's Rayon pool and written in tile order, so
//! the file bytes do not depend on the thread count. Tiles that hold only nodata are
//! not compressed again: they share one encoded tile written after the header.
//!
//! Overviews follow GDAL's `GDALResampleChunk_AverageOrRMS` with a nodata mask
//! (`gcore/overview.cpp`): each level is resampled from the level before it, a
//! destination pixel is the weighted mean, accumulated in binary64 in row then column
//! order, of the valid source pixels it covers (fractional weights on the first and
//! last source row and column when the ratio is not exactly two), rounded once to
//! binary32. Source validity is GDAL's Float32 nodata mask, which also treats values
//! within about four units in the last place of the nodata value as nodata.
//! No GDAL, Python or terrio code is involved in encoding.
use crate::{MapError, native_output::FinalFormat};
use rayon::prelude::*;
use std::{
    fs::File,
    io::{BufWriter, Seek, SeekFrom, Write},
    path::Path,
};

/// Resolved placement and encoding of one delivery raster.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    /// Full delivery grid.
    pub width: usize,
    pub height: usize,
    /// Position of the product window's upper-left pixel on the full grid.
    pub row_offset: usize,
    pub col_offset: usize,
    pub window_width: usize,
    pub window_height: usize,
    /// GDAL geotransform of the full grid.
    pub transform: [f64; 6],
    pub block: usize,
    pub nodata: f32,
    pub zlevel: u32,
    pub factors: Vec<usize>,
}

impl Layout {
    /// Resolve wire options against the product window and its geotransform.
    /// Nothing is read or allocated.
    pub fn new(
        format: &FinalFormat,
        window_width: usize,
        window_height: usize,
        window_transform: [f64; 6],
    ) -> Result<Self, MapError> {
        format.validate()?;
        let invalid = |m: &str| MapError::Invalid(format!("final_format {m}"));
        if window_width == 0
            || window_height == 0
            || window_width > u32::MAX as usize
            || window_height > u32::MAX as usize
        {
            return Err(invalid("product window dimensions invalid"));
        }
        let w = window_transform;
        if w.iter().any(|v| !v.is_finite())
            || w[1] == 0.0
            || w[5] == 0.0
            || w[2] != 0.0
            || w[4] != 0.0
        {
            return Err(invalid("product window transform invalid"));
        }
        let (width, height, row_offset, col_offset, transform) = match &format.full_grid {
            None => (window_width, window_height, 0, 0, w),
            Some(g) => {
                let t = g.transform;
                if g.col_offset
                    .checked_add(window_width)
                    .is_none_or(|v| v > g.width)
                    || g.row_offset
                        .checked_add(window_height)
                        .is_none_or(|v| v > g.height)
                {
                    return Err(invalid("product window exceeds full_grid"));
                }
                // Same lattice: equal pixel sizes and an integer placement.
                let x = (w[0] - t[0]) / t[1];
                let y = (w[3] - t[3]) / t[5];
                if t[1].to_bits() != w[1].to_bits()
                    || t[5].to_bits() != w[5].to_bits()
                    || (x - g.col_offset as f64).abs() >= 1e-8
                    || (y - g.row_offset as f64).abs() >= 1e-8
                {
                    return Err(invalid(
                        "full_grid transform and offsets do not place the product window",
                    ));
                }
                (g.width, g.height, g.row_offset, g.col_offset, t)
            }
        };
        width
            .checked_mul(height)
            .and_then(|n| n.checked_mul(8))
            .ok_or_else(|| MapError::Budget("final_format grid size overflow".into()))?;
        Ok(Self {
            width,
            height,
            row_offset,
            col_offset,
            window_width,
            window_height,
            transform,
            block: format.block_size,
            nodata: format.nodata as f32,
            zlevel: format.zlevel,
            factors: format.overview_factors.clone(),
        })
    }

    /// Base grid then each overview: `ceil(size / factor)` as GDAL's GTiff driver.
    fn level_sizes(&self) -> Vec<(usize, usize)> {
        std::iter::once((self.width, self.height))
            .chain(
                self.factors
                    .iter()
                    .map(|f| (self.width.div_ceil(*f), self.height.div_ceil(*f))),
            )
            .collect()
    }

    fn tile_bytes(&self) -> u64 {
        (self.block * self.block * 4) as u64
    }

    /// Upper bound of one writer's allocations with `threads` compression workers:
    /// per level one row of tiles, its compressed tiles in flight, the rows handed to
    /// the next level and the carried source rows; tile tables; per-worker scratch.
    pub fn planned_bytes(&self, threads: usize) -> Option<u64> {
        let block = self.block as u64;
        let mut total = (threads.max(1) as u64).checked_mul(self.tile_bytes().checked_mul(3)?)?;
        total = total.checked_add(self.tile_bytes())?;
        for (w, h) in self.level_sizes() {
            let (w, h) = (w as u64, h as u64);
            let band = block.checked_mul(w)?.checked_mul(4)?;
            let across = w.div_ceil(block);
            let tiles = across.checked_mul(h.div_ceil(block))?;
            let in_flight = across.checked_mul(self.tile_bytes() + 1024)?;
            // Destination rows of one source band (at most the band itself) and
            // the carried source rows of a fractional ratio.
            let next = band / 2 + 4 * w * 4;
            total = total
                .checked_add(band)?
                .checked_add(in_flight)?
                .checked_add(next)?
                .checked_add(tiles.checked_mul(16)?)?
                .checked_add(w.checked_mul(32)?)?;
        }
        Some(total)
    }
}

/// What a finished file contains.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    /// Product pixels that were not NaN.
    pub valid_pixels: u64,
    /// Tiles over all levels, and how many of them point at the shared nodata tile.
    pub tiles: u64,
    pub shared_nodata_tiles: u64,
    pub file_bytes: u64,
}

/// One destination column or row of an overview: source span and edge weights.
#[derive(Clone, Copy, Debug)]
struct Span {
    s0: usize,
    s1: usize,
    first: f64,
    last: f64,
}

/// `GDALResampleChunk_AverageOrRMS`: source pixels `[s0, s1)` of destination pixel
/// `d` with `ratio = source / destination`, and the weights of the first and last.
fn span(d: usize, ratio: f64, source: usize) -> Span {
    let f0 = d as f64 * ratio;
    let s0 = (f0 + 1e-8) as usize;
    let f1 = (d + 1) as f64 * ratio;
    let mut s1 = (f1 - 1e-8).ceil() as usize;
    if s1 == s0 {
        s1 += 1;
    }
    if s1 > source {
        s1 = source;
    }
    Span {
        s0,
        s1,
        first: if s1 == s0 + 1 {
            1.0
        } else {
            1.0 - (f0 - s0 as f64)
        },
        last: 1.0 - (s1 as f64 - f1),
    }
}

/// GDAL's Float32 nodata mask (`GDALNoDataMaskBand`, `ARE_REAL_EQUAL`): a value
/// within about four units in the last place of the nodata value reads as nodata.
#[inline]
pub(crate) fn masked(value: f32, nodata: f32) -> bool {
    value == nodata || (value - nodata).abs() < f32::EPSILON * (value + nodata).abs() * 2.0
}

/// `GDALGetNoDataReplacementValue` for Float32: a valid mean that rounds onto the
/// nodata value moves one representable step away so it stays valid.
fn replacement(nodata: f32) -> f32 {
    if nodata == f32::MAX {
        f32::from_bits(nodata.to_bits() - 1)
    } else if nodata == 0.0 {
        f32::from_bits(1)
    } else if nodata > 0.0 {
        f32::from_bits(nodata.to_bits() + 1)
    } else {
        f32::from_bits(nodata.to_bits() - 1)
    }
}

/// Rows passed from one level to the next.
enum Rows {
    Empty(usize),
    Data(Vec<f32>, usize),
}

/// Rows offered to a level.
#[derive(Clone, Copy)]
enum Source<'a> {
    /// All nodata.
    Empty,
    /// Complete rows of the level.
    Level(&'a [f32]),
    /// Product rows (window width) for the base level; NaN marks invalid pixels.
    Window(&'a [f32]),
}

/// Resampler from one level to the next, fed consecutive source rows.
struct Down {
    src_w: usize,
    src_h: usize,
    dst_w: usize,
    dst_h: usize,
    ry: f64,
    cols: Vec<Span>,
    /// Destination columns that can be valid; the others are nodata without work.
    hull: (usize, usize),
    next: usize,
    carry_row0: usize,
    carry: Vec<Option<Vec<f32>>>,
}

impl Down {
    fn new(src: (usize, usize), dst: (usize, usize), src_hull: (usize, usize)) -> Self {
        let rx = src.0 as f64 / dst.0 as f64;
        let cols: Vec<Span> = (0..dst.0).map(|d| span(d, rx, src.0)).collect();
        let lo = cols.partition_point(|c| c.s1 <= src_hull.0);
        let hi = cols.partition_point(|c| c.s0 < src_hull.1).max(lo);
        Self {
            src_w: src.0,
            src_h: src.1,
            dst_w: dst.0,
            dst_h: dst.1,
            ry: src.1 as f64 / dst.1 as f64,
            cols,
            hull: (lo, hi),
            next: 0,
            carry_row0: 0,
            carry: Vec::new(),
        }
    }

    /// Accept `take` source rows starting at `row0` (`None`: all nodata) and
    /// return every destination row they complete, in order.
    fn advance(
        &mut self,
        data: Option<&[f32]>,
        row0: usize,
        take: usize,
        nodata: f32,
        pool: &rayon::ThreadPool,
    ) -> Result<Vec<Rows>, MapError> {
        let end = row0 + take;
        let (src_w, dst_w, src_h, ry) = (self.src_w, self.dst_w, self.src_h, self.ry);
        let mut stop = self.next;
        while stop < self.dst_h && span(stop, ry, src_h).s1 <= end {
            stop += 1;
        }
        let (carry, carry_row0) = (&self.carry, self.carry_row0);
        let row = |i: usize| -> Option<&[f32]> {
            if i < row0 {
                carry[i - carry_row0].as_deref()
            } else {
                data.map(|d| &d[(i - row0) * src_w..][..src_w])
            }
        };
        let mut out = Vec::new();
        let mut y = self.next;
        while y < stop {
            let blank = |y: usize| {
                let s = span(y, ry, src_h);
                (s.s0..s.s1).all(|i| row(i).is_none())
            };
            let empty = blank(y);
            let mut run = y + 1;
            while run < stop && blank(run) == empty {
                run += 1;
            }
            if empty {
                out.push(Rows::Empty(run - y));
            } else {
                let mut values = Vec::new();
                values
                    .try_reserve_exact((run - y) * dst_w)
                    .map_err(|e| MapError::Budget(format!("overview rows: {e}")))?;
                values.resize((run - y) * dst_w, nodata);
                let (cols, hull, replace) = (&self.cols, self.hull, replacement(nodata));
                pool.install(|| {
                    values
                        .par_chunks_mut(dst_w)
                        .enumerate()
                        .for_each(|(k, line)| {
                            let rows = span(y + k, ry, src_h);
                            for (o, c) in line[hull.0..hull.1].iter_mut().zip(&cols[hull.0..hull.1])
                            {
                                let mut count = 0_usize;
                                let (mut total, mut weight) = (0.0_f64, 0.0_f64);
                                for yy in rows.s0..rows.s1 {
                                    let Some(p) = row(yy) else { continue };
                                    let (mut sum, mut w) = (0.0_f64, 0.0_f64);
                                    if !masked(p[c.s0], nodata) {
                                        count += 1;
                                        w = c.first;
                                        sum = f64::from(p[c.s0]) * c.first;
                                    }
                                    if c.s0 + 1 < c.s1 {
                                        for v in &p[c.s0 + 1..c.s1 - 1] {
                                            if !masked(*v, nodata) {
                                                count += 1;
                                                w += 1.0;
                                                sum += f64::from(*v);
                                            }
                                        }
                                        if !masked(p[c.s1 - 1], nodata) {
                                            count += 1;
                                            w += c.last;
                                            sum += f64::from(p[c.s1 - 1]) * c.last;
                                        }
                                    }
                                    let wy = if yy == rows.s0 {
                                        rows.first
                                    } else if yy + 1 == rows.s1 {
                                        rows.last
                                    } else {
                                        1.0
                                    };
                                    total += sum * wy;
                                    weight += w * wy;
                                }
                                if count > 0 {
                                    let v = (total / weight) as f32;
                                    *o = if v == nodata { replace } else { v };
                                }
                            }
                        })
                });
                out.push(Rows::Data(values, run - y));
            }
            y = run;
        }
        // Keep only the source rows the next destination row starts from.
        let first = if stop < self.dst_h {
            span(stop, ry, src_h).s0.min(end)
        } else {
            end
        };
        let kept: Vec<Option<Vec<f32>>> =
            (first..end).map(|i| row(i).map(<[f32]>::to_vec)).collect();
        self.carry = kept;
        self.carry_row0 = first;
        self.next = stop;
        Ok(out)
    }
}

struct Level {
    width: usize,
    height: usize,
    across: usize,
    /// Columns that can be valid.
    hull: (usize, usize),
    offsets: Vec<u64>,
    counts: Vec<u64>,
    /// One row of tiles; allocated when the first valid row arrives.
    band: Vec<f32>,
    band_rows: usize,
    band_data: bool,
    tile_row: usize,
    received: usize,
    down: Option<Down>,
}

/// Per-worker tile scratch.
struct Scratch {
    raw: Vec<u8>,
    encoder: flate2::Compress,
}

fn deflate(encoder: &mut flate2::Compress, raw: &[u8]) -> Result<Vec<u8>, MapError> {
    encoder.reset();
    let mut out = Vec::with_capacity(raw.len() / 8 + 256);
    loop {
        let consumed = encoder.total_in() as usize;
        let status = encoder
            .compress_vec(&raw[consumed..], &mut out, flate2::FlushCompress::Finish)
            .map_err(|e| MapError::Internal(format!("deflate: {e}")))?;
        if status == flate2::Status::StreamEnd {
            return Ok(out);
        }
        out.reserve(out.capacity().max(4096));
    }
}

enum Tag {
    Short(Vec<u16>),
    Long(Vec<u32>),
    Offsets(Vec<u64>),
    Double(Vec<f64>),
    Ascii(String),
}

impl Tag {
    /// TIFF field type, value count and little-endian payload.
    fn encode(&self, big: bool) -> (u16, u64, Vec<u8>) {
        match self {
            Tag::Short(v) => (
                3,
                v.len() as u64,
                v.iter().flat_map(|x| x.to_le_bytes()).collect(),
            ),
            Tag::Long(v) => (
                4,
                v.len() as u64,
                v.iter().flat_map(|x| x.to_le_bytes()).collect(),
            ),
            Tag::Offsets(v) if big => (
                16,
                v.len() as u64,
                v.iter().flat_map(|x| x.to_le_bytes()).collect(),
            ),
            Tag::Offsets(v) => (
                4,
                v.len() as u64,
                v.iter().flat_map(|x| (*x as u32).to_le_bytes()).collect(),
            ),
            Tag::Double(v) => (
                12,
                v.len() as u64,
                v.iter().flat_map(|x| x.to_le_bytes()).collect(),
            ),
            Tag::Ascii(s) => {
                let mut bytes = s.as_bytes().to_vec();
                bytes.push(0);
                (2, bytes.len() as u64, bytes)
            }
        }
    }
}

/// One image file directory at even offset `at`, followed by its out-of-line values.
fn encode_ifd(tags: &[(u16, Tag)], at: u64, last: bool, big: bool) -> Vec<u8> {
    let (entry, inline, head, tail) = if big { (20, 8, 8, 8) } else { (12, 4, 2, 4) };
    let fixed = head + entry * tags.len() + tail;
    let mut extra: Vec<u8> = Vec::new();
    let mut out = Vec::with_capacity(fixed);
    if big {
        out.extend_from_slice(&(tags.len() as u64).to_le_bytes());
    } else {
        out.extend_from_slice(&(tags.len() as u16).to_le_bytes());
    }
    for (tag, value) in tags {
        let (kind, count, mut payload) = value.encode(big);
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&kind.to_le_bytes());
        if big {
            out.extend_from_slice(&count.to_le_bytes());
        } else {
            out.extend_from_slice(&(count as u32).to_le_bytes());
        }
        if payload.len() <= inline {
            payload.resize(inline, 0);
            out.extend_from_slice(&payload);
        } else {
            let offset = at + (fixed + extra.len()) as u64;
            if big {
                out.extend_from_slice(&offset.to_le_bytes());
            } else {
                out.extend_from_slice(&(offset as u32).to_le_bytes());
            }
            extra.extend_from_slice(&payload);
            if !extra.len().is_multiple_of(2) {
                extra.push(0);
            }
        }
    }
    let next = if last {
        0
    } else {
        at + (fixed + extra.len()) as u64
    };
    if big {
        out.extend_from_slice(&next.to_le_bytes());
    } else {
        out.extend_from_slice(&(next as u32).to_le_bytes());
    }
    out.extend_from_slice(&extra);
    out
}

/// Largest classic TIFF this writer starts; anything that could grow past it is BigTIFF.
const CLASSIC_LIMIT: u64 = 4_000_000_000;

/// Incremental final-format writer. Product rows arrive from the top, complete and
/// consecutive; the file is valid only after [`Writer::finish`].
pub struct Writer {
    file: BufWriter<File>,
    pos: u64,
    big: bool,
    layout: Layout,
    geo_keys: Vec<u16>,
    levels: Vec<Level>,
    shared: (u64, u64),
    rows_in: usize,
    valid: u64,
    shared_tiles: u64,
}

/// Resolve the CRS GeoKey code and whether it is geographic, refusing codes
/// the final writer cannot encode. Request planning calls this before reads.
pub fn check_epsg(epsg: u32) -> Result<(u16, bool), MapError> {
    let code = u16::try_from(epsg)
        .ok()
        .filter(|c| *c != 0)
        .ok_or_else(|| {
            MapError::Unsupported("final_format needs an EPSG code below 65536".into())
        })?;
    let geographic = terrio::crs::SpatialRef::from_epsg(epsg)
        .map_err(|e| MapError::Unsupported(format!("final_format CRS EPSG:{epsg}: {e}")))?
        .is_geographic();
    Ok((code, geographic))
}

impl Writer {
    /// Create the file and write its header and the shared nodata tile.
    pub fn create(path: &Path, layout: &Layout, epsg: u32) -> Result<Self, MapError> {
        let (code, geographic) = check_epsg(epsg)?;
        // GeoKey directory: model type, raster type PixelIsArea, the EPSG CRS code.
        let geo_keys = vec![
            1,
            1,
            0,
            3,
            1024,
            0,
            1,
            if geographic { 2 } else { 1 },
            1025,
            0,
            1,
            1,
            if geographic { 2048 } else { 3072 },
            0,
            1,
            code,
        ];
        let block = layout.block;
        let sizes = layout.level_sizes();
        let mut worst = 1_u64 << 20;
        let mut levels = Vec::with_capacity(sizes.len());
        let mut hull = (layout.col_offset, layout.col_offset + layout.window_width);
        for (i, (width, height)) in sizes.iter().copied().enumerate() {
            let across = width.div_ceil(block);
            let tiles = across
                .checked_mul(height.div_ceil(block))
                .ok_or_else(|| MapError::Budget("final_format tile count overflow".into()))?;
            worst = worst
                .checked_add((tiles as u64).saturating_mul(layout.tile_bytes() + 1024 + 32))
                .ok_or_else(|| MapError::Budget("final_format size overflow".into()))?;
            let reserve = |name: &str| -> Result<Vec<u64>, MapError> {
                let mut v = Vec::new();
                v.try_reserve_exact(tiles)
                    .map_err(|e| MapError::Budget(format!("final_format {name}: {e}")))?;
                v.resize(tiles, 0);
                Ok(v)
            };
            let down = sizes
                .get(i + 1)
                .map(|dst| Down::new((width, height), *dst, hull));
            let next_hull = down.as_ref().map(|d| d.hull);
            levels.push(Level {
                width,
                height,
                across,
                hull,
                offsets: reserve("tile offsets")?,
                counts: reserve("tile byte counts")?,
                band: Vec::new(),
                band_rows: 0,
                band_data: false,
                tile_row: 0,
                received: 0,
                down,
            });
            if let Some(h) = next_hull {
                hull = h;
            }
        }
        let big = worst > CLASSIC_LIMIT;
        let mut file = BufWriter::with_capacity(
            1 << 20,
            File::create(path).map_err(|e| MapError::Io(format!("{}: {e}", path.display())))?,
        );
        let head: Vec<u8> = if big {
            [
                b"II".as_slice(),
                &43_u16.to_le_bytes(),
                &8_u16.to_le_bytes(),
                &0_u16.to_le_bytes(),
                &0_u64.to_le_bytes(),
            ]
            .concat()
        } else {
            [
                b"II".as_slice(),
                &42_u16.to_le_bytes(),
                &0_u32.to_le_bytes(),
            ]
            .concat()
        };
        file.write_all(&head)?;
        let raw: Vec<u8> = std::iter::repeat_n(layout.nodata.to_le_bytes(), block * block)
            .flatten()
            .collect();
        let tile = deflate(
            &mut flate2::Compress::new(flate2::Compression::new(layout.zlevel), true),
            &raw,
        )?;
        file.write_all(&tile)?;
        let shared = (head.len() as u64, tile.len() as u64);
        Ok(Self {
            file,
            pos: shared.0 + shared.1,
            big,
            layout: layout.clone(),
            geo_keys,
            levels,
            shared,
            rows_in: 0,
            valid: 0,
            shared_tiles: 0,
        })
    }

    /// Append complete, consecutive product rows (window width). NaN is invalid;
    /// a non-finite value, or one GDAL would read as the nodata value, is refused
    /// because it could not be told from an invalid pixel in the file.
    pub fn write_rows(&mut self, values: &[f32], pool: &rayon::ThreadPool) -> Result<(), MapError> {
        let width = self.layout.window_width;
        if !values.len().is_multiple_of(width) {
            return Err(MapError::Invalid(
                "final_format requires complete rows".into(),
            ));
        }
        let rows = values.len() / width;
        if rows > self.layout.window_height - self.rows_in {
            return Err(MapError::Invalid(
                "final_format row count exceeds height".into(),
            ));
        }
        if rows == 0 {
            return Ok(());
        }
        if self.rows_in == 0 {
            self.feed(0, self.layout.row_offset, Source::Empty, pool)?;
        }
        self.feed(0, rows, Source::Window(values), pool)?;
        self.rows_in += rows;
        if self.rows_in == self.layout.window_height {
            let below = self.layout.height - self.layout.row_offset - self.layout.window_height;
            self.feed(0, below, Source::Empty, pool)?;
        }
        Ok(())
    }

    fn feed(
        &mut self,
        level: usize,
        rows: usize,
        source: Source<'_>,
        pool: &rayon::ThreadPool,
    ) -> Result<(), MapError> {
        let (block, nodata) = (self.layout.block, self.layout.nodata);
        let (window_width, col_offset) = (self.layout.window_width, self.layout.col_offset);
        let mut done = 0;
        while done < rows {
            let lvl = &mut self.levels[level];
            let width = lvl.width;
            let take = (rows - done).min(block - lvl.band_rows);
            if lvl.received + take > lvl.height {
                return Err(MapError::Internal("final_format level row overrun".into()));
            }
            let first = lvl.band_rows;
            if matches!(source, Source::Empty) {
                if lvl.band_data {
                    lvl.band[first * width..(first + take) * width].fill(nodata);
                }
            } else {
                if lvl.band.is_empty() {
                    lvl.band
                        .try_reserve_exact(block * width)
                        .map_err(|e| MapError::Budget(format!("final_format tile row: {e}")))?;
                    lvl.band.resize(block * width, nodata);
                }
                if !lvl.band_data {
                    // Earlier rows of this band were nodata; the buffer is reused.
                    lvl.band[..first * width].fill(nodata);
                    lvl.band_data = true;
                }
                let target = &mut lvl.band[first * width..(first + take) * width];
                match source {
                    Source::Level(data) => {
                        target.copy_from_slice(&data[done * width..(done + take) * width]);
                    }
                    Source::Window(data) => {
                        let data = &data[done * window_width..(done + take) * window_width];
                        let valid = pool.install(|| {
                            target
                                .par_chunks_mut(width)
                                .zip(data.par_chunks(window_width))
                                .map(|(line, product)| {
                                    line[..col_offset].fill(nodata);
                                    line[col_offset + window_width..].fill(nodata);
                                    let mut valid = 0_u64;
                                    for (o, v) in line[col_offset..].iter_mut().zip(product) {
                                        if v.is_nan() {
                                            *o = nodata;
                                        } else if !v.is_finite() || masked(*v, nodata) {
                                            return None;
                                        } else {
                                            *o = *v;
                                            valid += 1;
                                        }
                                    }
                                    Some(valid)
                                })
                                .try_reduce(|| 0_u64, |a, b| Some(a + b))
                        });
                        self.valid += valid.ok_or_else(|| {
                            MapError::Invalid(
                                "final_format: a valid product value is non-finite or reads as the nodata value"
                                    .into(),
                            )
                        })?;
                    }
                    Source::Empty => {}
                }
            }
            let lvl = &mut self.levels[level];
            let row0 = lvl.received;
            lvl.band_rows += take;
            lvl.received += take;
            let produced = match lvl.down.as_mut() {
                None => Vec::new(),
                Some(down) => {
                    let data = lvl
                        .band_data
                        .then(|| &lvl.band[first * width..(first + take) * width])
                        .filter(|_| !matches!(source, Source::Empty));
                    down.advance(data, row0, take, nodata, pool)?
                }
            };
            if lvl.band_rows == block || lvl.received == lvl.height {
                self.flush(level, pool)?;
            }
            for rows in produced {
                match rows {
                    Rows::Empty(n) => self.feed(level + 1, n, Source::Empty, pool)?,
                    Rows::Data(values, n) => {
                        self.feed(level + 1, n, Source::Level(&values), pool)?
                    }
                }
            }
            done += take;
        }
        Ok(())
    }

    /// Compress and write the level's current row of tiles in tile order.
    fn flush(&mut self, level: usize, pool: &rayon::ThreadPool) -> Result<(), MapError> {
        let (block, nodata, zlevel) = (self.layout.block, self.layout.nodata, self.layout.zlevel);
        let lvl = &mut self.levels[level];
        let base = lvl.tile_row * lvl.across;
        let encoded: Vec<Option<Vec<u8>>> = if !lvl.band_data {
            (0..lvl.across).map(|_| None).collect()
        } else {
            let (band, rows, width, hull) = (&lvl.band, lvl.band_rows, lvl.width, lvl.hull);
            let bits = nodata.to_bits();
            pool.install(|| {
                (0..lvl.across)
                    .into_par_iter()
                    .map_init(
                        || Scratch {
                            raw: Vec::with_capacity(block * block * 4),
                            encoder: flate2::Compress::new(flate2::Compression::new(zlevel), true),
                        },
                        |scratch, tile| {
                            let x0 = tile * block;
                            let x1 = (x0 + block).min(width);
                            if x1 <= hull.0
                                || x0 >= hull.1
                                || (0..rows).all(|r| {
                                    band[r * width + x0..r * width + x1]
                                        .iter()
                                        .all(|v| v.to_bits() == bits)
                                })
                            {
                                return Ok(None);
                            }
                            // Edge tiles are padded with nodata to the full tile.
                            scratch.raw.clear();
                            for r in 0..block {
                                let line: &[f32] = if r < rows {
                                    &band[r * width + x0..r * width + x1]
                                } else {
                                    &[]
                                };
                                scratch
                                    .raw
                                    .extend(line.iter().flat_map(|v| v.to_le_bytes()));
                                scratch.raw.extend(
                                    std::iter::repeat_n(nodata.to_le_bytes(), block - line.len())
                                        .flatten(),
                                );
                            }
                            deflate(&mut scratch.encoder, &scratch.raw).map(Some)
                        },
                    )
                    .collect::<Result<Vec<_>, MapError>>()
            })?
        };
        for (tile, bytes) in encoded.into_iter().enumerate() {
            match bytes {
                None => {
                    lvl.offsets[base + tile] = self.shared.0;
                    lvl.counts[base + tile] = self.shared.1;
                    self.shared_tiles += 1;
                }
                Some(bytes) => {
                    lvl.offsets[base + tile] = self.pos;
                    lvl.counts[base + tile] = bytes.len() as u64;
                    self.file.write_all(&bytes)?;
                    self.pos += bytes.len() as u64;
                }
            }
        }
        if !self.big && self.pos > CLASSIC_LIMIT {
            return Err(MapError::Internal(
                "final_format classic TIFF size bound exceeded".into(),
            ));
        }
        lvl.tile_row += 1;
        lvl.band_rows = 0;
        lvl.band_data = false;
        Ok(())
    }

    /// Write the directories and close the file.
    pub fn finish(mut self) -> Result<Summary, MapError> {
        if self.rows_in != self.layout.window_height
            || self.levels.iter().any(|l| l.received != l.height)
        {
            return Err(MapError::Invalid("final_format incomplete rows".into()));
        }
        let layout = &self.layout;
        let nodata = format!("{}", f64::from(layout.nodata));
        let t = layout.transform;
        if !self.pos.is_multiple_of(2) {
            self.file.write_all(&[0])?;
            self.pos += 1;
        }
        let first = self.pos;
        let mut tiles = 0_u64;
        let count = self.levels.len();
        for (i, lvl) in self.levels.iter_mut().enumerate() {
            tiles += lvl.offsets.len() as u64;
            let mut tags: Vec<(u16, Tag)> = Vec::new();
            if i > 0 {
                // Reduced-resolution image.
                tags.push((254, Tag::Long(vec![1])));
            }
            tags.extend([
                (256, Tag::Long(vec![lvl.width as u32])),
                (257, Tag::Long(vec![lvl.height as u32])),
                (258, Tag::Short(vec![32])),
                (259, Tag::Short(vec![8])),
                (262, Tag::Short(vec![1])),
                (277, Tag::Short(vec![1])),
                (284, Tag::Short(vec![1])),
                (317, Tag::Short(vec![1])),
                (322, Tag::Long(vec![layout.block as u32])),
                (323, Tag::Long(vec![layout.block as u32])),
                (324, Tag::Offsets(std::mem::take(&mut lvl.offsets))),
                (325, Tag::Offsets(std::mem::take(&mut lvl.counts))),
                (339, Tag::Short(vec![3])),
            ]);
            if i == 0 {
                tags.extend([
                    (33550, Tag::Double(vec![t[1], -t[5], 0.0])),
                    (33922, Tag::Double(vec![0.0, 0.0, 0.0, t[0], t[3], 0.0])),
                    (34735, Tag::Short(self.geo_keys.clone())),
                ]);
                if count > 1 {
                    tags.push((
                        42112,
                        Tag::Ascii(
                            "<GDALMetadata>\n  <Item name=\"resampling\" domain=\"rio_overview\">average</Item>\n</GDALMetadata>\n"
                                .into(),
                        ),
                    ));
                }
            }
            tags.push((42113, Tag::Ascii(nodata.clone())));
            let ifd = encode_ifd(&tags, self.pos, i + 1 == count, self.big);
            self.file.write_all(&ifd)?;
            self.pos += ifd.len() as u64;
        }
        if !self.big && self.pos > u64::from(u32::MAX) {
            return Err(MapError::Internal(
                "final_format classic TIFF size bound exceeded".into(),
            ));
        }
        let mut file = self
            .file
            .into_inner()
            .map_err(|e| MapError::Io(format!("final_format flush: {}", e.error())))?;
        if self.big {
            file.seek(SeekFrom::Start(8))?;
            file.write_all(&first.to_le_bytes())?;
        } else {
            file.seek(SeekFrom::Start(4))?;
            file.write_all(&(first as u32).to_le_bytes())?;
        }
        file.flush()?;
        Ok(Summary {
            valid_pixels: self.valid,
            tiles,
            shared_nodata_tiles: self.shared_tiles,
            file_bytes: self.pos,
        })
    }
}

/// Write a complete product window held in memory.
pub fn write(
    path: &Path,
    values: &[f32],
    layout: &Layout,
    epsg: u32,
    pool: &rayon::ThreadPool,
) -> Result<Summary, MapError> {
    if layout
        .window_width
        .checked_mul(layout.window_height)
        .is_none_or(|n| n != values.len())
    {
        return Err(MapError::Invalid(
            "final_format buffer dimensions mismatch".into(),
        ));
    }
    let mut writer = Writer::create(path, layout, epsg)?;
    // Bounded pieces keep the per-call work on tile-row boundaries.
    for chunk in values.chunks(layout.window_width * layout.block) {
        writer.write_rows(chunk, pool)?;
    }
    writer.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_output::{FinalFormat, FullGrid, OverviewResampling};
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };
    use wbraster::raster::RasterData;

    const NODATA: f32 = -9999.0;

    fn directory(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "fast-map-final-tiff-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        path
    }

    fn pool(threads: usize) -> rayon::ThreadPool {
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap()
    }

    /// Full grid (height, width), placement (row, col), window (height, width).
    fn layout(
        full: (usize, usize),
        at: (usize, usize),
        window: (usize, usize),
        block: usize,
        factors: &[usize],
    ) -> Layout {
        let transform = [1000.5, 3.0, 0.0, 5000.25, 0.0, -3.0];
        let format = FinalFormat {
            nodata: f64::from(NODATA),
            block_size: block,
            zlevel: 6,
            full_grid: Some(FullGrid {
                width: full.1,
                height: full.0,
                row_offset: at.0,
                col_offset: at.1,
                transform,
            }),
            overview_factors: factors.to_vec(),
            overview_resampling: OverviewResampling::Average,
        };
        Layout::new(
            &format,
            window.1,
            window.0,
            [
                transform[0] + at.1 as f64 * 3.0,
                3.0,
                0.0,
                transform[3] - at.0 as f64 * 3.0,
                0.0,
                -3.0,
            ],
        )
        .unwrap()
    }

    /// Deterministic product: clustered NaN holes, signed zero, tiny and large values.
    fn product(height: usize, width: usize, seed: u32) -> Vec<f32> {
        let mut state = seed.wrapping_mul(2_654_435_761).wrapping_add(12345);
        (0..height * width)
            .map(|i| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let (r, c) = (i / width, i % width);
                if (r / 5 + c / 7 + seed as usize).is_multiple_of(3) || state >> 28 == 0 {
                    return f32::NAN;
                }
                match state >> 24 & 31 {
                    0 => -0.0,
                    1 => f32::from_bits(1),
                    2 => 3.0e38,
                    3 => -9998.0,
                    4 => -10000.0,
                    _ => (state >> 8) as f32 * 1.0e-3 - 4000.0,
                }
            })
            .collect()
    }

    /// Whole-array statement of the same contract, without tiles, bands or carries.
    fn reference(layout: &Layout, product: &[f32]) -> Vec<Vec<f32>> {
        let mut base = vec![NODATA; layout.width * layout.height];
        for r in 0..layout.window_height {
            for c in 0..layout.window_width {
                let v = product[r * layout.window_width + c];
                if !v.is_nan() {
                    base[(r + layout.row_offset) * layout.width + c + layout.col_offset] = v;
                }
            }
        }
        let sizes = layout.level_sizes();
        let mut levels = vec![base];
        for pair in sizes.windows(2) {
            let ((sw, sh), (dw, dh)) = (pair[0], pair[1]);
            let src = levels.last().unwrap();
            let (rx, ry) = (sw as f64 / dw as f64, sh as f64 / dh as f64);
            let mut dst = vec![NODATA; dw * dh];
            for y in 0..dh {
                let rows = span(y, ry, sh);
                for x in 0..dw {
                    let cols = span(x, rx, sw);
                    let (mut total, mut weight, mut count) = (0.0_f64, 0.0_f64, 0);
                    for yy in rows.s0..rows.s1 {
                        let (mut sum, mut w) = (0.0_f64, 0.0_f64);
                        for xx in cols.s0..cols.s1 {
                            let v = src[yy * sw + xx];
                            if masked(v, NODATA) {
                                continue;
                            }
                            let wx = if xx == cols.s0 {
                                cols.first
                            } else if xx + 1 == cols.s1 {
                                cols.last
                            } else {
                                1.0
                            };
                            count += 1;
                            w += wx;
                            sum += f64::from(v) * wx;
                        }
                        let wy = if yy == rows.s0 {
                            rows.first
                        } else if yy + 1 == rows.s1 {
                            rows.last
                        } else {
                            1.0
                        };
                        total += sum * wy;
                        weight += w * wy;
                    }
                    if count > 0 {
                        let v = (total / weight) as f32;
                        dst[y * dw + x] = if v == NODATA { replacement(NODATA) } else { v };
                    }
                }
            }
            levels.push(dst);
        }
        levels
    }

    fn read_window(
        raster: &mut terrio::LazyRaster,
        window: terrio::Window,
        overview: Option<usize>,
    ) -> Vec<f32> {
        let ds = raster
            .read_window(
                window,
                &terrio::WindowOptions {
                    bands: vec![1],
                    overview,
                },
            )
            .unwrap();
        let RasterData::F32(values) = ds.raster.data else {
            panic!("not Float32")
        };
        values
    }

    /// Every level decoded by the terrio reader.
    fn read(path: &Path) -> Vec<Vec<f32>> {
        let mut raster = terrio::LazyRaster::open_tiff(path.to_str().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(raster.nodata(), Some(f64::from(NODATA)));
        assert_eq!(raster.data_type(), terrio::DataType::F32);
        let mut sizes = vec![(raster.width(), raster.height())];
        sizes.extend(raster.overviews());
        sizes
            .iter()
            .enumerate()
            .map(|(i, (width, height))| {
                let window = terrio::Window {
                    x: 0,
                    y: 0,
                    width: *width,
                    height: *height,
                };
                read_window(&mut raster, window, i.checked_sub(1))
            })
            .collect()
    }

    fn bits(values: &[f32]) -> Vec<u32> {
        values.iter().map(|v| v.to_bits()).collect()
    }

    fn write_in_calls(
        path: &Path,
        values: &[f32],
        layout: &Layout,
        rows: usize,
        threads: usize,
    ) -> Summary {
        let pool = pool(threads);
        let mut writer = Writer::create(path, layout, 3433).unwrap();
        for chunk in values.chunks(layout.window_width * rows) {
            writer.write_rows(chunk, &pool).unwrap();
        }
        writer.finish().unwrap()
    }

    #[test]
    fn odd_sizes_and_placements_round_trip_with_any_threads_or_row_batches() {
        let dir = directory("round-trip");
        // Full (h, w), placement (row, col), window (h, w): odd sizes, a fractional
        // cascade ratio, edges, a single row/column and tile-aligned placement.
        for (case, (full, at, window)) in [
            ((53, 71), (7, 19), (31, 37)),
            ((67, 45), (1, 2), (65, 43)),
            ((33, 49), (0, 0), (33, 49)),
            ((50, 66), (33, 49), (17, 17)),
            ((1, 41), (0, 3), (1, 30)),
            ((37, 1), (5, 0), (20, 1)),
            ((5, 3), (1, 1), (3, 2)),
            ((64, 96), (16, 32), (32, 48)),
        ]
        .into_iter()
        .enumerate()
        {
            let layout = layout(full, at, window, 16, &[2, 4, 8]);
            let values = product(window.0, window.1, case as u32);
            let expected = reference(&layout, &values);
            let path = dir.join(format!("{case}.tif"));
            let summary = write(&path, &values, &layout, 3433, &pool(1)).unwrap();
            assert_eq!(
                summary.valid_pixels,
                values.iter().filter(|v| !v.is_nan()).count() as u64
            );
            let decoded = read(&path);
            assert_eq!(decoded.len(), 4, "case {case}");
            for (level, (actual, wanted)) in decoded.iter().zip(&expected).enumerate() {
                assert_eq!(bits(actual), bits(wanted), "case {case} level {level}");
            }
            let first = fs::read(&path).unwrap();
            assert_eq!(first.len() as u64, summary.file_bytes);
            for (rows, threads) in [(1, 4), (7, 3), (16, 2), (window.0, 8)] {
                let other = dir.join(format!("{case}-{rows}-{threads}.tif"));
                assert_eq!(
                    write_in_calls(&other, &values, &layout, rows, threads),
                    summary
                );
                assert_eq!(fs::read(&other).unwrap(), first, "case {case} rows {rows}");
            }
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn all_nodata_product_shares_one_tile_and_grid_padding_is_not_materialized() {
        let dir = directory("nodata");
        let small = layout((53, 71), (7, 19), (31, 37), 16, &[2, 4]);
        let path = dir.join("empty.tif");
        let summary = write(&path, &vec![f32::NAN; 31 * 37], &small, 3433, &pool(2)).unwrap();
        assert_eq!(summary.valid_pixels, 0);
        assert_eq!(summary.shared_nodata_tiles, summary.tiles);
        for level in read(&path) {
            assert!(level.iter().all(|v| v.to_bits() == NODATA.to_bits()));
        }
        // 1.6 billion pixels of padding around one valid pixel: a BigTIFF of a few
        // hundred kilobytes, written from tile-row buffers only.
        let big = layout((40_000, 40_000), (20_001, 30_003), (1, 1), 256, &[2, 4]);
        assert!(big.planned_bytes(4).unwrap() < 256 << 20);
        let path = dir.join("big.tif");
        let summary = write(&path, &[12.5], &big, 3433, &pool(2)).unwrap();
        assert_eq!(summary.shared_nodata_tiles, summary.tiles - 3);
        assert!(summary.file_bytes < 1 << 20);
        assert_eq!(&fs::read(&path).unwrap()[..4], b"II\x2b\x00");
        let mut raster = terrio::LazyRaster::open_tiff(path.to_str().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(raster.overviews(), vec![(20_000, 20_000), (10_000, 10_000)]);
        for (overview, (x, y)) in [
            (None, (30_003, 20_001)),
            (Some(0), (15_001, 10_000)),
            (Some(1), (7_500, 5_000)),
        ] {
            let window = terrio::Window {
                x: x - 1,
                y: y - 1,
                width: 3,
                height: 3,
            };
            let mut wanted = [NODATA; 9];
            wanted[4] = 12.5;
            assert_eq!(
                bits(&read_window(&mut raster, window, overview)),
                bits(&wanted),
                "{overview:?}"
            );
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn values_that_read_as_nodata_are_refused_and_overview_means_stay_valid() {
        let dir = directory("sentinel");
        let layout = layout((6, 6), (1, 1), (4, 4), 16, &[2]);
        let pool = pool(1);
        let near = f32::from_bits(NODATA.to_bits() + 1);
        for bad in [NODATA, near, f32::INFINITY, f32::NEG_INFINITY] {
            let mut values = vec![1.0_f32; 16];
            values[5] = bad;
            let error = write(&dir.join("bad.tif"), &values, &layout, 3433, &pool).unwrap_err();
            assert_eq!(error.kind(), "invalid", "{bad}");
        }
        // A mean of exactly the nodata value steps to the next Float32 and stays
        // valid; a mean next to holes uses the valid pixels only.
        let mut values = vec![f32::NAN; 16];
        values[5..7].copy_from_slice(&[-9998.0, -10000.0]);
        values[9..11].copy_from_slice(&[-9997.0, -10001.0]);
        values[0] = 3.0;
        let path = dir.join("means.tif");
        write(&path, &values, &layout, 3433, &pool).unwrap();
        let levels = read(&path);
        assert_eq!(bits(&levels[1]), bits(&reference(&layout, &values)[1]));
        // Window pixels (1..3, 1..3) are full-grid (2..4, 2..4): overview pixel (1, 1).
        assert_eq!(levels[1][3 + 1], f32::from_bits(NODATA.to_bits() - 1));
        // Window pixel (0, 0) is full-grid (1, 1): alone in overview pixel (0, 0).
        assert_eq!(levels[1][0], 3.0);
        assert_eq!(levels[1][2].to_bits(), NODATA.to_bits());
        assert!(masked(near, NODATA) && !masked(-9998.99, NODATA));
        assert_eq!(replacement(NODATA), f32::from_bits(NODATA.to_bits() - 1));
        assert_eq!(replacement(0.0), f32::from_bits(1));
        assert_eq!(replacement(5.0), f32::from_bits(5.0_f32.to_bits() + 1));
        assert_eq!(
            replacement(f32::MAX),
            f32::from_bits(f32::MAX.to_bits() - 1)
        );
        // Incomplete and misshapen deliveries are errors, not accepted files.
        let mut writer = Writer::create(&dir.join("short.tif"), &layout, 3433).unwrap();
        assert!(writer.write_rows(&[1.0; 3], &pool).is_err());
        writer.write_rows(&[1.0; 8], &pool).unwrap();
        assert!(writer.write_rows(&[1.0; 12], &pool).is_err());
        assert!(writer.finish().is_err());
        assert!(Writer::create(&dir.join("crs.tif"), &layout, 70_000).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn placement_is_validated_and_plans_are_bounded_before_any_read() {
        let format = |grid: Option<FullGrid>| FinalFormat {
            nodata: -9999.0,
            block_size: 256,
            zlevel: 6,
            full_grid: grid,
            overview_factors: vec![2, 4],
            overview_resampling: OverviewResampling::Average,
        };
        let window = [
            100.0 + 7787.0 * 10.0,
            10.0,
            0.0,
            900.0 - 1018.0 * 10.0,
            0.0,
            -10.0,
        ];
        let grid = FullGrid {
            width: 26316,
            height: 28916,
            row_offset: 1018,
            col_offset: 7787,
            transform: [100.0, 10.0, 0.0, 900.0, 0.0, -10.0],
        };
        let layout = Layout::new(&format(Some(grid.clone())), 18228, 27370, window).unwrap();
        assert_eq!(
            layout.level_sizes(),
            vec![(26316, 28916), (13158, 14458), (6579, 7229)]
        );
        // One row of tiles per level, not the 3 GiB grid.
        let planned = layout.planned_bytes(8).unwrap();
        assert!(
            planned > 26316 * 256 * 4 && planned < 160 << 20,
            "{planned}"
        );
        let alone = Layout::new(&format(None), 18228, 27370, window).unwrap();
        assert_eq!(
            (alone.width, alone.height, alone.row_offset),
            (18228, 27370, 0)
        );
        let moved = |transform: [f64; 6]| FullGrid {
            transform,
            ..grid.clone()
        };
        for (bad, width, height) in [
            (
                FullGrid {
                    col_offset: 7788,
                    ..grid.clone()
                },
                18228,
                27370,
            ),
            (
                FullGrid {
                    row_offset: 1017,
                    ..grid.clone()
                },
                18228,
                27370,
            ),
            (
                moved([100.0, 10.000001, 0.0, 900.0, 0.0, -10.0]),
                18228,
                27370,
            ),
            (moved([100.5, 10.0, 0.0, 900.0, 0.0, -10.0]), 18228, 27370),
            (grid.clone(), 18530, 27370),
            (grid.clone(), 18228, 27899),
        ] {
            let error = Layout::new(&format(Some(bad)), width, height, window).unwrap_err();
            assert_eq!(error.kind(), "invalid");
        }
        assert!(Layout::new(&format(None), 0, 4, window).is_err());
    }

    /// Opt-in single observation of writer time on a mostly empty grid.
    #[test]
    #[ignore]
    fn benchmark_sparse_grid() {
        let dir = directory("benchmark");
        let (full, at, window) = ((20_011, 20_003), (1_019, 3_787), (17_371, 13_229));
        let layout = layout(full, at, window, 256, &[2, 4]);
        let values: Vec<f32> = (0..window.0 * window.1)
            .map(|i| {
                let (r, c) = (i / window.1, i % window.1);
                let centre =
                    window.0 as f64 / 2.0 + window.0 as f64 / 3.0 * (c as f64 / 1500.0).sin();
                if (r as f64 - centre).abs() < window.0 as f64 * 0.075 {
                    (37.25 + 0.013 * c as f64 - 0.007 * r as f64
                        + ((i * 7919) % 1000) as f64 * 1e-3) as f32
                } else {
                    f32::NAN
                }
            })
            .collect();
        for threads in [1, 4, 1, 4] {
            let path = dir.join(format!("{threads}.tif"));
            let tick = std::time::Instant::now();
            let summary = write(&path, &values, &layout, 3433, &pool(threads)).unwrap();
            println!(
                "final_tiff_benchmark threads={threads} seconds={:.3} {summary:?}",
                tick.elapsed().as_secs_f64()
            );
        }
        fs::remove_dir_all(dir).unwrap();
    }
}
