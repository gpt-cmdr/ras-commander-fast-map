//! Strict identification of a terrain CRS that terrio reads without an EPSG code.
//!
//! HEC-RAS writes terrain GeoTIFFs whose projected CRS is user-defined in the GeoKeys
//! (ProjectedCSTypeGeoKey 32767, projection method, parameters, linear unit) on an EPSG
//! geographic CRS. terrio reads these as GDAL does: a `PROJCS` WKT whose root has no authority,
//! so `SpatialRef::epsg` is `None`. GDAL and terrio deliberately do not search a registry for an
//! equivalent code; this module does not search either. It only answers one closed question:
//! is this terrain CRS, parameter for parameter, the EPSG CRS the request already declares?
//!
//! The comparison is against terrio's own definition of that EPSG code
//! (`terrio::crs::SpatialRef::from_epsg`) and is strict:
//!
//! - both CRSs are WKT 1 `PROJCS`; the terrain's root carries no authority;
//! - the geographic CRS and datum carry EPSG codes and both are equal to the reference's
//!   (NAD83 4269/6269 is not NAD83(HARN) 4152/6152 or NAD83(2011) 6318/1116);
//! - ellipsoid axes, prime meridian and angular unit are equal within tight relative bounds;
//! - the projection method name is equal (case and punctuation ignored);
//! - the parameter names are the same set, without duplicates, and every value agrees: angles
//!   within 1e-9 degree, false easting/northing within 1 mm, scale factors within 1e-10;
//!   any parameter this rule does not classify is refused;
//! - for the two-parallel conics (Lambert Conformal Conic 2SP, Albers), whose formulas are
//!   symmetric in the two standard parallels, the parallels may be given in either order
//!   (HEC-RAS writes Brays Bayou's EPSG:2278 terrain with them swapped); the order found is
//!   recorded, and each value must still agree within the angle tolerance;
//! - the linear unit agrees within 1e-12 relative (US survey foot 0.3048006096 is not the
//!   international foot 0.3048), and with the same EPSG unit code where both declare one;
//! - axes, when declared, are easting then northing in both;
//! - any other node (TOWGS84, EXTENSION, a root AUTHORITY) the reference does not carry
//!   identically is refused.
//!
//! A successful match is recorded as [`EpsgParameterMatch`] in reports, so a receipt shows how
//! the EPSG code was identified.

use serde::Serialize;

/// Rule identifier written into reports.
pub const RULE: &str = "fast-map-projected-epsg-parameter-match/1";

/// Angular parameter tolerance, degrees (about 0.1 mm on the ground).
const ANGLE_TOLERANCE_DEG: f64 = 1e-9;
/// False easting/northing tolerance, metres.
const LINEAR_TOLERANCE_M: f64 = 1e-3;
/// Scale factor tolerance (absolute).
const SCALE_TOLERANCE: f64 = 1e-10;
/// Relative tolerance of unit factors and ellipsoid axes.
const UNIT_RELATIVE_TOLERANCE: f64 = 1e-12;
/// Relative tolerance of the inverse flattening (GDAL may derive it from the axes).
const INVERSE_FLATTENING_RELATIVE_TOLERANCE: f64 = 1e-10;

/// Projection methods symmetric in their two standard parallels (normalized names).
const TWO_PARALLEL_CONICS: [&str; 2] = ["lambertconformalconic2sp", "albersconicequalarea"];

const MAX_WKT_LEN: usize = 64 * 1024;
const MAX_DEPTH: usize = 32;

/// One compared projection parameter.
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct ParameterCheck {
    pub name: String,
    pub terrain: f64,
    pub reference: f64,
    /// `angle_degrees`, `linear_metres` or `scale`.
    pub kind: &'static str,
    /// Difference in the unit of `kind` (degrees, metres, or unitless).
    pub difference: f64,
    pub tolerance: f64,
}

/// How a terrain CRS without an EPSG code was identified as the request's EPSG CRS.
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct EpsgParameterMatch {
    pub rule: &'static str,
    pub epsg: u32,
    pub terrain_crs_name: String,
    pub reference_crs_name: String,
    pub geographic_epsg: u32,
    pub datum_epsg: u32,
    pub projection: String,
    pub linear_unit: String,
    pub linear_unit_metres: f64,
    pub linear_unit_epsg: Option<u32>,
    pub parameters: Vec<ParameterCheck>,
    /// The terrain gives a two-parallel conic's standard parallels in the reverse of the
    /// reference's order (each `ParameterCheck` then compares against the other parallel).
    pub standard_parallels_swapped: bool,
    /// FNV-1a 64 of terrio's reference WKT for `epsg`.
    pub reference_wkt_fnv1a64: String,
    /// EPSG registry version terrio carries.
    pub epsg_registry: &'static str,
}

/// Identify `terrain_wkt` as EPSG `epsg`, or say why not.
pub fn match_projected_epsg(terrain_wkt: &str, epsg: u32) -> Result<EpsgParameterMatch, String> {
    let reference_wkt = terrio::crs::SpatialRef::from_epsg(epsg)
        .ok()
        .and_then(|s| s.to_wkt())
        .ok_or_else(|| format!("terrio has no WKT for EPSG:{epsg}"))?;
    match_against(terrain_wkt, &reference_wkt, epsg)
}

fn match_against(
    terrain_wkt: &str,
    reference_wkt: &str,
    epsg: u32,
) -> Result<EpsgParameterMatch, String> {
    let terrain = parse(terrain_wkt).map_err(|e| format!("terrain WKT: {e}"))?;
    let reference = parse(reference_wkt).map_err(|e| format!("EPSG:{epsg} WKT: {e}"))?;
    if reference.key != "PROJCS" {
        return Err(format!("EPSG:{epsg} is not a projected CRS"));
    }
    if reference.authority() != Some(("EPSG".into(), epsg)) {
        return Err(format!(
            "EPSG:{epsg} reference WKT does not carry its own code"
        ));
    }
    if terrain.key != "PROJCS" {
        return Err(format!("terrain CRS is {} (PROJCS required)", terrain.key));
    }
    let allowed = ["GEOGCS", "PROJECTION", "PARAMETER", "UNIT", "AXIS"];
    if let Some(n) = terrain.nodes().find(|n| !allowed.contains(&n.key.as_str())) {
        return Err(format!("terrain PROJCS has an unsupported {} node", n.key));
    }
    if !matches!(terrain.items.first(), Some(Item::Text(_)))
        || terrain
            .items
            .iter()
            .skip(1)
            .any(|i| !matches!(i, Item::Node(_)))
    {
        return Err("terrain PROJCS has unexpected values".into());
    }

    // Geographic CRS, datum, ellipsoid, prime meridian, angular unit.
    let (tg, rg) = (
        terrain.one("GEOGCS").map_err(|e| format!("terrain {e}"))?,
        reference
            .one("GEOGCS")
            .map_err(|e| format!("reference {e}"))?,
    );
    let geographic_epsg = equal_epsg(tg, rg, "geographic CRS")?;
    for n in tg.nodes() {
        if !["DATUM", "PRIMEM", "UNIT", "AUTHORITY", "AXIS"].contains(&n.key.as_str()) {
            return Err(format!("terrain GEOGCS has an unsupported {} node", n.key));
        }
    }
    let (td, rd) = (tg.one("DATUM")?, rg.one("DATUM")?);
    let datum_epsg = equal_epsg(td, rd, "datum")?;
    for n in td.nodes() {
        match n.key.as_str() {
            "SPHEROID" | "AUTHORITY" => {}
            // A datum shift is accepted only when the reference states the same one.
            "TOWGS84" => {
                let rt = rd
                    .one("TOWGS84")
                    .map_err(|_| "terrain datum has TOWGS84; the reference does not".to_string())?;
                let (a, b) = (n.numbers(), rt.numbers());
                if a.len() != b.len() || a.iter().zip(&b).any(|(x, y)| (x - y).abs() > 1e-9) {
                    return Err("terrain datum TOWGS84 differs from the reference".into());
                }
            }
            other => return Err(format!("terrain datum has an unsupported {other} node")),
        }
    }
    let (ts, rs) = (td.one("SPHEROID")?, rd.one("SPHEROID")?);
    let (ta, rf_t) = ts.two_numbers("SPHEROID")?;
    let (ra, rf_r) = rs.two_numbers("SPHEROID")?;
    if !close_relative(ta, ra, UNIT_RELATIVE_TOLERANCE)
        || !close_relative(rf_t, rf_r, INVERSE_FLATTENING_RELATIVE_TOLERANCE)
    {
        return Err(format!(
            "ellipsoid ({ta}, {rf_t}) differs from EPSG:{epsg}'s ({ra}, {rf_r})"
        ));
    }
    optional_codes_equal(ts, rs, "ellipsoid")?;
    let tp = tg
        .one("PRIMEM")?
        .number(0)
        .ok_or("terrain PRIMEM has no value")?;
    let rp = rg
        .one("PRIMEM")?
        .number(0)
        .ok_or("reference PRIMEM has no value")?;
    if (tp - rp).abs() > 1e-12 {
        return Err(format!("prime meridian {tp} differs from {rp}"));
    }
    optional_codes_equal(tg.one("PRIMEM")?, rg.one("PRIMEM")?, "prime meridian")?;
    let tau = tg
        .one("UNIT")?
        .number(0)
        .ok_or("terrain angular unit has no factor")?;
    let rau = rg
        .one("UNIT")?
        .number(0)
        .ok_or("reference angular unit has no factor")?;
    if !close_relative(tau, rau, UNIT_RELATIVE_TOLERANCE) {
        return Err(format!("angular unit {tau} differs from {rau}"));
    }
    axes_east_north(tg, true)?;

    // Projection method.
    let tm = terrain.one("PROJECTION")?;
    let rm = reference.one("PROJECTION")?;
    let projection = tm.text(0).ok_or("terrain PROJECTION has no name")?;
    let reference_projection = rm.text(0).ok_or("reference PROJECTION has no name")?;
    if normalize(&projection) != normalize(&reference_projection) {
        return Err(format!(
            "projection {projection} differs from EPSG:{epsg}'s {reference_projection}"
        ));
    }
    optional_codes_equal(tm, rm, "projection")?;

    // Linear unit.
    let (tu, ru) = (terrain.one("UNIT")?, reference.one("UNIT")?);
    let linear = tu.number(0).ok_or("terrain linear unit has no factor")?;
    let reference_linear = ru.number(0).ok_or("reference linear unit has no factor")?;
    if !(linear.is_finite() && linear > 0.0)
        || !close_relative(linear, reference_linear, UNIT_RELATIVE_TOLERANCE)
    {
        return Err(format!(
            "linear unit {} ({linear} m) differs from EPSG:{epsg}'s {} ({reference_linear} m)",
            tu.text(0).unwrap_or_default(),
            ru.text(0).unwrap_or_default()
        ));
    }
    optional_codes_equal(tu, ru, "linear unit")?;

    // Parameters.
    let tparams = parameters(&terrain).map_err(|e| format!("terrain {e}"))?;
    let rparams = parameters(&reference).map_err(|e| format!("reference {e}"))?;
    if tparams.len() != rparams.len()
        || tparams
            .iter()
            .any(|(k, _, _)| !rparams.iter().any(|(r, _, _)| r == k))
    {
        let names = |p: &[(String, String, f64)]| {
            p.iter()
                .map(|(_, n, _)| n.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        return Err(format!(
            "parameters [{}] differ from EPSG:{epsg}'s [{}]",
            names(&tparams),
            names(&rparams)
        ));
    }
    // Parameters are in the GEOGCS angular unit; express angle differences in degrees.
    let angle_difference = |a: f64, b: f64| (a - b).abs() * tau / (std::f64::consts::PI / 180.0);
    let value_of = |p: &[(String, String, f64)], key: &str| {
        p.iter().find(|(k, _, _)| k == key).map(|(_, _, v)| *v)
    };
    let within = |a: Option<f64>, b: Option<f64>| match (a, b) {
        (Some(a), Some(b)) => angle_difference(a, b) <= ANGLE_TOLERANCE_DEG,
        _ => false,
    };
    let (sp1, sp2) = ("standardparallel1", "standardparallel2");
    let standard_parallels_swapped = TWO_PARALLEL_CONICS.contains(&normalize(&projection).as_str())
        && !(within(value_of(&tparams, sp1), value_of(&rparams, sp1))
            && within(value_of(&tparams, sp2), value_of(&rparams, sp2)))
        && within(value_of(&tparams, sp1), value_of(&rparams, sp2))
        && within(value_of(&tparams, sp2), value_of(&rparams, sp1));
    let mut checks = Vec::with_capacity(tparams.len());
    for (key, name, value) in &tparams {
        let reference_key = match key.as_str() {
            k if standard_parallels_swapped && k == sp1 => sp2,
            k if standard_parallels_swapped && k == sp2 => sp1,
            k => k,
        };
        let reference_value = value_of(&rparams, reference_key).unwrap();
        let (kind, difference, tolerance) = match classify(key) {
            Some("angle") => (
                "angle_degrees",
                angle_difference(*value, reference_value),
                ANGLE_TOLERANCE_DEG,
            ),
            Some("linear") => (
                "linear_metres",
                (value - reference_value).abs() * linear,
                LINEAR_TOLERANCE_M,
            ),
            Some("scale") => ("scale", (value - reference_value).abs(), SCALE_TOLERANCE),
            _ => return Err(format!("parameter {name} is not classified by {RULE}")),
        };
        if difference.is_nan() || difference > tolerance {
            return Err(format!(
                "parameter {name} = {value} differs from EPSG:{epsg}'s {reference_value} ({kind} difference {difference:e} > {tolerance:e})"
            ));
        }
        checks.push(ParameterCheck {
            name: name.clone(),
            terrain: *value,
            reference: reference_value,
            kind,
            difference,
            tolerance,
        });
    }
    axes_east_north(&terrain, false)?;
    axes_east_north(&reference, false)
        .map_err(|e| format!("EPSG:{epsg} is not easting/northing: {e}"))?;

    Ok(EpsgParameterMatch {
        rule: RULE,
        epsg,
        terrain_crs_name: terrain.text(0).unwrap_or_default(),
        reference_crs_name: reference.text(0).unwrap_or_default(),
        geographic_epsg,
        datum_epsg,
        projection,
        linear_unit: tu.text(0).unwrap_or_default(),
        linear_unit_metres: linear,
        linear_unit_epsg: tu.authority().filter(|(a, _)| a == "EPSG").map(|(_, c)| c),
        parameters: checks,
        standard_parallels_swapped,
        reference_wkt_fnv1a64: format!("{:016x}", crate::sink::fnv1a64(reference_wkt.as_bytes())),
        epsg_registry: terrio::crs::EPSG_VERSION,
    })
}

/// `angle`, `linear` or `scale` for the WKT 1 parameter names GDAL writes.
fn classify(key: &str) -> Option<&'static str> {
    const ANGLES: [&str; 12] = [
        "latitudeoforigin",
        "centralmeridian",
        "standardparallel1",
        "standardparallel2",
        "longitudeofcenter",
        "latitudeofcenter",
        "longitudeoforigin",
        "azimuth",
        "rectifiedgridangle",
        "longitudeofpoint1",
        "latitudeofpoint1",
        "pseudostandardparallel1",
    ];
    if ANGLES.contains(&key) {
        Some("angle")
    } else if key == "falseeasting" || key == "falsenorthing" {
        Some("linear")
    } else if key == "scalefactor" {
        Some("scale")
    } else {
        None
    }
}

fn parameters(n: &Node) -> Result<Vec<(String, String, f64)>, String> {
    let mut out: Vec<(String, String, f64)> = Vec::new();
    for p in n.nodes().filter(|p| p.key == "PARAMETER") {
        let name = p.text(0).ok_or("PARAMETER without a name")?;
        let value = p
            .number(0)
            .ok_or_else(|| format!("PARAMETER {name} without a value"))?;
        if !value.is_finite() {
            return Err(format!("PARAMETER {name} is not finite"));
        }
        let key = normalize(&name);
        if out.iter().any(|(k, _, _)| *k == key) {
            return Err(format!("duplicate PARAMETER {name}"));
        }
        out.push((key, name, value));
    }
    Ok(out)
}

/// Both nodes carry an EPSG `AUTHORITY`, with the same code.
fn equal_epsg(t: &Node, r: &Node, what: &str) -> Result<u32, String> {
    let rc = match r.authority() {
        Some((a, c)) if a == "EPSG" => c,
        _ => return Err(format!("reference {what} has no EPSG code")),
    };
    match t.authority() {
        Some((a, c)) if a == "EPSG" && c == rc => Ok(c),
        Some((a, c)) => Err(format!("terrain {what} is {a}:{c}, not EPSG:{rc}")),
        None => Err(format!(
            "terrain {what} has no EPSG code (EPSG:{rc} required); a datum is not identified from its ellipsoid"
        )),
    }
}

/// Where both declare an authority code, they agree.
fn optional_codes_equal(t: &Node, r: &Node, what: &str) -> Result<(), String> {
    match (t.authority(), r.authority()) {
        (Some(a), Some(b)) if a != b => Err(format!(
            "terrain {what} is {}:{}, reference {}:{}",
            a.0, a.1, b.0, b.1
        )),
        _ => Ok(()),
    }
}

/// Declared axes are (east, north) for a projected CRS; for a geographic CRS any declared order
/// is accepted only when absent or (north, east)/(east, north) as GDAL writes them.
fn axes_east_north(n: &Node, geographic: bool) -> Result<(), String> {
    let dirs: Vec<String> = n
        .nodes()
        .filter(|a| a.key == "AXIS")
        .map(|a| a.bare(0).unwrap_or_default().to_ascii_uppercase())
        .collect();
    let ok =
        dirs.is_empty() || dirs == ["EAST", "NORTH"] || (geographic && dirs == ["NORTH", "EAST"]);
    if ok {
        Ok(())
    } else {
        Err(format!("axis order {dirs:?} is not easting, northing"))
    }
}

fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

fn close_relative(a: f64, b: f64, tolerance: f64) -> bool {
    a.is_finite() && b.is_finite() && (a - b).abs() <= tolerance * a.abs().max(b.abs())
}

// --- A small WKT 1 reader -------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Item {
    Node(Node),
    Text(String),
    Number(f64),
    Bare(String),
}

#[derive(Debug, Clone, PartialEq)]
struct Node {
    key: String,
    items: Vec<Item>,
}

impl Node {
    fn nodes(&self) -> impl Iterator<Item = &Node> {
        self.items.iter().filter_map(|i| match i {
            Item::Node(n) => Some(n),
            _ => None,
        })
    }

    fn one(&self, key: &str) -> Result<&Node, String> {
        let mut it = self.nodes().filter(|n| n.key == key);
        match (it.next(), it.next()) {
            (Some(n), None) => Ok(n),
            (None, _) => Err(format!("{} has no {key}", self.key)),
            _ => Err(format!("{} has more than one {key}", self.key)),
        }
    }

    fn text(&self, index: usize) -> Option<String> {
        self.items
            .iter()
            .filter_map(|i| match i {
                Item::Text(t) => Some(t.clone()),
                _ => None,
            })
            .nth(index)
    }

    fn bare(&self, index: usize) -> Option<String> {
        self.items
            .iter()
            .filter_map(|i| match i {
                Item::Bare(t) => Some(t.clone()),
                _ => None,
            })
            .nth(index)
    }

    fn number(&self, index: usize) -> Option<f64> {
        self.numbers().get(index).copied()
    }

    fn numbers(&self) -> Vec<f64> {
        self.items
            .iter()
            .filter_map(|i| match i {
                Item::Number(v) => Some(*v),
                _ => None,
            })
            .collect()
    }

    fn two_numbers(&self, what: &str) -> Result<(f64, f64), String> {
        match self.numbers()[..] {
            [a, b, ..] => Ok((a, b)),
            _ => Err(format!("{what} needs two values")),
        }
    }

    /// The node's own `AUTHORITY["name","code"]`, if it has exactly one.
    fn authority(&self) -> Option<(String, u32)> {
        let a = self.one("AUTHORITY").ok()?;
        let name = a.text(0)?.to_ascii_uppercase();
        let code = a.text(1)?.trim().parse().ok()?;
        Some((name, code))
    }
}

fn parse(wkt: &str) -> Result<Node, String> {
    if wkt.len() > MAX_WKT_LEN {
        return Err("WKT too long".into());
    }
    let b = wkt.as_bytes();
    let mut pos = 0;
    let node = parse_node(b, &mut pos, 0)?;
    skip_ws(b, &mut pos);
    if pos != b.len() {
        return Err(format!("trailing text at byte {pos}"));
    }
    Ok(node)
}

fn skip_ws(b: &[u8], pos: &mut usize) {
    while *pos < b.len() && b[*pos].is_ascii_whitespace() {
        *pos += 1;
    }
}

fn parse_node(b: &[u8], pos: &mut usize, depth: usize) -> Result<Node, String> {
    if depth > MAX_DEPTH {
        return Err("WKT nested too deeply".into());
    }
    skip_ws(b, pos);
    let start = *pos;
    while *pos < b.len() && (b[*pos].is_ascii_alphanumeric() || b[*pos] == b'_') {
        *pos += 1;
    }
    if *pos == start {
        return Err(format!("expected a keyword at byte {start}"));
    }
    let key = String::from_utf8_lossy(&b[start..*pos]).to_ascii_uppercase();
    skip_ws(b, pos);
    let close = match b.get(*pos) {
        Some(b'[') => b']',
        Some(b'(') => b')',
        _ => return Err(format!("expected [ after {key}")),
    };
    *pos += 1;
    let mut items = Vec::new();
    loop {
        skip_ws(b, pos);
        match b.get(*pos) {
            None => return Err(format!("unterminated {key}")),
            Some(&c) if c == close && items.is_empty() => {
                *pos += 1;
                break;
            }
            _ => {}
        }
        items.push(parse_item(b, pos, depth)?);
        skip_ws(b, pos);
        match b.get(*pos) {
            Some(b',') => *pos += 1,
            Some(&c) if c == close => {
                *pos += 1;
                break;
            }
            _ => return Err(format!("expected , or close in {key} at byte {}", *pos)),
        }
    }
    Ok(Node { key, items })
}

fn parse_item(b: &[u8], pos: &mut usize, depth: usize) -> Result<Item, String> {
    match b.get(*pos) {
        Some(b'"') => {
            *pos += 1;
            let mut s = Vec::new();
            loop {
                match b.get(*pos) {
                    None => return Err("unterminated string".into()),
                    // WKT doubles a quote to escape it.
                    Some(b'"') if b.get(*pos + 1) == Some(&b'"') => {
                        s.push(b'"');
                        *pos += 2;
                    }
                    Some(b'"') => {
                        *pos += 1;
                        break;
                    }
                    Some(&c) => {
                        s.push(c);
                        *pos += 1;
                    }
                }
            }
            Ok(Item::Text(String::from_utf8_lossy(&s).into_owned()))
        }
        Some(c) if c.is_ascii_digit() || matches!(c, b'-' | b'+' | b'.') => {
            let start = *pos;
            while *pos < b.len() && (b[*pos].is_ascii_digit() || b"+-.eE".contains(&b[*pos])) {
                *pos += 1;
            }
            let t = std::str::from_utf8(&b[start..*pos]).map_err(|e| e.to_string())?;
            t.parse()
                .map(Item::Number)
                .map_err(|_| format!("bad number {t}"))
        }
        Some(c) if c.is_ascii_alphabetic() => {
            let start = *pos;
            while *pos < b.len() && (b[*pos].is_ascii_alphanumeric() || b[*pos] == b'_') {
                *pos += 1;
            }
            let word = String::from_utf8_lossy(&b[start..*pos]).into_owned();
            let mut look = *pos;
            skip_ws(b, &mut look);
            if matches!(b.get(look), Some(b'[') | Some(b'(')) {
                *pos = start;
                parse_node(b, pos, depth + 1).map(Item::Node)
            } else {
                Ok(Item::Bare(word))
            }
        }
        _ => Err(format!("unexpected byte at {}", *pos)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The CRS terrio reads from the GeoKeys of HEC-RAS-written HCFCD terrains: EPSG:2278 in
    /// every parameter, but a user-defined projected CRS without a code.
    const HEC_RAS_2278: &str = "PROJCS[\"unknown\",GEOGCS[\"NAD83\",DATUM[\"North_American_Datum_1983\",SPHEROID[\"GRS 1980\",6378137,298.257222101,AUTHORITY[\"EPSG\",\"7019\"]],AUTHORITY[\"EPSG\",\"6269\"]],PRIMEM[\"Greenwich\",0,AUTHORITY[\"EPSG\",\"8901\"]],UNIT[\"degree\",0.0174532925199433,AUTHORITY[\"EPSG\",\"9122\"]],AUTHORITY[\"EPSG\",\"4269\"]],PROJECTION[\"Lambert_Conformal_Conic_2SP\"],PARAMETER[\"latitude_of_origin\",27.8333333333333],PARAMETER[\"central_meridian\",-99],PARAMETER[\"standard_parallel_1\",30.2833333333333],PARAMETER[\"standard_parallel_2\",28.3833333333333],PARAMETER[\"false_easting\",1968500],PARAMETER[\"false_northing\",13123333.333],UNIT[\"US survey foot\",0.304800609601219,AUTHORITY[\"EPSG\",\"9003\"]],AXIS[\"Easting\",EAST],AXIS[\"Northing\",NORTH]]";

    fn refused(wkt: &str, epsg: u32) -> String {
        match_projected_epsg(wkt, epsg).unwrap_err()
    }

    #[test]
    fn identifies_hec_ras_texas_south_central_ftus_as_2278() {
        let m = match_projected_epsg(HEC_RAS_2278, 2278).unwrap();
        assert_eq!(m.rule, RULE);
        assert_eq!(
            (m.epsg, m.geographic_epsg, m.datum_epsg),
            (2278, 4269, 6269)
        );
        assert_eq!(m.linear_unit_epsg, Some(9003));
        assert_eq!(m.projection, "Lambert_Conformal_Conic_2SP");
        assert_eq!(m.reference_crs_name, "NAD83 / Texas South Central (ftUS)");
        assert_eq!(m.parameters.len(), 6);
        assert!(m.parameters.iter().all(|p| p.difference <= p.tolerance));
    }

    #[test]
    fn accepts_gdal_style_name_and_digits() {
        // The PROJCS name the GTCitation gives and the digits gdalinfo prints.
        let wkt = HEC_RAS_2278
            .replace("\"unknown\"", "\"NAD83 / Texas South Central (ftUS)\"")
            .replace("298.257222101", "298.2572221010042")
            .replace("0.304800609601219", "0.3048006096012192")
            .replace("13123333.333", "13123333.3333333");
        let m = match_projected_epsg(&wkt, 2278).unwrap();
        assert_eq!(m.terrain_crs_name, "NAD83 / Texas South Central (ftUS)");
        // Without AXIS nodes, as gdalinfo prints the PROJCS.
        let no_axes = wkt.replace(",AXIS[\"Easting\",EAST],AXIS[\"Northing\",NORTH]", "");
        match_projected_epsg(&no_axes, 2278).unwrap();
    }

    #[test]
    fn accepts_swapped_standard_parallels_of_a_two_parallel_conic() {
        // As HEC-RAS writes Brays Bayou's terrain.
        let swapped = HEC_RAS_2278
            .replace("30.2833333333333", "SP_A")
            .replace("28.3833333333333", "30.2833333333333")
            .replace("SP_A", "28.3833333333333")
            .replace("13123333.333", "13123333.3333333");
        let m = match_projected_epsg(&swapped, 2278).unwrap();
        assert!(m.standard_parallels_swapped);
        let p1 = m
            .parameters
            .iter()
            .find(|p| p.name == "standard_parallel_1")
            .unwrap();
        assert_eq!(
            (p1.terrain, p1.reference),
            (28.3833333333333, 28.3833333333333)
        );
        assert!(
            !match_projected_epsg(HEC_RAS_2278, 2278)
                .unwrap()
                .standard_parallels_swapped
        );
        // Swapped, but one parallel off: refused.
        let off = swapped.replace("30.2833333333333", "30.2833333433333");
        assert!(refused(&off, 2278).contains("standard_parallel"));
        // Not for other parameters: origin latitude and a parallel exchanged.
        let other = HEC_RAS_2278
            .replace("27.8333333333333", "SP_A")
            .replace("28.3833333333333", "27.8333333333333")
            .replace("SP_A", "28.3833333333333");
        assert!(refused(&other, 2278).contains("parameter"));
    }

    #[test]
    fn refuses_international_foot() {
        let wkt = HEC_RAS_2278.replace(
            "UNIT[\"US survey foot\",0.304800609601219,AUTHORITY[\"EPSG\",\"9003\"]]",
            "UNIT[\"foot\",0.3048,AUTHORITY[\"EPSG\",\"9002\"]]",
        );
        let e = refused(&wkt, 2278);
        assert!(e.contains("linear unit"), "{e}");
        // Same factor, wrong unit code.
        let wkt = HEC_RAS_2278.replace("\"9003\"", "\"9002\"");
        let e = refused(&wkt, 2278);
        assert!(e.contains("linear unit is EPSG:9002"), "{e}");
    }

    #[test]
    fn refuses_other_datums_and_session_codes() {
        // Same parameters on NAD83(2011): EPSG:6588, not 2278, and vice versa.
        let e = refused(HEC_RAS_2278, 6588);
        assert!(
            e.contains("geographic CRS is EPSG:4269, not EPSG:6318"),
            "{e}"
        );
        let harn = HEC_RAS_2278
            .replace("\"4269\"", "\"4152\"")
            .replace("\"6269\"", "\"6152\"");
        let e = refused(&harn, 2278);
        assert!(e.contains("geographic CRS is EPSG:4152"), "{e}");
        // A datum without a code is never inferred from the ellipsoid.
        let uncoded = HEC_RAS_2278.replace(",AUTHORITY[\"EPSG\",\"4269\"]", "");
        let e = refused(&uncoded, 2278);
        assert!(e.contains("geographic CRS has no EPSG code"), "{e}");
        // The metre zone shares the projection but not the unit.
        let e = refused(HEC_RAS_2278, 32140);
        assert!(e.contains("linear unit"), "{e}");
        // Another zone.
        let e = refused(HEC_RAS_2278, 2277);
        assert!(e.contains("parameter"), "{e}");
    }

    #[test]
    fn refuses_parameter_drift_beyond_tolerance() {
        // 1e-8 degree on the origin latitude (about 1 mm).
        let wkt = HEC_RAS_2278.replace("27.8333333333333", "27.83333334");
        let e = refused(&wkt, 2278);
        assert!(e.contains("latitude_of_origin"), "{e}");
        // 0.01 ft false easting (3 mm).
        let wkt = HEC_RAS_2278.replace("1968500", "1968500.01");
        let e = refused(&wkt, 2278);
        assert!(e.contains("false_easting"), "{e}");
        // Within 1 mm is accepted (0.003 ftUS = 0.9 mm).
        let wkt = HEC_RAS_2278.replace("1968500", "1968500.003");
        match_projected_epsg(&wkt, 2278).unwrap();
    }

    #[test]
    fn refuses_structural_differences() {
        let one_sp =
            HEC_RAS_2278.replace("Lambert_Conformal_Conic_2SP", "Lambert_Conformal_Conic_1SP");
        assert!(refused(&one_sp, 2278).contains("projection"));
        let extra = HEC_RAS_2278.replace(
            "PARAMETER[\"false_easting\"",
            "PARAMETER[\"scale_factor\",1],PARAMETER[\"false_easting\"",
        );
        assert!(refused(&extra, 2278).contains("parameters"));
        let dup = HEC_RAS_2278.replace(
            "PARAMETER[\"false_easting\",1968500]",
            "PARAMETER[\"central_meridian\",-99]",
        );
        assert!(refused(&dup, 2278).contains("duplicate"));
        let shift = HEC_RAS_2278.replace(
            ",AUTHORITY[\"EPSG\",\"6269\"]]",
            ",TOWGS84[1,2,3,0,0,0,0],AUTHORITY[\"EPSG\",\"6269\"]]",
        );
        assert!(refused(&shift, 2278).contains("TOWGS84"));
        let swapped = HEC_RAS_2278.replace(
            "AXIS[\"Easting\",EAST],AXIS[\"Northing\",NORTH]",
            "AXIS[\"Northing\",NORTH],AXIS[\"Easting\",EAST]",
        );
        assert!(refused(&swapped, 2278).contains("axis"));
        let coded = HEC_RAS_2278.replacen(
            "AXIS[\"Northing\",NORTH]]",
            "AXIS[\"Northing\",NORTH],AUTHORITY[\"ESRI\",\"102740\"]]",
            1,
        );
        assert!(refused(&coded, 2278).contains("AUTHORITY"));
        let geographic = "GEOGCS[\"NAD83\",DATUM[\"North_American_Datum_1983\",SPHEROID[\"GRS 1980\",6378137,298.257222101]],PRIMEM[\"Greenwich\",0],UNIT[\"degree\",0.0174532925199433],AUTHORITY[\"EPSG\",\"4269\"]]";
        assert!(refused(geographic, 2278).contains("PROJCS required"));
        assert!(refused(HEC_RAS_2278, 4269).contains("not a projected CRS"));
        assert!(refused("PROJCS[\"x\"", 2278).contains("terrain WKT"));
    }

    #[test]
    fn wkt_reader_handles_quotes_and_parentheses() {
        let n = parse("PROJCS(\"a \"\"b\"\"\", UNIT(\"m\", 1e0))").unwrap();
        assert_eq!(n.text(0).unwrap(), "a \"b\"");
        assert_eq!(n.one("UNIT").unwrap().number(0), Some(1.0));
        assert!(parse("PROJCS[\"a\"] x").is_err());
        let deep = format!("{}{}", "A[".repeat(40), "]".repeat(40));
        assert!(parse(&deep).is_err());
    }
}
