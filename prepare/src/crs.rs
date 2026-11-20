//! The model/terrain CRS gate without PROJ.
//!
//! ras-commander resolves the plan HDF's CRS from ordered evidence
//! (`HdfUtils.collect_hdf_crs_evidence`: the HDF `Projection` attribute, then
//! the `.rasmap` projection file and every terrain layer HDF it lists; any
//! conflict refuses), requires it to identify as the terrain's EPSG code
//! (`_prepare.same_crs`, `_engine.terrain_identity`), and takes the request's
//! `epsg` from the terrain. Here every evidence item found must match the
//! terrain's EPSG definition from terrio's registry parameter by parameter:
//! projection method, datum name, ellipsoid, prime meridian, linear and
//! angular units, axes, and every conversion parameter within PROJ's 1e-10
//! relative equivalence tolerance (exact for zero). WKT carrying a datum
//! shift (`TOWGS84`), a PROJ `EXTENSION`, non-standard axes or anything else
//! not listed is refused. The gate is intended to accept only CRSs PROJ would
//! find equivalent to the terrain's; it refuses whatever it cannot prove.

use crate::error::{Error, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};

const MAX_DEPTH: usize = 64;

#[derive(Debug, Clone)]
enum Node {
    Text(String),
    Number(f64),
    Word(String),
    Tag(String, Vec<Node>),
}

fn parse(wkt: &str) -> Result<Node> {
    let chars: Vec<char> = wkt.chars().collect();
    let mut pos = 0;
    let node = parse_node(&chars, &mut pos, 0)?;
    skip_ws(&chars, &mut pos);
    if pos != chars.len() {
        return Err(Error::unsupported("trailing text after the WKT definition"));
    }
    Ok(node)
}

fn skip_ws(s: &[char], pos: &mut usize) {
    while *pos < s.len() && s[*pos].is_whitespace() {
        *pos += 1;
    }
}

fn parse_node(s: &[char], pos: &mut usize, depth: usize) -> Result<Node> {
    if depth > MAX_DEPTH {
        return Err(Error::unsupported("WKT nesting is too deep"));
    }
    skip_ws(s, pos);
    if *pos >= s.len() {
        return Err(Error::unsupported("truncated WKT"));
    }
    if s[*pos] == '"' {
        *pos += 1;
        let mut text = String::new();
        loop {
            if *pos >= s.len() {
                return Err(Error::unsupported("unterminated WKT string"));
            }
            // WKT doubles a quote inside a string.
            if s[*pos] == '"' {
                if *pos + 1 < s.len() && s[*pos + 1] == '"' {
                    text.push('"');
                    *pos += 2;
                    continue;
                }
                *pos += 1;
                break;
            }
            text.push(s[*pos]);
            *pos += 1;
        }
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
            if *pos >= s.len() {
                return Err(Error::unsupported("truncated WKT"));
            }
            if s[*pos] == ']' || s[*pos] == ')' {
                *pos += 1;
                break;
            }
            args.push(parse_node(s, pos, depth + 1)?);
            skip_ws(s, pos);
            if *pos < s.len() && s[*pos] == ',' {
                *pos += 1;
            }
        }
        return Ok(Node::Tag(word.to_ascii_uppercase(), args));
    }
    if word.is_empty() {
        return Err(Error::unsupported("empty WKT token"));
    }
    Ok(match word.parse::<f64>() {
        Ok(v) => Node::Number(v),
        Err(_) => Node::Word(word),
    })
}

/// True when every bare word (an unquoted, non-numeric token) is an argument of
/// one of `parents`, as in `AXIS["Easting",EAST]` or WKT2's `CS[Cartesian,2]`.
/// PROJ rejects bare words anywhere else, so such WKT is invalid evidence.
fn bare_words_only_in(node: &Node, parents: &[&str]) -> bool {
    match node {
        Node::Tag(t, args) => args.iter().all(|a| match a {
            Node::Word(_) => parents.contains(&t.as_str()),
            other => bare_words_only_in(other, parents),
        }),
        Node::Word(_) => false,
        _ => true,
    }
}

fn tag(n: &Node) -> Option<&str> {
    match n {
        Node::Tag(t, _) => Some(t),
        _ => None,
    }
}

fn args(n: &Node) -> &[Node] {
    match n {
        Node::Tag(_, a) => a,
        _ => &[],
    }
}

fn children<'a>(n: &'a Node, name: &str) -> Vec<&'a Node> {
    args(n).iter().filter(|a| tag(a) == Some(name)).collect()
}

fn child<'a>(n: &'a Node, name: &str) -> Result<&'a Node> {
    let found = children(n, name);
    match found.as_slice() {
        [one] => Ok(one),
        [] => Err(Error::unsupported(format!("WKT lacks {name}"))),
        _ => Err(Error::unsupported(format!("WKT repeats {name}"))),
    }
}

fn name(n: &Node) -> String {
    match args(n).first() {
        Some(Node::Text(t)) => t.clone(),
        _ => String::new(),
    }
}

fn number(n: &Node, k: usize) -> Result<f64> {
    match args(n).get(k) {
        Some(Node::Number(v)) if v.is_finite() => Ok(*v),
        _ => Err(Error::unsupported("WKT value is not a number")),
    }
}

/// Every tag name in the tree.
fn tags(n: &Node, out: &mut Vec<String>) {
    if let Node::Tag(t, a) = n {
        out.push(t.clone());
        for c in a {
            tags(c, out);
        }
    }
}

/// `ID["EPSG",n]` (WKT2) or `AUTHORITY["EPSG","n"]` (WKT1) on this node.
fn authority_code(n: &Node) -> Option<u32> {
    for c in args(n) {
        if matches!(tag(c), Some("ID") | Some("AUTHORITY")) {
            let a = args(c);
            let auth = match a.first() {
                Some(Node::Text(t)) => t.as_str(),
                _ => continue,
            };
            if !auth.eq_ignore_ascii_case("EPSG") {
                continue;
            }
            return match a.get(1) {
                Some(Node::Number(v)) if v.fract() == 0.0 && *v > 0.0 && *v < 1e9 => {
                    Some(*v as u32)
                }
                Some(Node::Text(t)) => t.trim().parse().ok(),
                _ => None,
            };
        }
    }
    None
}

fn norm_name(s: &str) -> String {
    let s = s.to_ascii_lowercase();
    let s = s.strip_prefix("d_").unwrap_or(&s);
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .replace("datum", "")
}

/// PROJ's relative equivalence (`Measure::_isEquivalentTo`, 1e-10), exact at zero.
fn close(a: f64, b: f64) -> bool {
    if b == 0.0 {
        a == 0.0
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
        _ => Err(Error::unsupported("registry unit is not modelled")),
    }
}

/// WKT1 parameter names mapped to EPSG parameter codes.
type ParameterNames = &'static [(&'static str, u64)];

fn method(projection: &str) -> Option<(u64, ParameterNames)> {
    const LCC: ParameterNames = &[
        ("latitude_of_origin", 8821),
        ("central_meridian", 8822),
        ("standard_parallel_1", 8823),
        ("standard_parallel_2", 8824),
        ("false_easting", 8826),
        ("false_northing", 8827),
    ];
    const TM: ParameterNames = &[
        ("latitude_of_origin", 8801),
        ("central_meridian", 8802),
        ("scale_factor", 8805),
        ("false_easting", 8806),
        ("false_northing", 8807),
    ];
    const ALBERS: ParameterNames = &[
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

/// Refuse unless `wkt` (WKT1, ESRI or OGC) is the terrain's EPSG CRS.
pub fn require_epsg(wkt: Option<&str>, epsg: u32) -> Result<()> {
    let wkt = wkt.filter(|w| !w.trim().is_empty()).ok_or_else(|| {
        Error::unsupported(
            "the plan HDF records no Projection; the CRS cannot be matched to the terrain",
        )
    })?;
    let fail = |why: &str| {
        Error::unsupported(format!(
            "the hydraulic results CRS must equal the terrain CRS (EPSG:{epsg}): {why}"
        ))
    };
    let root = parse(wkt.trim())?;
    if tag(&root) != Some("PROJCS") {
        return Err(fail("the CRS is not a projected WKT1 CRS"));
    }
    if !bare_words_only_in(&root, &["AXIS"]) {
        return Err(fail(
            "an unquoted word outside AXIS (PROJ cannot parse this WKT)",
        ));
    }
    let mut all = Vec::new();
    tags(&root, &mut all);
    const ALLOWED: [&str; 10] = [
        "PROJCS",
        "GEOGCS",
        "DATUM",
        "SPHEROID",
        "PRIMEM",
        "UNIT",
        "PROJECTION",
        "PARAMETER",
        "AUTHORITY",
        "AXIS",
    ];
    if let Some(t) = all.iter().find(|t| !ALLOWED.contains(&t.as_str())) {
        return Err(fail(&format!(
            "WKT element {t} is not supported (no datum shift or PROJ extension)"
        )));
    }
    let identified = terrio::crs::SpatialRef::from_wkt(wkt).epsg;
    if identified != Some(epsg) {
        return Err(fail(&format!("the CRS identifies as {identified:?}")));
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
    let rdatum = &registry["base_crs"]["datum"];
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
    if let Some(pm) = children(geogcs, "PRIMEM").first()
        && number(pm, 1)? != 0.0
    {
        return Err(fail("prime meridian is not Greenwich"));
    }
    let angular = child(geogcs, "UNIT")?;
    if !close(number(angular, 1)?, std::f64::consts::PI / 180.0) {
        return Err(fail("the geographic angular unit is not the degree"));
    }
    if !children(geogcs, "AXIS").is_empty() {
        return Err(fail("geographic axes are not supported"));
    }
    let axes = children(&root, "AXIS");
    if !axes.is_empty() {
        let dirs: Vec<String> = axes
            .iter()
            .map(|a| match args(a).get(1) {
                Some(Node::Word(w)) => w.to_ascii_uppercase(),
                _ => String::new(),
            })
            .collect();
        if dirs != ["EAST", "NORTH"] {
            return Err(fail("projected axes must be east then north"));
        }
    }
    if let Some(code) = authority_code(&root)
        && code != epsg
    {
        return Err(fail(&format!("the WKT declares EPSG:{code}")));
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
        if !matched.insert(*pcode) {
            return Err(fail(&format!("parameter {key} is repeated")));
        }
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

/// The EPSG code a projected CRS WKT declares for itself (outermost `ID` or
/// `AUTHORITY` of a `PROJCRS`/`PROJCS`); None for a BoundCRS, a compound CRS,
/// a datum shift, a PROJ extension or no declared code.
pub fn declared_projected_epsg(wkt: &str) -> Option<u32> {
    let root = parse(wkt.trim()).ok()?;
    if !matches!(tag(&root), Some("PROJCRS") | Some("PROJCS")) {
        return None;
    }
    if !bare_words_only_in(&root, &["AXIS", "CS"]) {
        return None;
    }
    let mut all = Vec::new();
    tags(&root, &mut all);
    if all.iter().any(|t| {
        matches!(
            t.as_str(),
            "TOWGS84" | "EXTENSION" | "BOUNDCRS" | "ABRIDGEDTRANSFORMATION"
        )
    }) {
        return None;
    }
    authority_code(&root)
}

/// One CRS evidence item (`CrsEvidence`). `wkt` is Some when a WKT was found;
/// `epsg` is Some for a raster terrain layer, whose GeoTIFF EPSG key is read.
pub struct Evidence {
    pub source: &'static str,
    pub path: PathBuf,
    pub wkt: Option<String>,
    pub epsg: Option<u32>,
}

/// Python's `glob("*.rasmap")`: case-insensitive on Windows, exact elsewhere;
/// it matches directories and a file named `.rasmap` too.
fn is_rasmap_name(name: &str) -> bool {
    if cfg!(windows) {
        name.to_ascii_lowercase().ends_with(".rasmap")
    } else {
        name.ends_with(".rasmap")
    }
}

/// `HdfUtils._select_rasmap`: the `.rasmap` beside the HDF whose stem is the
/// project name, else the only one; None when there is none or the choice is
/// ambiguous (ras-commander then records invalid evidence, which does not vote).
/// A selected entry that is not a file is missing evidence, as in Python.
/// (Stems are compared with `to_lowercase`, not Python's `casefold`.)
fn select_rasmap(hdf: &Path) -> Option<PathBuf> {
    let folder = hdf.parent()?;
    let mut maps: Vec<PathBuf> = std::fs::read_dir(folder)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| is_rasmap_name(&n.to_string_lossy()))
        })
        .collect();
    maps.sort();
    if maps.is_empty() {
        return None;
    }
    let file = hdf.file_name()?.to_string_lossy().to_string();
    let lower = file.to_ascii_lowercase();
    let mut stem = file.clone();
    if lower.ends_with(".hdf") && lower.len() > 4 {
        let without = &file[..file.len() - 4];
        if let Some((head, tail)) = without.rsplit_once('.') {
            let t = tail.to_ascii_lowercase();
            if (t.starts_with('p') || t.starts_with('g'))
                && t.len() > 1
                && t[1..].bytes().all(|b| b.is_ascii_digit())
            {
                stem = head.to_string();
            }
        }
    }
    if stem == file {
        stem = Path::new(&file).file_stem()?.to_string_lossy().to_string();
    }
    let matches: Vec<&PathBuf> = maps
        .iter()
        .filter(|p| {
            p.file_stem()
                .is_some_and(|s| s.to_string_lossy().to_lowercase() == stem.to_lowercase())
        })
        .collect();
    if matches.len() == 1 {
        return Some(matches[0].clone());
    }
    if maps.len() == 1 {
        return Some(maps[0].clone());
    }
    None
}

/// `HdfUtils.resolve_ras_relative_path`.
fn ras_relative(base: &Path, filename: &str) -> PathBuf {
    let text = filename.trim();
    let b = text.as_bytes();
    let drive = b.len() > 2
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && (b[2] == b'\\' || b[2] == b'/');
    if drive || text.starts_with("\\\\") || text.starts_with("//") {
        return PathBuf::from(text);
    }
    let relative = PathBuf::from(text.replace('\\', "/"));
    if relative.is_absolute() {
        return relative;
    }
    base.join(relative)
}

/// Largest projection file read; real `.prj` files are a few KiB.
const MAX_PRJ_BYTES: u64 = 1 << 20;

fn read_prj(path: &Path) -> Result<String> {
    if std::fs::metadata(path)?.len() > MAX_PRJ_BYTES {
        return Err(Error::unsupported(format!(
            "projection file {} is larger than 1 MiB",
            path.display()
        )));
    }
    let raw = std::fs::read(path)?;
    let raw = raw.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&raw);
    Ok(match std::str::from_utf8(raw) {
        Ok(s) => s.to_string(),
        Err(_) => raw.iter().map(|&b| b as char).collect(),
    })
}

/// Readers for evidence files other than the plan HDF: another HDF's
/// `Projection` attribute, and a GeoTIFF's explicit EPSG key.
pub struct EvidenceReaders<'a> {
    pub hdf_projection: &'a dyn Fn(&Path) -> Result<Option<String>>,
    pub raster_epsg: &'a dyn Fn(&Path) -> Result<u32>,
}

/// `HdfUtils.read_crs_evidence` for a file named by the `.rasmap`, dispatched
/// by suffix as Python does. A missing file, or a suffix Python cannot read,
/// does not vote (as in Python). A file that exists but cannot be read is
/// refused here, where Python lets it not vote: the gate refuses what it
/// cannot check. A `.vrt` layer is refused (no GDAL here).
fn file_evidence(
    source: &'static str,
    path: PathBuf,
    readers: &EvidenceReaders,
) -> Result<Evidence> {
    let mut e = Evidence {
        source,
        path,
        wkt: None,
        epsg: None,
    };
    if !e.path.is_file() {
        return Ok(e);
    }
    let suffix = e
        .path
        .extension()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let refuse = |err: Error| {
        Error::unsupported(format!(
            "{source} ({}) cannot be read as CRS evidence: {}",
            e.path.display(),
            err.message
        ))
    };
    match suffix.as_str() {
        "hdf" | "h5" | "hdf5" => {
            e.wkt = found_wkt((readers.hdf_projection)(&e.path).map_err(refuse)?);
        }
        "prj" => e.wkt = found_wkt(Some(read_prj(&e.path).map_err(refuse)?)),
        "tif" | "tiff" => e.epsg = Some((readers.raster_epsg)(&e.path).map_err(refuse)?),
        "vrt" => {
            return Err(refuse(Error::unsupported(
                "a VRT's CRS needs GDAL; this preparation cannot check it",
            )));
        }
        _ => {}
    }
    Ok(e)
}

fn found_wkt(w: Option<String>) -> Option<String> {
    w.map(|w| w.trim().to_string()).filter(|w| !w.is_empty())
}

/// `HdfUtils.collect_hdf_crs_evidence(hdf_path)`: the HDF attribute (given),
/// then the selected `.rasmap`'s projection file and terrain layers. A
/// `.rasmap` that exists but is not UTF-8 XML is refused (Python records it
/// unreadable and lets it not vote).
pub fn model_crs_evidence(
    hdf_attribute: Option<String>,
    hdf_path: &Path,
    readers: &EvidenceReaders,
) -> Result<Vec<Evidence>> {
    let mut evidence = vec![Evidence {
        source: "hdf_attribute",
        path: hdf_path.to_path_buf(),
        wkt: found_wkt(hdf_attribute),
        epsg: None,
    }];
    let Some(rasmap) = select_rasmap(hdf_path).filter(|p| p.is_file()) else {
        return Ok(evidence);
    };
    let unreadable = |why: String| {
        Error::unsupported(format!(
            "RASMapper file {} cannot be read for CRS evidence: {why}",
            rasmap.display()
        ))
    };
    let raw = std::fs::read(&rasmap)?;
    let text = std::str::from_utf8(raw.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&raw))
        .map_err(|_| unreadable("it is not UTF-8".into()))?;
    let doc = roxmltree::Document::parse(text).map_err(|e| unreadable(e.to_string()))?;
    let base = rasmap.parent().unwrap_or(Path::new("")).to_path_buf();
    let root = doc.root_element();
    if let Some(projection) = root
        .descendants()
        .find(|n| n.has_tag_name("RASProjectionFilename"))
        .and_then(|n| n.attribute("Filename"))
        .filter(|f| !f.is_empty())
    {
        evidence.push(file_evidence(
            "rasmap_projection",
            ras_relative(&base, projection),
            readers,
        )?);
    }
    for terrains in root.children().filter(|n| n.has_tag_name("Terrains")) {
        for layer in terrains.children().filter(|n| n.has_tag_name("Layer")) {
            if let Some(file) = layer.attribute("Filename").filter(|f| !f.is_empty()) {
                evidence.push(file_evidence(
                    "terrain_hdf",
                    ras_relative(&base, file),
                    readers,
                )?);
            }
        }
    }
    Ok(evidence)
}

/// Every CRS evidence item found must be the terrain's EPSG CRS, and at least
/// one must be found (ras-commander refuses a missing or conflicting CRS).
pub fn require_model_crs(evidence: &[Evidence], epsg: u32) -> Result<()> {
    let mut any = false;
    for e in evidence {
        if let Some(code) = e.epsg {
            any = true;
            if code != epsg {
                return Err(Error::unsupported(format!(
                    "{} ({}): EPSG:{code} is not the terrain CRS EPSG:{epsg}",
                    e.source,
                    e.path.display()
                )));
            }
        }
        if let Some(w) = &e.wkt {
            any = true;
            require_epsg(Some(w), epsg).map_err(|err| {
                Error::unsupported(format!(
                    "{} ({}): {}",
                    e.source,
                    e.path.display(),
                    err.message
                ))
            })?;
        }
    }
    if !any {
        return Err(Error::unsupported(
            "the hydraulic results CRS is missing: no Projection in the plan HDF or its .rasmap evidence",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The UPGU1/UPGU3 plan HDF `Projection` (ESRI WKT1 of EPSG:2278).
    const UPGU: &str = r#"PROJCS["NAD_1983_StatePlane_Texas_South_Central_FIPS_4204_Feet",GEOGCS["GCS_North_American_1983",DATUM["D_North_American_1983",SPHEROID["GRS_1980",6378137.0,298.257222101]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]],PROJECTION["Lambert_Conformal_Conic"],PARAMETER["False_Easting",1968500.0],PARAMETER["False_Northing",13123333.33333333],PARAMETER["Central_Meridian",-99.0],PARAMETER["Standard_Parallel_1",28.38333333333333],PARAMETER["Standard_Parallel_2",30.28333333333333],PARAMETER["Latitude_Of_Origin",27.83333333333333],UNIT["Foot_US",0.3048006096012192]]"#;
    const UNIT_FT: &str = "UNIT[\"Foot_US\",0.3048006096012192]";

    #[test]
    fn upgu_projection_is_epsg_2278() {
        require_epsg(Some(UPGU), 2278).unwrap();
        let tail = format!(
            "{UNIT_FT},AXIS[\"Easting\",EAST],AXIS[\"Northing\",NORTH],AUTHORITY[\"EPSG\",\"2278\"]"
        );
        require_epsg(Some(&UPGU.replacen(UNIT_FT, &tail, 1)), 2278).unwrap();
    }

    #[test]
    fn other_codes_and_changed_definitions_are_refused() {
        assert!(require_epsg(Some(UPGU), 2277).is_err());
        let cases = [
            ("1968500.0".to_string(), "1968501.0".to_string()),
            (
                "D_North_American_1983".into(),
                "D_North_American_1983_HARN".into(),
            ),
            ("Lambert_Conformal_Conic".into(), "Mercator".into()),
            (
                "298.257222101]]".into(),
                "298.257222101],TOWGS84[1,2,3,0,0,0,0]]".into(),
            ),
            (
                "UNIT[\"Degree\",0.0174532925199433]".into(),
                "UNIT[\"Grad\",0.015707963267949]".into(),
            ),
            (
                UNIT_FT.into(),
                format!("{UNIT_FT},AXIS[\"Northing\",NORTH],AXIS[\"Easting\",EAST]"),
            ),
            (
                UNIT_FT.into(),
                format!("{UNIT_FT},EXTENSION[\"PROJ4\",\"+proj=longlat\"]"),
            ),
            (
                UNIT_FT.into(),
                format!("{UNIT_FT},AUTHORITY[\"EPSG\",\"2277\"]"),
            ),
        ];
        for (from, to) in cases {
            let changed = UPGU.replacen(from.as_str(), to.as_str(), 1);
            assert_ne!(changed, UPGU, "{from}");
            assert!(require_epsg(Some(&changed), 2278).is_err(), "{to}");
        }
        assert!(require_epsg(None, 2278).is_err());
        assert!(require_epsg(Some("  "), 2278).is_err());
        let deep = "PROJCS[".repeat(200);
        assert!(require_epsg(Some(&deep), 2278).is_err());
    }

    #[test]
    fn aoi_crs_needs_a_declared_projected_code() {
        let wkt2 = r#"PROJCRS["NAD83 / Texas South Central (ftUS)",BASEGEOGCRS["NAD83",DATUM["North American Datum 1983",ELLIPSOID["GRS 1980",6378137,298.257222101,LENGTHUNIT["metre",1]]],PRIMEM["Greenwich",0,ANGLEUNIT["degree",0.0174532925199433]],ID["EPSG",4269]],CONVERSION["unnamed",METHOD["Lambert Conic Conformal (2SP)",ID["EPSG",9802]]],CS[Cartesian,2],AXIS["easting (X)",east,ORDER[1],LENGTHUNIT["US survey foot",0.304800609601219]],AXIS["northing (Y)",north,ORDER[2],LENGTHUNIT["US survey foot",0.304800609601219]],ID["EPSG",2278]]"#;
        assert_eq!(declared_projected_epsg(wkt2), Some(2278));
        assert_eq!(declared_projected_epsg(UPGU), None);
        let bound = format!("BOUNDCRS[SOURCECRS[{wkt2}],TARGETCRS[GEOGCRS[\"WGS 84\"]]]");
        assert_eq!(declared_projected_epsg(&bound), None);
    }

    #[test]
    fn rasmap_evidence_must_agree_with_the_terrain() {
        let folder = std::env::temp_dir().join(format!("fmp-rasmap-{}", std::process::id()));
        std::fs::create_dir_all(folder.join("GIS")).unwrap();
        let hdf = folder.join("Model.p01.hdf");
        std::fs::write(&hdf, b"").unwrap();
        let other = UPGU.replacen("1968500.0", "1640416.6667", 1);
        std::fs::write(folder.join("GIS").join("other.prj"), other).unwrap();
        std::fs::write(
            folder.join("Model.rasmap"),
            "<RASMapper><RASProjectionFilename Filename=\".\\GIS\\other.prj\" /><Terrains /></RASMapper>",
        )
        .unwrap();
        let none = |_: &Path| -> Result<Option<String>> { Ok(None) };
        let epsg_2277 = |_: &Path| -> Result<u32> { Ok(2277) };
        let readers = EvidenceReaders {
            hdf_projection: &none,
            raster_epsg: &epsg_2277,
        };
        let evidence = model_crs_evidence(Some(UPGU.into()), &hdf, &readers).unwrap();
        assert_eq!(evidence.len(), 2);
        assert!(require_model_crs(&evidence, 2278).is_err());
        std::fs::write(folder.join("GIS").join("other.prj"), UPGU).unwrap();
        let evidence = model_crs_evidence(Some(UPGU.into()), &hdf, &readers).unwrap();
        require_model_crs(&evidence, 2278).unwrap();
        // A blank root attribute is absent; the .rasmap alone can resolve it.
        let evidence = model_crs_evidence(Some("   ".into()), &hdf, &readers).unwrap();
        require_model_crs(&evidence, 2278).unwrap();
        // A GeoTIFF terrain layer votes with its EPSG key; a VRT is refused.
        std::fs::write(folder.join("GIS").join("t.tif"), b"").unwrap();
        std::fs::write(folder.join("GIS").join("t.vrt"), b"").unwrap();
        let layers = |layer: &str| {
            std::fs::write(
                folder.join("Model.rasmap"),
                format!("<RASMapper><RASProjectionFilename Filename=\".\\GIS\\other.prj\" /><Terrains><Layer Filename=\".\\GIS\\{layer}\" /></Terrains></RASMapper>"),
            )
            .unwrap();
        };
        layers("t.tif");
        let evidence = model_crs_evidence(Some(UPGU.into()), &hdf, &readers).unwrap();
        assert_eq!(evidence.len(), 3);
        assert!(require_model_crs(&evidence, 2278).is_err());
        layers("t.vrt");
        assert!(model_crs_evidence(Some(UPGU.into()), &hdf, &readers).is_err());
        // A .rasmap that exists but is not UTF-8 is refused, not ignored.
        std::fs::write(folder.join("Model.rasmap"), b"<RASMapper>\xe9</RASMapper>").unwrap();
        assert!(model_crs_evidence(Some(UPGU.into()), &hdf, &readers).is_err());
        std::fs::remove_file(folder.join("Model.rasmap")).unwrap();
        let evidence = model_crs_evidence(Some("   ".into()), &hdf, &readers).unwrap();
        assert!(require_model_crs(&evidence, 2278).is_err());
        std::fs::remove_dir_all(&folder).ok();
    }

    #[test]
    fn rasmap_selection_follows_python_glob() {
        let folder = std::env::temp_dir().join(format!("fmp-select-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let hdf = folder.join("UPGU1.p02.hdf");
        std::fs::write(folder.join("Other.rasmap"), b"").unwrap();
        std::fs::write(folder.join("old.RASMAP"), b"").unwrap();
        // POSIX glob is case-sensitive: one map, selected. Windows sees two, neither
        // matching the project stem: ambiguous, none selected.
        let selected = select_rasmap(&hdf);
        if cfg!(windows) {
            assert_eq!(selected, None);
        } else {
            assert_eq!(selected, Some(folder.join("Other.rasmap")));
        }
        std::fs::write(folder.join("upgu1.rasmap"), b"").unwrap();
        assert_eq!(select_rasmap(&hdf), Some(folder.join("upgu1.rasmap")));
        std::fs::remove_dir_all(&folder).ok();
    }

    #[test]
    fn bare_words_are_refused_outside_axis() {
        let junk = UPGU.replacen("0.3048006096012192]", "0.3048006096012192,junk]", 1);
        assert!(require_epsg(Some(&junk), 2278).is_err());
        require_epsg(Some(UPGU), 2278).unwrap();
    }
}
