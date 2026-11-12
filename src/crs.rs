//! Output CRS of engine-written GeoTIFFs: an EPSG code, or a GeoTIFF GeoKey
//! directory copied verbatim.
//!
//! An EPSG code is written as before (`ProjectedCSTypeGeoKey` or
//! `GeographicTypeGeoKey` holding the code). Any other CRS, such as
//! ESRI:102630 (NAD83 / Alabama West, US survey feet), has no code to write.
//! For those the request carries the CRS as a [`GeoKeys`] triple: the
//! GeoKeyDirectoryTag (34735), GeoDoubleParamsTag (34736) and
//! GeoAsciiParamsTag (34737) values of a GeoTIFF already in that CRS, read
//! with [`read_geokeys`] (printed by `ras2fim-batch terrain-info`). The engine
//! writes the three tags back unchanged, so a reader of the output (GDAL,
//! rasterio, terrio) decodes the same keys it decodes from the source: the CRS
//! is equal by construction, units and datum included. No WKT or PROJ string
//! is converted to GeoKeys here, so no CRS is re-encoded or approximated.
//!
//! Only the georeferencing model is checked ([`GeoKeys::validate`]): the
//! directory must be well formed, name a projected, geographic or
//! user-defined model, and declare pixels as areas (or not declare them), as
//! every engine writer places the tie point at the outer pixel corner.
use crate::MapError;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Arc;

/// GeoKeyDirectoryTag.
pub const GEO_KEY_DIRECTORY: u16 = 34735;
/// GeoDoubleParamsTag.
pub const GEO_DOUBLE_PARAMS: u16 = 34736;
/// GeoAsciiParamsTag.
pub const GEO_ASCII_PARAMS: u16 = 34737;
/// Capability identifiers advertised by methods that accept [`GeoKeys`].
pub const CRS_IDENTITY_KINDS: [&str; 4] = ["epsg", "caller_verified_wkt", "geokeys", "untagged"];
pub const OUTPUT_CRS_KINDS: [&str; 2] = ["epsg", "geokeys"];

const MAX_KEYS: usize = 1024;
const MAX_DOUBLES: usize = 4096;
const MAX_ASCII: usize = 65_535;
const MAX_IFD_ENTRIES: u64 = 4096;

/// The three GeoKey tags of a GeoTIFF, as stored.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeoKeys {
    /// GeoKeyDirectoryTag SHORT values: the header `[1, 1, minor, n]` and `n`
    /// entries `[key, location, count, value or offset]`.
    pub directory: Vec<u16>,
    /// GeoDoubleParamsTag values (empty: no tag). In JSON each value is a
    /// decimal **string** (Rust's shortest round-trip form, such as
    /// `"0.999933333333333"`), parsed with the correctly rounded `str::parse`:
    /// serde_json's default number parser can be one unit in the last place
    /// off for 17-digit decimals, which would not be verbatim. JSON numbers
    /// are refused.
    #[serde(default, with = "exact_doubles")]
    pub doubles: Vec<f64>,
    /// GeoAsciiParamsTag text without the TIFF terminating NUL (empty: no tag).
    #[serde(default)]
    pub ascii: String,
}

/// `GeoKeys::doubles` as exact decimal strings.
mod exact_doubles {
    use serde::de::Error;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(values: &[f64], s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(values.iter().map(|v| format!("{v:?}")))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<f64>, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Item {
            Text(String),
            Number(serde_json::Number),
        }
        Vec::<Item>::deserialize(d)?
            .into_iter()
            .map(|item| match item {
                Item::Text(text) => text
                    .trim()
                    .parse::<f64>()
                    .map_err(|e| D::Error::custom(format!("crs_geokeys.doubles: {text:?}: {e}"))),
                Item::Number(n) => Err(D::Error::custom(format!(
                    "crs_geokeys.doubles must be decimal strings (got the JSON number {n}); \
                     copy them from `terrain-info`"
                ))),
            })
            .collect()
    }
}

impl GeoKeys {
    /// The keys of an EPSG code, as the engine has always written them: model
    /// type, raster type PixelIsArea and the CRS code.
    pub fn from_epsg(code: u16, geographic: bool) -> Self {
        Self {
            directory: vec![
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
            ],
            doubles: Vec::new(),
            ascii: String::new(),
        }
    }

    fn entries(&self) -> impl Iterator<Item = &[u16]> {
        self.directory.get(4..).unwrap_or(&[]).chunks(4)
    }

    /// `(location, count, value or offset)` of key `id`.
    pub fn key(&self, id: u16) -> Option<(u16, u16, u16)> {
        self.entries()
            .find(|k| k.len() == 4 && k[0] == id)
            .map(|k| (k[1], k[2], k[3]))
    }

    /// The SHORT value of key `id` stored in the directory.
    pub fn short(&self, id: u16) -> Option<u16> {
        self.key(id).filter(|k| k.0 == 0).map(|k| k.2)
    }

    /// Check that the triple is a well-formed GeoKey directory that a reader
    /// decodes as a CRS, before any read. Never alters it.
    pub fn validate(&self) -> Result<(), MapError> {
        let bad = |m: &str| MapError::Invalid(format!("crs_geokeys: {m}"));
        let d = &self.directory;
        if d.len() < 8 || d[0] != 1 || d[1] != 1 || d[2] > 1 {
            return Err(bad(
                "directory must start with header [1, 1, 0|1, n] and hold at least one key",
            ));
        }
        let n = usize::from(d[3]);
        if n == 0 || n > MAX_KEYS || d.len() != 4 + 4 * n {
            return Err(bad("directory length does not match its key count"));
        }
        if self.doubles.len() > MAX_DOUBLES || self.ascii.len() > MAX_ASCII {
            return Err(bad("double or ASCII parameters too large"));
        }
        if !self.doubles.iter().all(|v| v.is_finite()) {
            return Err(bad("double parameters must be finite"));
        }
        if !self.ascii.is_ascii() {
            return Err(bad("ASCII parameters must be 7-bit ASCII"));
        }
        let mut last = None;
        for k in self.entries() {
            let (id, loc, count, off) = (k[0], k[1], usize::from(k[2]), usize::from(k[3]));
            if last.is_some_and(|l| id <= l) {
                return Err(bad("keys must be unique and in ascending order"));
            }
            last = Some(id);
            let ok = match loc {
                0 => count == 1,
                GEO_DOUBLE_PARAMS => count >= 1 && off + count <= self.doubles.len(),
                GEO_ASCII_PARAMS => count >= 1 && off + count <= self.ascii.len(),
                _ => false,
            };
            if !ok {
                return Err(bad(&format!(
                    "key {id} location {loc} count {count} offset {off} is out of range or unsupported"
                )));
            }
        }
        let model = self
            .short(1024)
            .ok_or_else(|| bad("GTModelTypeGeoKey (1024) missing: the keys describe no CRS"))?;
        match model {
            1 if self.key(3072).is_some() => {}
            2 if self.key(2048).is_some() => {}
            32767 => {}
            1 | 2 => {
                return Err(bad(
                    "a projected model needs ProjectedCSTypeGeoKey (3072), a geographic one GeographicTypeGeoKey (2048)",
                ));
            }
            other => {
                return Err(bad(&format!(
                    "GTModelTypeGeoKey {other} is not projected (1), geographic (2) or user-defined (32767)"
                )));
            }
        }
        if self.key(1025).is_some_and(|k| k != (0, 1, 1)) {
            return Err(bad(
                "GTRasterTypeGeoKey (1025) must be PixelIsArea (1) or absent: engine products are pixel areas",
            ));
        }
        Ok(())
    }

    /// FNV-1a 64 of the three tags' little-endian payloads (ASCII with its NUL),
    /// in tag order: the identity reported for the written CRS.
    pub fn fnv1a64(&self) -> String {
        let mut bytes: Vec<u8> = self
            .directory
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        bytes.extend(self.doubles.iter().flat_map(|v| v.to_le_bytes()));
        bytes.extend(self.ascii.as_bytes());
        bytes.push(0);
        format!("{:016x}", crate::sink::fnv1a64(&bytes))
    }
}

/// CRS written to engine GeoTIFFs.
#[derive(Clone, Debug, PartialEq)]
pub enum OutputCrs {
    Epsg(u32),
    GeoKeys(Arc<GeoKeys>),
}

impl OutputCrs {
    /// The output CRS of a request: its `epsg` (0 when absent) or its
    /// `crs_geokeys`, never both. `epsg` 0 without keys stays `Epsg(0)`, which
    /// each method refuses as before.
    pub fn resolve(epsg: u32, geokeys: Option<&GeoKeys>) -> Result<Self, MapError> {
        match (epsg, geokeys) {
            (0, Some(keys)) => {
                keys.validate()?;
                Ok(Self::GeoKeys(Arc::new(keys.clone())))
            }
            (_, Some(_)) => Err(MapError::Invalid(
                "give either epsg or crs_geokeys, not both".into(),
            )),
            (code, None) => Ok(Self::Epsg(code)),
        }
    }

    pub fn epsg(&self) -> Option<u32> {
        match self {
            Self::Epsg(code) => Some(*code),
            Self::GeoKeys(_) => None,
        }
    }

    pub fn geokeys(&self) -> Option<&GeoKeys> {
        match self {
            Self::Epsg(_) => None,
            Self::GeoKeys(keys) => Some(keys),
        }
    }

    /// Report record: `{"kind":"epsg","code":..}` or
    /// `{"kind":"geokeys","fnv1a64":..}`.
    pub fn echo(&self) -> Value {
        match self {
            Self::Epsg(code) => json!({"kind": "epsg", "code": code}),
            Self::GeoKeys(keys) => json!({"kind": "geokeys", "fnv1a64": keys.fnv1a64()}),
        }
    }
}

/// Advertise `crs_geokeys` on a method record: `crs_any`, `crs_identity_kinds`,
/// `output_crs_kinds` and the `crs_geokeys` contract.
pub fn advertise(record: &mut Value) {
    record["crs_any"] = json!(true);
    record["crs_identity_kinds"] = json!(CRS_IDENTITY_KINDS);
    record["output_crs_kinds"] = json!(OUTPUT_CRS_KINDS);
    record["crs_geokeys"] = capability();
}

/// The `crs_geokeys` contract.
pub fn capability() -> Value {
    json!({
        "request_field": "crs_geokeys {directory, doubles, ascii}, with epsg 0 or absent; doubles are decimal strings, parsed exactly",
        "source": "the GeoKey tags of a GeoTIFF in the CRS, as `terrain-info` prints them (`geokeys`)",
        "written": "GeoKeyDirectoryTag, GeoDoubleParamsTag and GeoAsciiParamsTag copied verbatim; no WKT or PROJ conversion",
        "terrain_identity": "crs_identity geokeys: the terrain's own GeoKey tags equal crs_geokeys exactly; untagged: the terrain carries no CRS at all (RAS Mapper terrain sources in a CRS without an EPSG code) and crs_geokeys declares it; caller_verified_wkt: the pinned WKT hash as before; epsg: refused with crs_geokeys",
        "refused": "a directory without a projected, geographic or user-defined model, PixelIsPoint, or keys stored outside the double/ASCII tags"
    })
}

struct Reader {
    file: File,
    little: bool,
    big: bool,
}

impl Reader {
    fn open(path: &Path, write: bool) -> Result<(Self, u64), MapError> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(write)
            .open(path)
            .map_err(|e| MapError::Io(format!("{}: {e}", path.display())))?;
        let mut head = [0_u8; 16];
        let got = file.read(&mut head)?;
        let little = match &head[..2] {
            b"II" => true,
            b"MM" => false,
            _ => return Err(not_tiff(path)),
        };
        let mut r = Self {
            file,
            little,
            big: false,
        };
        let version = r.u16_at(&head[2..4]);
        let first = match version {
            42 if got >= 8 => u64::from(r.u32_at(&head[4..8])),
            43 if got >= 16 && r.u16_at(&head[4..6]) == 8 => {
                r.big = true;
                r.u64_at(&head[8..16])
            }
            _ => return Err(not_tiff(path)),
        };
        Ok((r, first))
    }

    fn u16_at(&self, b: &[u8]) -> u16 {
        let a = [b[0], b[1]];
        if self.little {
            u16::from_le_bytes(a)
        } else {
            u16::from_be_bytes(a)
        }
    }

    fn u32_at(&self, b: &[u8]) -> u32 {
        let a = [b[0], b[1], b[2], b[3]];
        if self.little {
            u32::from_le_bytes(a)
        } else {
            u32::from_be_bytes(a)
        }
    }

    fn u64_at(&self, b: &[u8]) -> u64 {
        let a: [u8; 8] = b[..8].try_into().unwrap();
        if self.little {
            u64::from_le_bytes(a)
        } else {
            u64::from_be_bytes(a)
        }
    }

    fn read_exact_at(&mut self, at: u64, n: usize) -> Result<Vec<u8>, MapError> {
        let mut out = vec![0_u8; n];
        self.file.seek(SeekFrom::Start(at))?;
        self.file.read_exact(&mut out)?;
        Ok(out)
    }

    /// The first IFD's raw entries `(tag, type, count, value field)` and its
    /// next-IFD field.
    fn ifd(&mut self, at: u64) -> Result<(Vec<Entry>, Vec<u8>), MapError> {
        let (count_len, entry_len, next_len) = if self.big { (8, 20, 8) } else { (2, 12, 4) };
        let head = self.read_exact_at(at, count_len)?;
        let n = if self.big {
            self.u64_at(&head)
        } else {
            u64::from(self.u16_at(&head))
        };
        if n == 0 || n > MAX_IFD_ENTRIES {
            return Err(MapError::Unsupported(format!(
                "TIFF directory with {n} entries"
            )));
        }
        let body = self.read_exact_at(at + count_len as u64, n as usize * entry_len + next_len)?;
        let mut entries = Vec::with_capacity(n as usize);
        for e in body[..n as usize * entry_len].chunks(entry_len) {
            let (tag, kind) = (self.u16_at(&e[0..2]), self.u16_at(&e[2..4]));
            let (count, value) = if self.big {
                (self.u64_at(&e[4..12]), e[12..20].to_vec())
            } else {
                (u64::from(self.u32_at(&e[4..8])), e[8..12].to_vec())
            };
            entries.push(Entry {
                tag,
                kind,
                count,
                value,
            });
        }
        Ok((entries, body[n as usize * entry_len..].to_vec()))
    }

    /// Payload bytes of an entry, inline or at its offset (bounded).
    fn payload(&mut self, e: &Entry, unit: usize) -> Result<Vec<u8>, MapError> {
        let n = usize::try_from(e.count)
            .ok()
            .and_then(|c| c.checked_mul(unit))
            .filter(|n| *n <= 1 << 20)
            .ok_or_else(|| MapError::Unsupported(format!("TIFF tag {} too large", e.tag)))?;
        if n <= e.value.len() {
            return Ok(e.value[..n].to_vec());
        }
        let at = if self.big {
            self.u64_at(&e.value)
        } else {
            u64::from(self.u32_at(&e.value))
        };
        self.read_exact_at(at, n)
    }
}

struct Entry {
    tag: u16,
    kind: u16,
    count: u64,
    value: Vec<u8>,
}

fn not_tiff(path: &Path) -> MapError {
    MapError::Unsupported(format!("{}: not a TIFF", path.display()))
}

/// The GeoKey tags of a TIFF's first image, as stored; `None` without a
/// GeoKeyDirectoryTag. Not validated (see [`GeoKeys::validate`]).
pub fn read_geokeys(path: &Path) -> Result<Option<GeoKeys>, MapError> {
    let (mut r, first) = Reader::open(path, false)?;
    let (entries, _) = r.ifd(first)?;
    let find = |tag: u16| entries.iter().find(|e| e.tag == tag);
    let Some(dir) = find(GEO_KEY_DIRECTORY) else {
        return Ok(None);
    };
    let wrong = |tag: u16| {
        MapError::Unsupported(format!(
            "{}: GeoKey tag {tag} has an unexpected TIFF type",
            path.display()
        ))
    };
    if dir.kind != 3 {
        return Err(wrong(GEO_KEY_DIRECTORY));
    }
    let directory = r.payload(dir, 2)?.chunks(2).map(|b| r.u16_at(b)).collect();
    let doubles = match find(GEO_DOUBLE_PARAMS) {
        None => Vec::new(),
        Some(e) if e.kind == 12 => r
            .payload(e, 8)?
            .chunks(8)
            .map(|b| f64::from_bits(r.u64_at(b)))
            .collect(),
        Some(_) => return Err(wrong(GEO_DOUBLE_PARAMS)),
    };
    let ascii = match find(GEO_ASCII_PARAMS) {
        None => String::new(),
        Some(e) if e.kind == 2 => {
            let mut bytes = r.payload(e, 1)?;
            if bytes.last() == Some(&0) {
                bytes.pop();
            }
            String::from_utf8(bytes).map_err(|_| wrong(GEO_ASCII_PARAMS))?
        }
        Some(_) => return Err(wrong(GEO_ASCII_PARAMS)),
    };
    Ok(Some(GeoKeys {
        directory,
        doubles,
        ascii,
    }))
}

/// Add `keys` to a TIFF written without GeoKeys (terrio writes none for an
/// unknown CRS): its first directory is rewritten at the end of the file with
/// the three tags inserted in tag order, and the header points to it. Pixel
/// data, other tags and later directories are unchanged.
pub fn insert_geokeys(path: &Path, keys: &GeoKeys) -> Result<(), MapError> {
    keys.validate()?;
    let (mut r, first) = Reader::open(path, true)?;
    let (mut entries, next) = r.ifd(first)?;
    if entries
        .iter()
        .any(|e| (GEO_KEY_DIRECTORY..=GEO_ASCII_PARAMS).contains(&e.tag))
    {
        return Err(MapError::Internal(format!(
            "{}: GeoKeys already written",
            path.display()
        )));
    }
    let little = r.little;
    let big = r.big;
    let short = |v: u16| {
        if little {
            v.to_le_bytes()
        } else {
            v.to_be_bytes()
        }
    };
    let double = |v: f64| {
        if little {
            v.to_le_bytes()
        } else {
            v.to_be_bytes()
        }
    };
    let mut payloads: Vec<(u16, u16, u64, Vec<u8>)> = vec![(
        GEO_KEY_DIRECTORY,
        3,
        keys.directory.len() as u64,
        keys.directory.iter().flat_map(|v| short(*v)).collect(),
    )];
    if !keys.doubles.is_empty() {
        payloads.push((
            GEO_DOUBLE_PARAMS,
            12,
            keys.doubles.len() as u64,
            keys.doubles.iter().flat_map(|v| double(*v)).collect(),
        ));
    }
    if !keys.ascii.is_empty() {
        let mut bytes = keys.ascii.as_bytes().to_vec();
        bytes.push(0);
        payloads.push((GEO_ASCII_PARAMS, 2, bytes.len() as u64, bytes));
    }
    let inline = if big { 8 } else { 4 };
    let mut end = r.file.seek(SeekFrom::End(0))?;
    let offset_bytes = |v: u64| -> Result<Vec<u8>, MapError> {
        Ok(match (big, little) {
            (true, true) => v.to_le_bytes().to_vec(),
            (true, false) => v.to_be_bytes().to_vec(),
            (false, _) => {
                let v = u32::try_from(v).map_err(|_| {
                    MapError::Budget("classic TIFF exceeds 4 GiB after GeoKeys".into())
                })?;
                if little {
                    v.to_le_bytes().to_vec()
                } else {
                    v.to_be_bytes().to_vec()
                }
            }
        })
    };
    for (tag, kind, count, mut bytes) in payloads {
        let value = if bytes.len() <= inline {
            bytes.resize(inline, 0);
            bytes
        } else {
            if end % 2 == 1 {
                r.file.write_all(&[0])?;
                end += 1;
            }
            r.file.write_all(&bytes)?;
            let at = end;
            end += bytes.len() as u64;
            offset_bytes(at)?
        };
        entries.push(Entry {
            tag,
            kind,
            count,
            value,
        });
    }
    entries.sort_by_key(|e| e.tag);
    if end % 2 == 1 {
        r.file.write_all(&[0])?;
        end += 1;
    }
    let ifd_at = end;
    let mut ifd = Vec::new();
    for e in &entries {
        ifd.extend(short(e.tag));
        ifd.extend(short(e.kind));
        if big {
            ifd.extend(if little {
                e.count.to_le_bytes()
            } else {
                e.count.to_be_bytes()
            });
        } else {
            ifd.extend(offset_bytes(e.count)?);
        }
        ifd.extend(&e.value);
    }
    let mut out = if big {
        offset_bytes(entries.len() as u64)?
    } else {
        short(entries.len() as u16).to_vec()
    };
    out.extend(ifd);
    out.extend(next);
    r.file.write_all(&out)?;
    r.file.seek(SeekFrom::Start(if big { 8 } else { 4 }))?;
    r.file.write_all(&offset_bytes(ifd_at)?)?;
    r.file.flush()?;
    r.file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// GDAL's keys for ESRI:102630, as RAS Mapper writes them.
    pub(crate) fn alabama_west() -> GeoKeys {
        GeoKeys {
            directory: vec![
                1, 1, 0, 18, 1024, 0, 1, 1, 1025, 0, 1, 1, 1026, 34737, 48, 0, 2048, 0, 1, 4269,
                2049, 34737, 6, 48, 2054, 0, 1, 9102, 2057, 34736, 1, 6, 2059, 34736, 1, 5, 3059,
                0, 1, 1, 3072, 0, 1, 32767, 3074, 0, 1, 32767, 3075, 0, 1, 1, 3076, 0, 1, 9003,
                3080, 34736, 1, 1, 3081, 34736, 1, 0, 3082, 34736, 1, 3, 3083, 34736, 1, 4, 3092,
                34736, 1, 2,
            ],
            doubles: vec![
                30.0,
                -87.5,
                0.999933333333333,
                1968500.0,
                0.0,
                298.257222101,
                6378137.0,
            ],
            ascii: "NAD_1983_StatePlane_Alabama_West_FIPS_0102_Feet|NAD83|".into(),
        }
    }

    #[test]
    fn validates_well_formed_keys_and_refuses_the_rest() {
        alabama_west().validate().unwrap();
        GeoKeys::from_epsg(2277, false).validate().unwrap();
        let refuse = |f: &dyn Fn(&mut GeoKeys)| {
            let mut k = alabama_west();
            f(&mut k);
            assert!(k.validate().is_err());
        };
        refuse(&|k| k.directory[3] = 17);
        refuse(&|k| k.directory[0] = 2);
        refuse(&|k| k.doubles.truncate(6));
        refuse(&|k| k.ascii.truncate(50));
        refuse(&|k| k.doubles[0] = f64::NAN);
        // PixelIsPoint.
        refuse(&|k| k.directory[11] = 2);
        // Model type 0 and a local CRS without the projected key.
        refuse(&|k| k.directory[7] = 0);
        refuse(&|k| {
            k.directory[40] = 3071;
        });
        // Keys out of order.
        refuse(&|k| k.directory.swap(4, 8));
        refuse(&|k| k.ascii.push('\u{e9}'));
    }

    #[test]
    fn resolve_takes_one_crs() {
        let keys = alabama_west();
        assert_eq!(
            OutputCrs::resolve(2277, None).unwrap(),
            OutputCrs::Epsg(2277)
        );
        assert!(OutputCrs::resolve(2277, Some(&keys)).is_err());
        let crs = OutputCrs::resolve(0, Some(&keys)).unwrap();
        assert_eq!(crs.epsg(), None);
        assert_eq!(crs.echo()["fnv1a64"], keys.fnv1a64());
    }
}
