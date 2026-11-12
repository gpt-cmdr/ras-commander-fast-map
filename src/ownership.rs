//! Terrio ownership on an exact admitted grid. Input polygons are validated by the caller.
use crate::MapError;
use std::{
    fs::File,
    io::{BufReader, Read},
    path::Path,
};
use terrio::ops::rasterize::{Grid, RasterizeOptions, rasterize};
use wbraster::{DataType, raster::RasterData};
use wbvector::{Feature, FieldDef, FieldType, FieldValue, Geometry, Layer};

fn invalid(message: &str) -> MapError {
    MapError::Invalid(message.into())
}

// The transport deliberately accepts only little-endian 2D Polygon/MultiPolygon WKB.
// Check counts against remaining bytes before the general WKB decoder allocates vectors.
fn check_wkb(data: &[u8], multipart: bool) -> Result<usize, MapError> {
    if data.len() < 9 || data[0] != 1 {
        return Err(invalid("invalid ownership WKB header"));
    }
    let kind = u32::from_le_bytes(data[1..5].try_into().unwrap());
    let n = u32::from_le_bytes(data[5..9].try_into().unwrap()) as usize;
    let mut pos = 9_usize;
    match kind {
        3 => {
            if n == 0 || n > (data.len() - pos) / 4 {
                return Err(invalid("invalid polygon ring count"));
            }
            for _ in 0..n {
                let raw = data
                    .get(pos..pos + 4)
                    .ok_or_else(|| invalid("truncated ring"))?;
                let points = u32::from_le_bytes(raw.try_into().unwrap()) as usize;
                pos += 4;
                if points < 4 || points > (data.len() - pos) / 16 {
                    return Err(invalid("invalid polygon point count"));
                }
                for pair in data[pos..pos + points * 16].as_chunks::<8>().0 {
                    if !f64::from_le_bytes(*pair).is_finite() {
                        return Err(invalid("nonfinite polygon coordinate"));
                    }
                }
                pos += points * 16;
            }
        }
        6 if multipart => {
            if n == 0 || n > (data.len() - pos) / 9 {
                return Err(invalid("invalid multipolygon count"));
            }
            for _ in 0..n {
                pos += check_wkb(&data[pos..], false)?;
            }
        }
        _ => {
            return Err(invalid(
                "ownership requires 2D Polygons; AOI may be MultiPolygon",
            ));
        }
    }
    Ok(pos)
}

pub fn load(path: &Path, count: usize, multipart: bool) -> Result<Layer, MapError> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut remaining = std::fs::metadata(path)?.len();
    let mut layer = Layer::new("ownership");
    layer
        .schema
        .add_field(FieldDef::new("id", FieldType::Integer));
    for i in 0..count {
        if remaining < 4 {
            return Err(invalid("truncated geometry framing"));
        }
        let mut header = [0; 4];
        reader.read_exact(&mut header)?;
        remaining -= 4;
        let size = u32::from_le_bytes(header) as usize;
        if size as u64 > remaining {
            return Err(invalid("truncated geometry WKB"));
        }
        let mut raw = vec![0; size];
        reader.read_exact(&mut raw)?;
        remaining -= size as u64;
        if check_wkb(&raw, multipart)? != size {
            return Err(invalid("trailing WKB bytes"));
        }
        let g = Geometry::from_wkb(&raw).map_err(|e| MapError::Invalid(e.to_string()))?;
        layer.features.push(Feature {
            fid: i as u64,
            geometry: Some(g),
            attributes: vec![FieldValue::Integer(i as i64 + 1)],
        });
    }
    let mut extra = [0];
    if remaining != 0 || reader.read(&mut extra)? != 0 {
        return Err(invalid("geometry count/length mismatch"));
    }
    Ok(layer)
}

pub fn burn(layer: Layer, grid: Grid) -> Result<Vec<u32>, MapError> {
    let options = RasterizeOptions {
        attribute: Some("id".into()),
        grid: Some(grid),
        data_type: Some(DataType::U32),
        ..Default::default()
    };
    let result = crate::terrio_pool::call(|| rasterize(&[layer], &options))?
        .map_err(|e| MapError::Invalid(e.to_string()))?;
    match result.raster.data {
        RasterData::U32(values) => Ok(values),
        _ => Err(MapError::Internal("unexpected ownership data type".into())),
    }
}

/// Physical claims on complete source tiles. Window clipping happens only at
/// delivery; neither scanline endpoints nor rectangle seeds use a crop origin.
pub(crate) fn burn_stored(
    layer: Layer,
    layout: &crate::stored_grid::Layout,
    window: [usize; 4],
    squares: &[Option<[f64; 4]>],
) -> Result<Vec<u32>, MapError> {
    use wbvector::Ring;
    layout.validate_window(window)?;
    if layer.features.len() != squares.len() {
        return Err(invalid(
            "stored ownership square descriptors do not match polygons",
        ));
    }
    let [col, row, width, height] = window;
    let count = width
        .checked_mul(height)
        .ok_or_else(|| invalid("stored ownership size overflow"))?;
    let mut output = vec![0; count];
    for (index, feature) in layer.features.iter().enumerate() {
        let Some(Geometry::Polygon {
            exterior,
            interiors,
        }) = &feature.geometry
        else {
            return Err(invalid("stored ownership requires physical polygons"));
        };
        let rings: Vec<&Ring> = std::iter::once(exterior).chain(interiors.iter()).collect();
        let bounds = rings.iter().flat_map(|r| r.coords()).fold(
            [
                f64::INFINITY,
                f64::INFINITY,
                f64::NEG_INFINITY,
                f64::NEG_INFINITY,
            ],
            |b, p| [b[0].min(p.x), b[1].min(p.y), b[2].max(p.x), b[3].max(p.y)],
        );
        let bounds = squares[index].unwrap_or(bounds);
        let label = (index + 1) as u32;
        for tr in row / 256..=(row + height - 1) / 256 {
            for tc in col / 256..=(col + width - 1) / 256 {
                let tile = layout.tile(tr, tc);
                if bounds[0] > tile.right
                    || bounds[2] < tile.left
                    || bounds[1] > tile.top
                    || bounds[3] < tile.bottom
                {
                    continue;
                }
                let mut claim = |lr: usize, lc: usize| {
                    let (r, c) = (tr * 256 + lr, tc * 256 + lc);
                    if r >= row && r < row + height && c >= col && c < col + width {
                        let value = &mut output[(r - row) * width + c - col];
                        if *value == 0 {
                            *value = label;
                        }
                    }
                };
                if let Some(square) = squares[index] {
                    if let Some([c0, c1, r0, r1]) = tile.rectangle(square) {
                        for r in r0..=r1 {
                            for c in c0..=c1 {
                                claim(r, c);
                            }
                        }
                    }
                } else {
                    let r0 = tile.row(bounds[3].min(tile.top)).max(0) as usize;
                    let r1 = tile.row(bounds[1].max(tile.bottom)).clamp(0, 255) as usize;
                    let mut crossing = Vec::new();
                    for r in r0..=r1 {
                        crossing.clear();
                        for ring in &rings {
                            stored_intersections(ring, &tile, r, &mut crossing);
                        }
                        crossing.sort_by(f64::total_cmp);
                        if crossing.len() % 2 == 1 {
                            crossing.push(tile.right);
                        }
                        for pair in crossing.as_chunks::<2>().0 {
                            if pair[1] <= tile.left {
                                continue;
                            }
                            let mut c0 = if pair[0] < tile.left {
                                0
                            } else {
                                tile.col(pair[0])
                            };
                            if pair[0] >= tile.left
                                && pair[0] > tile.left + tile.step * (c0 as f64 + 0.5)
                            {
                                c0 += 1;
                            }
                            let mut c1 = if pair[1] >= tile.right {
                                255
                            } else {
                                tile.col(pair[1])
                            };
                            if pair[1] < tile.right
                                && pair[1] < tile.left + tile.step * (c1 as f64 + 0.5)
                            {
                                c1 -= 1;
                            }
                            for c in c0.max(0)..=c1.min(255) {
                                claim(r, c as usize);
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(output)
}

// Scalar row evaluation keeps at most one intersection per polygon edge live.
// Endpoint perturbation is part of the selected physical-cell scan conversion,
// distinct from the unchanged half-open delivery AOI rule.
fn stored_intersections(
    ring: &wbvector::Ring,
    tile: &crate::stored_grid::Tile,
    row: usize,
    out: &mut Vec<f64>,
) {
    let all = ring.coords();
    let n = all
        .len()
        .saturating_sub(usize::from(all.first() == all.last()));
    if n < 3 {
        return;
    }
    let vertex = |i: usize, out: &mut Vec<f64>| {
        let p = &all[i];
        if p.x >= tile.right {
            return;
        }
        let before = all[(i + n - 1) % n].y - p.y;
        let after = all[(i + 1) % n].y - p.y;
        let between = if after == 0. {
            before > 0.
        } else if before == 0. {
            after > 0.
        } else {
            before.signum() != after.signum()
        };
        if between {
            if !out.contains(&p.x) {
                out.push(p.x);
            }
        } else if let Some(i) = out.iter().position(|&x| x == p.x) {
            out.remove(i);
        }
    };
    for i in 0..n {
        let j = (i + 1) % n;
        let (low, high) = if all[i].y < all[j].y { (i, j) } else { (j, i) };
        let (a, b) = (&all[low], &all[high]);
        let dy = b.y - a.y;
        if dy == 0. || a.y >= tile.top || b.y <= tile.bottom {
            continue;
        }
        let first = tile.row(b.y).max(0);
        let last = tile.row(a.y).min(255);
        if (row as i64) < first || (row as i64) > last {
            continue;
        }
        let slope = (b.x - a.x) / dy;
        let seed_y = tile.y(first as usize);
        let mut lower = a.y;
        let mut upper = b.y;
        let shift = tile.step * 1e-5;
        if upper == seed_y {
            upper -= shift;
        }
        if lower == seed_y {
            lower -= shift;
        }
        let y = tile.y(row);
        if row as i64 == first {
            if upper == y {
                vertex(high, out);
            } else if lower == y {
                vertex(low, out);
            } else if upper > y && lower < y {
                let x = a.x + (y - lower) * slope;
                if x < tile.right {
                    out.push(x);
                }
            }
            continue;
        }
        if lower == tile.y(last as usize) {
            lower -= shift;
        }
        if row as i64 == last && lower == y {
            vertex(low, out);
        } else if (row as i64) != last || lower < y {
            let x = a.x + (y - lower) * slope;
            if x < tile.right {
                out.push(x);
            }
        }
    }
}

pub fn burn_ring(points: &[[f64; 2]], grid: Grid) -> Result<Vec<u32>, MapError> {
    if points.len() < 3 {
        return Err(invalid("carry polygon needs three points"));
    }
    let closed = points.first() == points.last();
    let mut wkb = vec![1];
    wkb.extend(3_u32.to_le_bytes());
    wkb.extend(1_u32.to_le_bytes());
    wkb.extend(((points.len() + usize::from(!closed)) as u32).to_le_bytes());
    for point in points.iter().chain((!closed).then_some(&points[0])) {
        for value in point {
            wkb.extend(value.to_le_bytes());
        }
    }
    let geometry = Geometry::from_wkb(&wkb).map_err(|e| invalid(&e.to_string()))?;
    let mut layer = Layer::new("carry polygon");
    layer
        .schema
        .add_field(FieldDef::new("id", FieldType::Integer));
    layer.features.push(Feature {
        fid: 0,
        geometry: Some(geometry),
        attributes: vec![FieldValue::Integer(1)],
    });
    burn(layer, grid)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_counts_rejected_before_decoder_allocation() {
        let mut data = vec![1, 3, 0, 0, 0];
        data.extend(u32::MAX.to_le_bytes());
        assert!(check_wkb(&data, false).is_err());
        assert!(check_wkb(&[0; 16], false).is_err());
    }
    fn layer(rings: &[Vec<[f64; 2]>], count: usize) -> Layer {
        let mut wkb = vec![1];
        wkb.extend(3u32.to_le_bytes());
        wkb.extend((rings.len() as u32).to_le_bytes());
        for ring in rings {
            wkb.extend((ring.len() as u32).to_le_bytes());
            for p in ring {
                for v in p {
                    wkb.extend(v.to_le_bytes());
                }
            }
        }
        let g = Geometry::from_wkb(&wkb).unwrap();
        let mut l = Layer::new("synthetic");
        for i in 0..count {
            l.features.push(Feature {
                fid: i as u64,
                geometry: Some(g.clone()),
                attributes: vec![],
            });
        }
        l
    }
    #[test]
    fn stored_rectangle_claims_center_ties_once_and_crop_keeps_global_tile() {
        let l = crate::stored_grid::Layout::new(513, 257, [0., 1., 0., 257., 0., -1.]).unwrap();
        let ring = vec![
            [254.5, 0.5],
            [254.5, 2.5],
            [257.5, 2.5],
            [257.5, 0.5],
            [254.5, 0.5],
        ];
        let full = burn_stored(
            layer(std::slice::from_ref(&ring), 2),
            &l,
            [0, 0, 513, 257],
            &[Some([254.5, 0.5, 257.5, 2.5]); 2],
        )
        .unwrap();
        let crop = burn_stored(
            layer(&[ring], 2),
            &l,
            [255, 255, 3, 2],
            &[Some([254.5, 0.5, 257.5, 2.5]); 2],
        )
        .unwrap();
        assert_eq!(crop, vec![1; 6]);
        for r in 0..2 {
            assert_eq!(
                &crop[r * 3..r * 3 + 3],
                &full[(r + 255) * 513 + 255..(r + 255) * 513 + 258]
            );
        }
        assert_eq!(full.iter().filter(|&&v| v != 0).count(), 12);
    }
    #[test]
    fn stored_polygon_scanlines_keep_holes_and_first_claim() {
        let l = crate::stored_grid::Layout::new(8, 8, [0., 1., 0., 8., 0., -1.]).unwrap();
        let exterior = vec![[0., 0.], [0., 8.], [8., 8.], [8., 0.], [0., 0.]];
        let hole = vec![[2., 2.], [6., 2.], [6., 6.], [2., 6.], [2., 2.]];
        let values =
            burn_stored(layer(&[exterior, hole], 2), &l, [0, 0, 8, 8], &[None, None]).unwrap();
        for r in 0..8 {
            for c in 0..8 {
                assert_eq!(
                    values[r * 8 + c],
                    u32::from(!((2..6).contains(&r) && (2..6).contains(&c)))
                );
            }
        }
    }
    #[test]
    fn stored_polygon_center_endpoint_rule_is_distinct_from_square_rectangle() {
        let l = crate::stored_grid::Layout::new(4, 4, [0., 1., 0., 4., 0., -1.]).unwrap();
        let ring = vec![[0.5, 0.5], [0.5, 2.5], [1.5, 2.5], [1.5, 0.5], [0.5, 0.5]];
        let generic = burn_stored(
            layer(std::slice::from_ref(&ring), 1),
            &l,
            [0, 0, 4, 4],
            &[None],
        )
        .unwrap();
        assert_eq!(
            generic,
            vec![0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0]
        );
        let rectangle = burn_stored(
            layer(std::slice::from_ref(&ring), 1),
            &l,
            [0, 0, 4, 4],
            &[Some([0.5, 0.5, 1.5, 2.5])],
        )
        .unwrap();
        assert_eq!(
            rectangle,
            vec![0, 0, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0]
        );
        let reversed: Vec<_> = ring.into_iter().rev().collect();
        assert_eq!(
            generic,
            burn_stored(layer(&[reversed], 1), &l, [0, 0, 4, 4], &[None]).unwrap()
        );
    }
}
