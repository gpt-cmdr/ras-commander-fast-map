//! Explicit stored-terrain sampling geometry. Source tags remain source identity.
use crate::MapError;
use serde::{Deserialize, Serialize};

pub const ID: &str = "rasmapper-stored-tile-grid/1";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Contract {
    pub id: String,
    pub source_width: usize,
    pub source_height: usize,
    pub tile_size: usize,
    pub source_geo_transform: [f64; 6],
    pub output_geo_transform: [f64; 6],
}

#[derive(Clone, Copy, Debug)]
pub struct Layout {
    pub width: usize,
    pub height: usize,
    pub source: [f64; 6],
    pub output: [f64; 6],
    pitch: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Tile {
    pub left: f64,
    pub right: f64,
    pub top: f64,
    pub bottom: f64,
    pub step: f64,
}

fn invalid(message: &str) -> MapError {
    MapError::Invalid(message.into())
}
pub fn same(a: [f64; 6], b: [f64; 6]) -> bool {
    a.iter().zip(b).all(|(a, b)| a.to_bits() == b.to_bits())
}

impl Contract {
    pub fn exactly_matches(&self, other: &Self) -> bool {
        self.id == other.id
            && self.source_width == other.source_width
            && self.source_height == other.source_height
            && self.tile_size == other.tile_size
            && same(self.source_geo_transform, other.source_geo_transform)
            && same(self.output_geo_transform, other.output_geo_transform)
    }
    pub fn validate(&self, window: [usize; 4]) -> Result<Layout, MapError> {
        if self.id != ID || self.tile_size != 256 {
            return Err(MapError::Unsupported(
                "unsupported stored tile grid contract".into(),
            ));
        }
        let layout = Layout::new(
            self.source_width,
            self.source_height,
            self.source_geo_transform,
        )?;
        if !same(layout.output, self.output_geo_transform) {
            return Err(invalid(
                "stored tile output framing does not match source-derived geometry",
            ));
        }
        layout.validate_window(window)?;
        // This bounded initial contract admits only layouts whose actual logical
        // centers equal the advertised full output lattice. No per-pixel arrays.
        let cols = self.source_width.div_ceil(256);
        let rows = self.source_height.div_ceil(256);
        if cols
            .checked_mul(rows)
            .and_then(|n| n.checked_mul(512))
            .is_none_or(|n| n > 16_777_216)
        {
            return Err(MapError::Unsupported(
                "stored tile center admission exceeds bounded layout scope".into(),
            ));
        }
        for tr in 0..rows {
            for tc in 0..cols {
                let tile = layout.tile(tr, tc);
                if ![tile.left, tile.right, tile.top, tile.bottom, tile.step]
                    .iter()
                    .all(|v| v.is_finite())
                    || tile.step <= 0.
                {
                    return Err(invalid("stored tile coordinate arithmetic overflow"));
                }
                for lc in 0..256.min(self.source_width - tc * 256) {
                    let global = tc * 256 + lc;
                    if tile.x(lc).to_bits()
                        != (layout.output[0] + layout.output[1] * (global as f64 + 0.5)).to_bits()
                    {
                        return Err(MapError::Unsupported(
                            "stored tile logical X centers differ from delivery lattice".into(),
                        ));
                    }
                }
                for lr in 0..256.min(self.source_height - tr * 256) {
                    let global = tr * 256 + lr;
                    if tile.y(lr).to_bits()
                        != (layout.output[3] - layout.output[1] * (global as f64 + 0.5)).to_bits()
                    {
                        return Err(MapError::Unsupported(
                            "stored tile logical Y centers differ from delivery lattice".into(),
                        ));
                    }
                }
            }
        }
        Ok(layout)
    }
}

impl Layout {
    pub fn new(width: usize, height: usize, g: [f64; 6]) -> Result<Self, MapError> {
        if width == 0
            || height == 0
            || width > i32::MAX as usize
            || height > i32::MAX as usize
            || !g.iter().all(|v| v.is_finite())
            || g[1] <= 0.
            || g[5] != -g[1]
            || g[2] != 0.
            || g[4] != 0.
        {
            return Err(invalid(
                "stored tile grid requires finite north-up square source geometry",
            ));
        }
        let right = g[0] + width as f64 * g[1];
        let bottom = g[3] + height as f64 * g[5];
        let cell = ((right - g[0]) / width as f64).max((g[3] - bottom) / height as f64);
        let cols = width.div_ceil(256);
        let rows = height.div_ceil(256);
        cols.checked_mul(256)
            .and_then(|_| rows.checked_mul(256))
            .ok_or_else(|| invalid("stored tile padding overflow"))?;
        let pitch0 = 256. * cell;
        let padded_right = g[0] + cols as f64 * pitch0;
        let padded_bottom = g[3] - rows as f64 * pitch0;
        let pitch = ((padded_right - g[0]) / cols as f64).max((g[3] - padded_bottom) / rows as f64);
        if ![right, bottom, cell, padded_right, padded_bottom, pitch]
            .iter()
            .all(|v| v.is_finite())
            || cell <= 0.
            || pitch <= 0.
        {
            return Err(invalid("stored tile extent arithmetic overflow"));
        }
        Ok(Self {
            width,
            height,
            source: g,
            output: [g[0], cell, 0., g[3], 0., -cell],
            pitch,
        })
    }
    pub fn validate_window(&self, w: [usize; 4]) -> Result<(), MapError> {
        let derived = Self::new(self.width, self.height, self.source)?;
        if !same(self.output, derived.output) || self.pitch.to_bits() != derived.pitch.to_bits() {
            return Err(invalid("stored tile layout was changed after derivation"));
        }
        let [col, row, width, height] = w;
        if width == 0
            || height == 0
            || col.checked_add(width).is_none_or(|n| n > self.width)
            || row.checked_add(height).is_none_or(|n| n > self.height)
        {
            return Err(invalid("stored tile window outside original source"));
        }
        Ok(())
    }
    pub fn window_transform(&self, window: [usize; 4]) -> [f64; 6] {
        let g = self.output;
        [
            g[0] + window[0] as f64 * g[1],
            g[1],
            0.,
            g[3] + window[1] as f64 * g[5],
            0.,
            g[5],
        ]
    }
    pub fn tile(&self, row: usize, col: usize) -> Tile {
        let left = self.source[0] + self.pitch * col as f64;
        let right = self.source[0] + self.pitch * (col + 1) as f64;
        let top = self.source[3] - self.pitch * row as f64;
        let bottom = self.source[3] - self.pitch * (row + 1) as f64;
        Tile {
            left,
            right,
            top,
            bottom,
            step: ((right - left) / 256.).max((top - bottom) / 256.),
        }
    }
    pub fn point(&self, row: usize, col: usize) -> [f64; 2] {
        let tile = self.tile(row / 256, col / 256);
        [tile.x(col % 256), tile.y(row % 256)]
    }
}

impl Tile {
    pub fn x(&self, col: usize) -> f64 {
        self.left + self.step * (col as f64 + 0.5)
    }
    pub fn y(&self, row: usize) -> f64 {
        self.top - self.step * (row as f64 + 0.5)
    }
    pub fn col(&self, x: f64) -> i64 {
        ((x - self.left) / self.step)
            .floor()
            .clamp(i32::MIN as f64, i32::MAX as f64) as i64
    }
    pub fn row(&self, y: f64) -> i64 {
        ((self.top - y) / self.step)
            .floor()
            .clamp(i32::MIN as f64, i32::MAX as f64) as i64
    }
    /// Inclusive pixel-center bounds, evaluated on a complete logical tile.
    pub fn rectangle(&self, b: [f64; 4]) -> Option<[usize; 4]> {
        let [left, bottom, right, top] = b;
        let mut c0 = self.col(left);
        if left > self.left + self.step * (c0 as f64 + 0.5) {
            c0 += 1;
        }
        let mut c1 = self.col(right);
        if right < self.left + self.step * (c1 as f64 + 0.5) {
            c1 -= 1;
        }
        let mut r0 = self.row(top);
        if top < self.top - self.step * (r0 as f64 + 0.5) {
            r0 += 1;
        }
        let mut r1 = self.row(bottom);
        if bottom > self.top - self.step * (r1 as f64 + 0.5) {
            r1 -= 1;
        }
        if c0 > 255 || c1 < 0 || r0 > 255 || r1 < 0 {
            return None;
        }
        let a = [
            c0.max(0) as usize,
            c1.min(255) as usize,
            r0.max(0) as usize,
            r1.min(255) as usize,
        ];
        (a[0] <= a[1] && a[2] <= a[3]).then_some(a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn partial_tiles_and_windows_preserve_global_centers() {
        let l = Layout::new(513, 257, [100., 2., 0., 1000., 0., -2.]).unwrap();
        let c = Contract {
            id: ID.into(),
            source_width: 513,
            source_height: 257,
            tile_size: 256,
            source_geo_transform: l.source,
            output_geo_transform: l.output,
        };
        let l = c.validate([255, 255, 258, 2]).unwrap();
        assert_eq!(l.point(256, 512), [1125., 487.]);
        assert_eq!(
            l.window_transform([255, 255, 258, 2]),
            [610., 2., 0., 490., 0., -2.]
        );
        assert_eq!(
            l.tile(1, 2).rectangle([1124., 486., 1126., 488.]),
            Some([0, 0, 0, 0])
        );
    }
    #[test]
    fn descriptor_and_unsafe_layout_refusals() {
        assert!(Layout::new(0, 1, [0., 1., 0., 0., 0., -1.]).is_err());
        assert!(Layout::new(1, 1, [0., 1., 0.1, 0., 0., -1.]).is_err());
        assert!(Layout::new(1, 1, [0., 1., 0., 0., 0., -2.]).is_err());
        assert!(Layout::new(usize::MAX, 1, [0., 1., 0., 0., 0., -1.]).is_err());
        let l = Layout::new(8, 9, [0., 1., 0., 9., 0., -1.]).unwrap();
        let mut c = Contract {
            id: ID.into(),
            source_width: 8,
            source_height: 9,
            tile_size: 256,
            source_geo_transform: l.source,
            output_geo_transform: l.output,
        };
        assert!(c.validate([8, 0, 1, 1]).is_err());
        c.output_geo_transform[1] = 2.;
        assert!(c.validate([0, 0, 1, 1]).is_err());
    }
    #[test]
    fn rounded_full_extent_is_derived_without_replacing_source_identity() {
        let raw = [1_000_000.125, 0.13, 0., 2_000_000.1, 0., -0.13];
        let l = Layout::new(513, 257, raw).unwrap();
        assert!(same(l.source, raw));
        assert_ne!(l.output[1].to_bits(), raw[1].to_bits());
        assert_eq!(l.output[1].to_bits(), 0x3fc0a3d70a3d614f);
    }
}
