//! The model/terrain CRS gate without PROJ.
//!
//! ras-commander requires the plan HDF's CRS (`HdfBase.get_projection`) to
//! identify as the terrain's EPSG code (`_prepare.same_crs`, `to_epsg()`), and
//! takes the request's `epsg` from the terrain. Here the terrain must carry an
//! EPSG GeoKey, and the HDF `Projection` WKT (ESRI or OGC WKT1) must match that
//! EPSG definition from terrio's registry parameter by parameter: projection
//! method, datum name, ellipsoid, prime meridian, linear unit and every
//! conversion parameter within PROJ's 1e-10 relative equivalence tolerance.
//! Anything else is refused, so this gate never accepts a CRS that
//! ras-commander would refuse.

use crate::error::{Error, Result};
use serde_json::Value;

#[derive(Debug, Clone)]
enum Node {
    Text(String),
    Number(f64),
    Tag(String, Vec<Node>),
}

fn parse(wkt: &str) -> Result<Node> {
    let bytes: Vec<char> = wkt.chars().collect();
    let mut pos = 0;
    let node = parse_node(&bytes, &mut pos)?;
    Ok(node)
}

fn skip_ws(s: &[char], pos: &mut usize) {
    while *pos < s.len() && s[*pos].is_whitespace() {
        *pos += 1;
    }
}

fn parse_node(s: &[char], pos: &mut usize) -> Result<Node> {
    skip_ws(s, pos);
    if *pos >= s.len() {
        return Err(Error::unsupported("truncated WKT"));
    }
    if s[*pos] == '"' {
        *pos += 1;
        let start = *pos;
        while *pos < s.len() && s[*pos] != '"' {
            *pos += 1;
        }
        let text: String = s[start..*pos].iter().collect();
        *pos += 1;
        return Ok(Node::Text(text));
    }
    let start = *pos;
    while *pos < s.len() && !matches!(s[*pos], '[' | ']' | '(' | ')' | ',') {
        *pos += 1;
    }
    let word: String = s[start..*pos].iter().collect::<String>().trim().to_string();
    skip_ws(s, pos);
    if *pos < s.len() && (s[*pos] == '[' || s[*pos] == '(') {
        *pos += 1;
        let mut args = Vec::new();
        loop {
            skip_ws(s, pos);
            if *pos < s.len() && (s[*pos] == ']' || s[*pos] == ')') {
                *pos += 1;
                break;
            }
            args.push(parse_node(s, pos)?);
            skip_ws(s, pos);
            if *pos < s.len() && s[*pos] == ',' {
                *pos += 1;
            }
        }
        return Ok(Node::Tag(word.to_ascii_uppercase(), args));
    }
    word.parse::<f64>()
        .map(Node::Number)
        .map_err(|_| Error::unsupported(format!("unparsed WKT token {word:?}")))
}

fn children<'a>(n: &'a Node, tag: &str) -> Vec<&'a Node> {
    match n {
        Node::Tag(_, args) => args
            .iter()
            .filter(|a| matches!(a, Node::Tag(t, _) if t == tag))
            .collect(),
        _ => Vec::new(),
    }
}
fn child<'a>(n: &'a Node, tag: &str) -> Result<&'a Node> {
    children(n, tag)
        .into_iter()
        .next()
        .ok_or_else(|| Error::unsupported(format!("WKT lacks {tag}")))
}
fn name(n: &Node) -> String {
    match n {
        Node::Tag(_, args) => match args.first() {
            Some(Node::Text(t)) => t.clone(),
            _ => String::new(),
        },
        _ => String::new(),
    }
}
fn number(n: &Node, k: usize) -> Result<f64> {
    match n {
        Node::Tag(_, args) => match args.get(k) {
            Some(Node::Number(v)) if v.is_finite() => Ok(*v),
            _ => Err(Error::unsupported("WKT value is not a number")),
        },
        _ => Err(Error::unsupported("WKT value is not a number")),
    }
}

fn norm_name(s: &str) -> String {
    let s = s.to_ascii_lowercase();
    let s = s.strip_prefix("d_").unwrap_or(&s);
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .replace("datum", "")
}

fn close(a: f64, b: f64) -> bool {
    if b == 0.0 {
        a.abs() <= 1e-10
    } else {
        ((a - b) / b).abs() <= 1e-10
    }
}

fn unit_factor(u: &Value) -> Result<f64> {
    match u {
        Value::String(s) if s == "metre" => Ok(1.0),
        Value::String(s) if s == "degree" => Ok(1.0),
        Value::Object(o) => o
            .get("conversion_factor")
            .and_then(Value::as_f64)
            .ok_or_else(|| Error::unsupported("registry unit lacks a conversion factor")),
        _ => Ok(1.0),
    }
}

/// EPSG method code and the WKT parameter names mapped to EPSG parameter codes.
fn method(projection: &str) -> Option<(u64, &'static [(&'static str, u64)])> {
    const LCC: &[(&str, u64)] = &[
        ("latitude_of_origin", 8821),
        ("central_meridian", 8822),
        ("standard_parallel_1", 8823),
        ("standard_parallel_2", 8824),
        ("false_easting", 8826),
        ("false_northing", 8827),
    ];
    const TM: &[(&str, u64)] = &[
        ("latitude_of_origin", 8801),
        ("central_meridian", 8802),
        ("scale_factor", 8805),
        ("false_easting", 8806),
        ("false_northing", 8807),
    ];
    const ALBERS: &[(&str, u64)] = &[
        ("latitude_of_center", 8821),
        ("latitude_of_origin", 8821),
        ("longitude_of_center", 8822),
        ("central_meridian", 8822),
        ("standard_parallel_1", 8823),
        ("standard_parallel_2", 8824),
        ("false_easting", 8826),
        ("false_northing", 8827),
    ];
    match projection.to_ascii_lowercase().as_str() {
        "lambert_conformal_conic" | "lambert_conformal_conic_2sp" => Some((9802, LCC)),
        "transverse_mercator" => Some((9807, TM)),
        "albers" | "albers_conic_equal_area" => Some((9822, ALBERS)),
        _ => None,
    }
}

/// Refuse unless the HDF `Projection` WKT is the terrain's EPSG CRS.
pub fn require_epsg(hdf_wkt: Option<&str>, epsg: u32) -> Result<()> {
    let wkt = hdf_wkt.ok_or_else(|| {
        Error::unsupported(
            "the plan HDF records no Projection; the CRS cannot be matched to the terrain",
        )
    })?;
    let fail = |why: &str| {
        Error::unsupported(format!(
            "the hydraulic results CRS must equal the terrain CRS (EPSG:{epsg}): {why}"
        ))
    };
    let identified = terrio::crs::SpatialRef::from_wkt(wkt).epsg;
    if identified != Some(epsg) {
        return Err(fail(&format!(
            "the HDF Projection identifies as {identified:?}"
        )));
    }
    let root = parse(wkt)?;
    match &root {
        Node::Tag(t, _) if t == "PROJCS" => {}
        _ => return Err(fail("the HDF Projection is not a projected WKT1 CRS")),
    }
    let registry: Value = serde_json::from_str(
        &terrio::crs::SpatialRef::from_epsg(epsg)
            .map_err(|e| fail(&e.to_string()))?
            .to_projjson()
            .map_err(|e| fail(&e.to_string()))?,
    )?;
    let geogcs = child(&root, "GEOGCS")?;
    let datum = child(geogcs, "DATUM")?;
    let spheroid = child(datum, "SPHEROID")?;
    let base = &registry["base_crs"];
    let rdatum = &base["datum"];
    if norm_name(&name(datum)) != norm_name(rdatum["name"].as_str().unwrap_or("")) {
        return Err(fail("datum differs"));
    }
    let ellipsoid = &rdatum["ellipsoid"];
    let (a, rf) = (number(spheroid, 1)?, number(spheroid, 2)?);
    if !close(a, ellipsoid["semi_major_axis"].as_f64().unwrap_or(f64::NAN))
        || !close(
            rf,
            ellipsoid["inverse_flattening"].as_f64().unwrap_or(f64::NAN),
        )
    {
        return Err(fail("ellipsoid differs"));
    }
    if let Ok(pm) = child(geogcs, "PRIMEM")
        && number(pm, 1)? != 0.0
    {
        return Err(fail("prime meridian is not Greenwich"));
    }
    let projection = name(child(&root, "PROJECTION")?);
    let (code, names) = method(&projection)
        .ok_or_else(|| fail(&format!("projection {projection:?} is not supported")))?;
    if registry["conversion"]["method"]["id"]["code"].as_u64() != Some(code) {
        return Err(fail("projection method differs"));
    }
    let unit = child(&root, "UNIT")?;
    let linear = number(unit, 1)?;
    let axis_unit = unit_factor(&registry["coordinate_system"]["axis"][0]["unit"])?;
    if !close(linear, axis_unit) {
        return Err(fail("linear unit differs"));
    }
    let params = registry["conversion"]["parameters"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let target = |code: u64| -> Result<f64> {
        let p = params
            .iter()
            .find(|p| p["id"]["code"].as_u64() == Some(code))
            .ok_or_else(|| fail(&format!("registry lacks parameter {code}")))?;
        Ok(p["value"].as_f64().unwrap_or(f64::NAN) * unit_factor(&p["unit"])?)
    };
    let mut parallels = Vec::new();
    let mut matched = std::collections::BTreeSet::new();
    for p in children(&root, "PARAMETER") {
        let key = name(p).to_ascii_lowercase();
        let value = number(p, 1)?;
        if key == "scale_factor" && code != 9807 {
            if value != 1.0 {
                return Err(fail("scale factor differs"));
            }
            continue;
        }
        let Some((_, pcode)) = names.iter().find(|(n, _)| *n == key) else {
            return Err(fail(&format!("unexpected parameter {key:?}")));
        };
        let value = if matches!(pcode, 8826 | 8827 | 8806 | 8807) {
            value * linear
        } else {
            value
        };
        if matches!(pcode, 8823 | 8824) {
            parallels.push(value);
        } else if !close(value, target(*pcode)?) {
            return Err(fail(&format!("parameter {key} differs")));
        }
        matched.insert(*pcode);
    }
    if !parallels.is_empty() {
        let mut want = [target(8823)?, target(8824)?];
        parallels.sort_by(f64::total_cmp);
        want.sort_by(f64::total_cmp);
        if parallels.len() != 2 || !close(parallels[0], want[0]) || !close(parallels[1], want[1]) {
            return Err(fail("standard parallels differ"));
        }
    }
    for p in &params {
        let c = p["id"]["code"].as_u64().unwrap_or(0);
        if !matched.contains(&c) {
            return Err(fail(&format!("WKT lacks parameter {c}")));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::require_epsg;

    /// The UPGU1/UPGU3 plan HDF `Projection` (ESRI WKT1 of EPSG:2278).
    const UPGU: &str = r#"PROJCS["NAD_1983_StatePlane_Texas_South_Central_FIPS_4204_Feet",GEOGCS["GCS_North_American_1983",DATUM["D_North_American_1983",SPHEROID["GRS_1980",6378137.0,298.257222101]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]],PROJECTION["Lambert_Conformal_Conic"],PARAMETER["False_Easting",1968500.0],PARAMETER["False_Northing",13123333.33333333],PARAMETER["Central_Meridian",-99.0],PARAMETER["Standard_Parallel_1",28.38333333333333],PARAMETER["Standard_Parallel_2",30.28333333333333],PARAMETER["Latitude_Of_Origin",27.83333333333333],UNIT["Foot_US",0.3048006096012192]]"#;

    #[test]
    fn upgu_projection_is_epsg_2278() {
        require_epsg(Some(UPGU), 2278).unwrap();
    }

    #[test]
    fn other_codes_and_changed_parameters_are_refused() {
        assert!(require_epsg(Some(UPGU), 2277).is_err());
        let shifted = UPGU.replace("1968500.0", "1968501.0");
        assert!(require_epsg(Some(&shifted), 2278).is_err());
        let harn = UPGU.replace("D_North_American_1983", "D_North_American_1983_HARN");
        assert!(require_epsg(Some(&harn), 2278).is_err());
        let mercator = UPGU.replace("Lambert_Conformal_Conic", "Mercator");
        assert!(require_epsg(Some(&mercator), 2278).is_err());
        assert!(require_epsg(None, 2278).is_err());
    }
}
