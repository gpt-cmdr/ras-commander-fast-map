//! Crop-invariant AOI coverage. Hydraulic cell ownership still uses terrio.
use crate::{MapError, ownership};
use rayon::prelude::*;
use std::path::Path;
use wbvector::{Geometry, Ring};

pub const METHOD: &str = "global-terrain-lattice/1";
type Edge = ([f64; 2], [f64; 2]);

/// Additional planned parallel scanline scratch, checked before geometry reads.
pub fn scratch_bytes(path: Option<&Path>, threads: usize) -> Result<u64, MapError> {
    path.map_or(Ok(0), |p| {
        std::fs::metadata(p)?
            .len()
            .checked_mul(threads as u64)
            .ok_or_else(|| MapError::Budget("AOI worker scratch overflow".into()))
    })
}

fn edges(geometry: &Geometry, gt: [f64; 6]) -> Result<Vec<Edge>, MapError> {
    let mut result = Vec::new();
    let mut ring = |ring: &Ring| -> Result<(), MapError> {
        let points: Vec<_> = ring
            .coords()
            .iter()
            .map(|p| [(p.x - gt[0]) / gt[1], (p.y - gt[3]) / -gt[5]])
            .collect();
        if points
            .iter()
            .flatten()
            .any(|v| !v.is_finite() || v.abs() > (1_u64 << 52) as f64)
        {
            return Err(MapError::Unsupported(
                "AOI exceeds exact pixel-index range".into(),
            ));
        }
        for i in 0..points.len() {
            let (mut a, mut b) = (points[i], points[(i + 1) % points.len()]);
            if a[1] == b[1] {
                continue;
            }
            if a[1] > b[1] {
                std::mem::swap(&mut a, &mut b);
            }
            result.push((a, b));
        }
        Ok(())
    };
    match geometry {
        Geometry::Polygon {
            exterior,
            interiors,
        } => {
            ring(exterior)?;
            for hole in interiors {
                ring(hole)?;
            }
        }
        Geometry::MultiPolygon(parts) => {
            for (exterior, interiors) in parts {
                ring(exterior)?;
                for hole in interiors {
                    ring(hole)?;
                }
            }
        }
        _ => {
            return Err(MapError::Invalid(
                "AOI requires Polygon/MultiPolygon".into(),
            ));
        }
    }
    Ok(result)
}

fn burn(edges: &[Edge], window: [usize; 4]) -> Result<Vec<u32>, MapError> {
    let [col, row, width, height] = window;
    let count = width
        .checked_mul(height)
        .ok_or_else(|| MapError::Budget("AOI size overflow".into()))?;
    if col
        .checked_add(width)
        .is_none_or(|n| n as u64 > (1_u64 << 52))
        || row
            .checked_add(height)
            .is_none_or(|n| n as u64 > (1_u64 << 52))
    {
        return Err(MapError::Unsupported(
            "AOI window exceeds exact pixel-index range".into(),
        ));
    }
    let mut mask = vec![0; count];
    if width == 0 {
        return Ok(mask);
    }
    mask.par_chunks_mut(width)
        .enumerate()
        .for_each_init(Vec::new, |crossings, (r, line)| {
            // Both samples and edge arithmetic use GLOBAL coordinates, never a crop origin.
            let y = -((row + r) as f64 + 0.5);
            crossings.clear();
            for &(a, b) in edges {
                // Lower y inclusive, upper y exclusive; horizontal edges do not cross.
                if a[1] <= y && y < b[1] {
                    crossings.push(a[0] + ((y - a[1]) / (b[1] - a[1])) * (b[0] - a[0]));
                }
            }
            crossings.sort_unstable_by(f64::total_cmp);
            for pair in crossings.chunks_exact(2) {
                // Left inclusive, right exclusive, in source pixel coordinates.
                let start = (pair[0] - 0.5)
                    .ceil()
                    .clamp(col as f64, (col + width) as f64) as usize;
                let stop = (pair[1] - 0.5)
                    .ceil()
                    .clamp(col as f64, (col + width) as f64) as usize;
                line[start - col..stop - col].fill(1);
            }
        });
    Ok(mask)
}

pub fn mask(path: &Path, gt: [f64; 6], window: [usize; 4]) -> Result<Vec<u32>, MapError> {
    if !gt.iter().all(|v| v.is_finite()) || gt[1] <= 0. || gt[5] >= 0. || gt[2] != 0. || gt[4] != 0.
    {
        return Err(MapError::Invalid(
            "AOI needs a finite north-up terrain transform".into(),
        ));
    }
    let layer = ownership::load(path, 1, true)?;
    let geometry = layer.features[0]
        .geometry
        .as_ref()
        .ok_or_else(|| MapError::Invalid("empty AOI".into()))?;
    burn(&edges(geometry, gt)?, window)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wbvector::Coord;

    fn polygon(points: &[(f64, f64)]) -> Geometry {
        Geometry::polygon(
            points.iter().map(|&(x, y)| Coord::xy(x, y)).collect(),
            vec![],
        )
    }

    #[test]
    fn half_open_center_edges_and_reversed_rings() {
        let p = [(1.5, -4.5), (5.5, -4.5), (5.5, -1.5), (1.5, -1.5)];
        let gt = [0., 1., 0., 0., 0., -1.];
        let e = edges(&polygon(&p), gt).unwrap();
        let actual = burn(&e, [0, 0, 8, 8]).unwrap();
        for y in 0..8 {
            for x in 0..8 {
                assert_eq!(
                    actual[y * 8 + x],
                    u32::from((1..5).contains(&x) && (2..5).contains(&y))
                );
            }
        }
        let reverse: Vec<_> = p.into_iter().rev().collect();
        assert_eq!(
            actual,
            burn(&edges(&polygon(&reverse), gt).unwrap(), [0, 0, 8, 8]).unwrap()
        );
    }

    #[test]
    fn every_integer_crop_matches_full_mask_at_fractional_origin() {
        let gt = [
            6628329.947181307,
            28.8452448838947,
            0.,
            1969956.489543507,
            0.,
            -28.8452448838947,
        ];
        let to_world = |x: f64, y: f64| (gt[0] + x * gt[1], gt[3] - y * gt[1]);
        let shell = [(10.5, 12.5), (382.5, 12.5), (382.5, 331.5), (10.5, 331.5)];
        let mut p = polygon(&shell.map(|(x, y)| to_world(x, y)));
        if let Geometry::Polygon { interiors, .. } = &mut p {
            interiors.push(Ring::new(
                [(40.5, 50.5), (140.5, 61.5), (121.5, 200.5)]
                    .map(|(x, y)| {
                        let (x, y) = to_world(x, y);
                        Coord::xy(x, y)
                    })
                    .into(),
            ));
        }
        let e = edges(&p, gt).unwrap();
        let full = burn(&e, [0, 0, 394, 345]).unwrap();
        for (col, row, width, height) in [
            (10, 13, 373, 319),
            (11, 12, 100, 150),
            (121, 50, 4, 250),
            (0, 0, 394, 345),
        ] {
            let part = burn(&e, [col, row, width, height]).unwrap();
            for r in 0..height {
                assert_eq!(
                    &part[r * width..(r + 1) * width],
                    &full[(row + r) * 394 + col..(row + r) * 394 + col + width]
                );
            }
        }
    }

    #[test]
    #[ignore = "explicit release-mode timing observation, not a speed assertion"]
    fn mask_kernel_timing() {
        use std::time::Instant;
        use terrio::ops::rasterize::Grid;
        use wbvector::{Feature, FieldDef, FieldType, FieldValue, Layer};
        let gt = [
            6628329.947181307,
            28.8452448838947,
            0.,
            1969956.489543507,
            0.,
            -28.8452448838947,
        ];
        let points = [
            (52.123, 63.321),
            (1900.987, 63.321),
            (1900.987, 1891.432),
            (52.123, 1891.432),
        ];
        let geometry = polygon(&points.map(|(x, y)| (gt[0] + x * gt[1], gt[3] - y * gt[1])));
        let edges = edges(&geometry, gt).unwrap();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(4)
            .build()
            .unwrap();
        let make_layer = || {
            let mut layer = Layer::new("timing");
            layer
                .schema
                .add_field(FieldDef::new("id", FieldType::Integer));
            layer.features.push(Feature {
                fid: 0,
                geometry: Some(geometry.clone()),
                attributes: vec![FieldValue::Integer(1)],
            });
            layer
        };
        let grid = Grid {
            geo_transform: gt,
            cols: 2048,
            rows: 2048,
            crs: Default::default(),
        };
        let expected = pool
            .install(|| ownership::burn(make_layer(), grid.clone()))
            .unwrap();
        let actual = pool.install(|| burn(&edges, [0, 0, 2048, 2048])).unwrap();
        assert_eq!(actual, expected);
        let tick = Instant::now();
        for _ in 0..20 {
            std::hint::black_box(
                pool.install(|| ownership::burn(make_layer(), grid.clone()))
                    .unwrap(),
            );
        }
        let old = tick.elapsed().as_secs_f64();
        let tick = Instant::now();
        for _ in 0..20 {
            std::hint::black_box(pool.install(|| burn(&edges, [0, 0, 2048, 2048])).unwrap());
        }
        eprintln!(
            "2048x2048 AOI, 4 workers, 20 warm iterations: terrio={old:.6}s global_lattice={:.6}s",
            tick.elapsed().as_secs_f64()
        );
    }
}
