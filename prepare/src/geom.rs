//! GEOS helpers that make the same GEOS calls shapely 2.1.2 makes.
//!
//! Coordinates are 2-D (`XY`), as shapely builds them from N x 2 arrays.

use crate::error::{Error, Result};
use geos::{CoordSeq, CoordType, Geom, Geometry, GeometryTypes};
use rstar::{AABB, RTree, RTreeObject};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Envelope {
    pub min: [f64; 2],
    pub max: [f64; 2],
}

fn seq(points: &[[f64; 2]]) -> Result<CoordSeq> {
    let flat: Vec<f64> = points.iter().flat_map(|p| [p[0], p[1]]).collect();
    Ok(CoordSeq::new_from_buffer(
        &flat,
        points.len(),
        CoordType::XY,
    )?)
}

/// `shapely.linearrings` closing rule: append the first point when the ring
/// is open or has only three points.
fn closed(ring: &[[f64; 2]]) -> Vec<[f64; 2]> {
    let mut r = ring.to_vec();
    if !r.is_empty() && (r.first() != r.last() || r.len() == 3) {
        r.push(r[0]);
    }
    r
}

/// `Polygon(shell, holes)`; the first ring is the shell.
pub fn polygon(rings: &[Vec<[f64; 2]>]) -> Result<Geometry> {
    let mut it = rings.iter();
    let shell = it
        .next()
        .ok_or_else(|| Error::invalid("polygon without a shell"))?;
    let shell = Geometry::create_linear_ring(seq(&closed(shell))?)?;
    let holes = it
        .map(|h| Ok(Geometry::create_linear_ring(seq(&closed(h))?)?))
        .collect::<Result<Vec<_>>>()?;
    Ok(Geometry::create_polygon(shell, holes)?)
}

pub fn line_string(points: &[[f64; 2]]) -> Result<Geometry> {
    Ok(Geometry::create_line_string(seq(points)?)?)
}

/// `MultiPoint(coords)`.
pub fn multipoint(points: &[[f64; 2]]) -> Result<Geometry> {
    let pts = points
        .iter()
        .map(|p| Ok(Geometry::create_point(seq(&[*p])?)?))
        .collect::<Result<Vec<_>>>()?;
    Ok(Geometry::create_multipoint(pts)?)
}

/// `shapely.box(xmin, ymin, xmax, ymax)` (counter-clockwise from the lower right).
pub fn rect(xmin: f64, ymin: f64, xmax: f64, ymax: f64) -> Result<Geometry> {
    polygon(&[vec![
        [xmax, ymin],
        [xmax, ymax],
        [xmin, ymax],
        [xmin, ymin],
        [xmax, ymin],
    ]])
}

pub fn clone(g: &impl Geom) -> Result<Geometry> {
    Ok(Geom::clone(g)?)
}

pub fn is_polygon(g: &impl Geom) -> Result<bool> {
    Ok(g.geometry_type()? == GeometryTypes::Polygon)
}

/// `shapely.get_parts`: the members of a collection, or the geometry itself.
pub fn parts(g: &impl Geom) -> Result<Vec<Geometry>> {
    match g.geometry_type()? {
        GeometryTypes::MultiPolygon
        | GeometryTypes::MultiLineString
        | GeometryTypes::MultiPoint
        | GeometryTypes::GeometryCollection => {
            let n = g.get_num_geometries()?;
            (0..n).map(|i| clone(&g.get_geometry_n(i)?)).collect()
        }
        _ => Ok(vec![clone(g)?]),
    }
}

/// `shapely.union_all(list)`: GEOSUnaryUnion of a GEOMETRYCOLLECTION of copies.
pub fn union_all(geoms: Vec<Geometry>) -> Result<Geometry> {
    Ok(Geometry::create_geometry_collection(geoms)?.unary_union()?)
}

fn coords(g: &impl Geom) -> Result<Vec<[f64; 2]>> {
    let s = g.get_coord_seq()?;
    let b = s.as_buffer(Some(CoordType::XY))?;
    Ok(b.as_chunks::<2>().0.iter().map(|c| [c[0], c[1]]).collect())
}

/// Rings of a Polygon: exterior then interiors, as stored.
pub fn rings(g: &impl Geom) -> Result<Vec<Vec<[f64; 2]>>> {
    if g.is_empty()? {
        return Ok(Vec::new());
    }
    let mut out = vec![coords(&g.get_exterior_ring()?)?];
    for i in 0..g.get_num_interior_rings()? {
        out.push(coords(&g.get_interior_ring_n(i)?)?);
    }
    Ok(out)
}

/// Polygon parts of a Polygon or MultiPolygon, each as rings.
pub fn polygon_rings(g: &impl Geom) -> Result<Vec<Vec<Vec<[f64; 2]>>>> {
    match g.geometry_type()? {
        GeometryTypes::Polygon => Ok(vec![rings(g)?]),
        GeometryTypes::MultiPolygon => parts(g)?.iter().map(rings).collect(),
        t => Err(Error::invalid(format!("expected a polygon, got {t:?}"))),
    }
}

/// Exact `shapely.bounds` (min and max of the stored coordinates).
pub fn envelope(g: &impl Geom) -> Result<Envelope> {
    let mut e = Envelope {
        min: [f64::INFINITY; 2],
        max: [f64::NEG_INFINITY; 2],
    };
    for part in polygon_rings(g)? {
        for ring in part {
            for p in ring {
                e.min[0] = e.min[0].min(p[0]);
                e.min[1] = e.min[1].min(p[1]);
                e.max[0] = e.max[0].max(p[0]);
                e.max[1] = e.max[1].max(p[1]);
            }
        }
    }
    Ok(e)
}

/// Total stored coordinates of a polygon (`shapely.count_coordinates`).
pub fn count_coordinates(g: &impl Geom) -> Result<usize> {
    Ok(polygon_rings(g)?.iter().flatten().map(Vec::len).sum())
}

/// `shapely.to_wkb(g, output_dimension=2, byte_order=1, include_srid=False)`
/// of a Polygon or MultiPolygon.
pub fn wkb(g: &impl Geom) -> Result<Vec<u8>> {
    fn polygon_body(out: &mut Vec<u8>, rings: &[Vec<[f64; 2]>]) {
        out.push(1);
        out.extend_from_slice(&3u32.to_le_bytes());
        out.extend_from_slice(&(rings.len() as u32).to_le_bytes());
        for r in rings {
            out.extend_from_slice(&(r.len() as u32).to_le_bytes());
            for p in r {
                out.extend_from_slice(&p[0].to_le_bytes());
                out.extend_from_slice(&p[1].to_le_bytes());
            }
        }
    }
    let mut out = Vec::new();
    match g.geometry_type()? {
        GeometryTypes::Polygon => polygon_body(&mut out, &rings(g)?),
        GeometryTypes::MultiPolygon => {
            let parts = polygon_rings(g)?;
            out.push(1);
            out.extend_from_slice(&6u32.to_le_bytes());
            out.extend_from_slice(&(parts.len() as u32).to_le_bytes());
            for p in &parts {
                polygon_body(&mut out, p);
            }
        }
        t => return Err(Error::invalid(format!("WKB of {t:?} is not used"))),
    }
    Ok(out)
}

/// Polygon or MultiPolygon from little- or big-endian 2-D WKB.
pub fn from_wkb(bytes: &[u8]) -> Result<Geometry> {
    Ok(Geometry::new_from_wkb(bytes)?)
}

struct Item {
    index: usize,
    env: AABB<[f64; 2]>,
}

impl RTreeObject for Item {
    type Envelope = AABB<[f64; 2]>;
    fn envelope(&self) -> Self::Envelope {
        self.env
    }
}

/// Envelope index over a geometry list (shapely's STRtree role). Candidate
/// envelopes intersect as closed boxes; the predicate decides membership.
pub struct Index {
    tree: RTree<Item>,
}

impl Index {
    pub fn new(envs: &[Envelope]) -> Self {
        let items = envs
            .iter()
            .enumerate()
            .map(|(index, e)| Item {
                index,
                env: AABB::from_corners(e.min, e.max),
            })
            .collect();
        Self {
            tree: RTree::bulk_load(items),
        }
    }

    /// Indices whose envelope intersects `env` (closed boxes), unordered.
    pub fn envelope_hits(&self, env: &Envelope) -> Vec<usize> {
        if !(env.min[0] <= env.max[0] && env.min[1] <= env.max[1]) {
            return Vec::new();
        }
        self.tree
            .locate_in_envelope_intersecting(AABB::from_corners(env.min, env.max))
            .map(|i| i.index)
            .collect()
    }

    /// `tree.query(g, predicate="intersects")`, unordered: prepared `g`
    /// intersects each candidate.
    pub fn query_intersects<'a>(
        &self,
        g: &Geometry,
        geometry: impl Fn(usize) -> &'a Geometry,
    ) -> Result<Vec<usize>> {
        let env = envelope_any(g)?;
        let prepared = g.to_prepared_geom()?;
        let mut out = Vec::new();
        for i in self.envelope_hits(&env) {
            if prepared.intersects(geometry(i))? {
                out.push(i);
            }
        }
        Ok(out)
    }
}

/// Envelope of any geometry through GEOS (for queries with non-polygon input).
pub fn envelope_any(g: &impl Geom) -> Result<Envelope> {
    if g.is_empty()? {
        return Ok(Envelope {
            min: [f64::INFINITY; 2],
            max: [f64::NEG_INFINITY; 2],
        });
    }
    Ok(Envelope {
        min: [g.get_x_min()?, g.get_y_min()?],
        max: [g.get_x_max()?, g.get_y_max()?],
    })
}
