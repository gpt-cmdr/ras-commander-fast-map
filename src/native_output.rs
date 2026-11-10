//! Optional native GeoTIFF delivery controls; legacy Rust request types stay intact.
use crate::{MapError, sloping_session::Request};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Read extension-bearing requests without weakening duplicate-key rejection.
pub fn read_wire(reader: impl std::io::Read) -> Result<Value, MapError> {
    serde_json::from_reader::<_, Document>(reader)
        .map(|document| document.0)
        .map_err(|e| MapError::Invalid(e.to_string()))
}

struct Document(Value);
impl<'de> Deserialize<'de> for Document {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Document;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("JSON without duplicate object keys")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Document, A::Error> {
                let mut object = serde_json::Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    if object.contains_key(&key) {
                        return Err(serde::de::Error::custom(format!("duplicate field {key}")));
                    }
                    object.insert(key, map.next_value::<Document>()?.0);
                }
                Ok(Document(Value::Object(object)))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Document, A::Error> {
                let mut values = Vec::new();
                while let Some(value) = seq.next_element::<Document>()? {
                    values.push(value.0);
                }
                Ok(Document(Value::Array(values)))
            }
            fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Document, E> {
                Ok(Document(v.into()))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Document, E> {
                Ok(Document(v.into()))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Document, E> {
                Ok(Document(v.into()))
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Document, E> {
                serde_json::Number::from_f64(v)
                    .map(|n| Document(Value::Number(n)))
                    .ok_or_else(|| E::custom("nonfinite JSON number"))
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Document, E> {
                Ok(Document(v.into()))
            }
            fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Document, E> {
                Ok(Document(v.into()))
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Document, E> {
                Ok(Document(Value::Null))
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub enum Compression {
    #[serde(rename = "NONE")]
    None,
    #[default]
    #[serde(rename = "LZW")]
    Lzw,
    /// Tiled zlib DEFLATE (TIFF compression 8). Selected only with `final_format`.
    #[serde(rename = "DEFLATE")]
    Deflate,
}

impl Compression {
    pub fn name(self) -> &'static str {
        match self {
            Self::None => "NONE",
            Self::Lzw => "LZW",
            Self::Deflate => "DEFLATE",
        }
    }
}

/// Placement of the product window on a larger delivery grid. The window's
/// upper-left pixel lands at (`row_offset`, `col_offset`); every other pixel of
/// the `width` x `height` grid is nodata.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FullGrid {
    pub width: usize,
    pub height: usize,
    pub row_offset: usize,
    pub col_offset: usize,
    /// GDAL geotransform of the full grid: `[x0, dx, 0, y0, 0, dy]`.
    pub transform: [f64; 6],
}
/// Validated transforms are finite, so equality is total.
impl Eq for FullGrid {}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub enum OverviewResampling {
    /// GDAL `AVERAGE` overview arithmetic, each level from the level before it.
    #[default]
    #[serde(rename = "average")]
    Average,
}

/// Final delivery GeoTIFF written once by the engine: tiled DEFLATE, a numeric
/// nodata value, optional full-grid placement and average overviews.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FinalFormat {
    /// Value written for every invalid pixel; finite and exactly Float32.
    pub nodata: f64,
    /// Square tile edge in pixels, a multiple of 16 from 16 through 4096.
    #[serde(default = "default_block_size")]
    pub block_size: usize,
    /// zlib level, 1 through 9.
    #[serde(default = "default_zlevel")]
    pub zlevel: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_grid: Option<FullGrid>,
    /// `[]`, `[2]`, `[2, 4]`, ...: consecutive powers of two.
    #[serde(default)]
    pub overview_factors: Vec<usize>,
    #[serde(default)]
    pub overview_resampling: OverviewResampling,
}
/// A validated nodata value is finite, so equality is total.
impl Eq for FinalFormat {}

fn default_block_size() -> usize {
    256
}

fn default_zlevel() -> u32 {
    6
}

impl FinalFormat {
    pub fn validate(&self) -> Result<(), MapError> {
        let invalid = |m: &str| Err(MapError::Invalid(format!("final_format {m}")));
        if !self.nodata.is_finite() || f64::from(self.nodata as f32) != self.nodata {
            return invalid("nodata must be finite and exactly representable as Float32");
        }
        if !(16..=4096).contains(&self.block_size) || !self.block_size.is_multiple_of(16) {
            return invalid("block_size must be a multiple of 16 from 16 through 4096");
        }
        if !(1..=9).contains(&self.zlevel) {
            return invalid("zlevel must be 1 through 9");
        }
        if self.overview_factors.len() > 16
            || self
                .overview_factors
                .iter()
                .enumerate()
                .any(|(i, f)| *f != 2_usize << i)
        {
            return invalid("overview_factors must be consecutive powers of two from 2");
        }
        if let Some(grid) = &self.full_grid {
            let t = grid.transform;
            if grid.width == 0
                || grid.height == 0
                || grid.width > u32::MAX as usize
                || grid.height > u32::MAX as usize
                || grid.col_offset >= grid.width
                || grid.row_offset >= grid.height
                || t.iter().any(|v| !v.is_finite())
                || t[1] == 0.0
                || t[5] == 0.0
                || t[2] != 0.0
                || t[4] != 0.0
            {
                return invalid("full_grid shape, offsets or transform invalid");
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OutputOptions {
    #[serde(default = "default_products")]
    pub products: Vec<String>,
    #[serde(default)]
    pub compression: Compression,
    /// Opt-in bounded output row buffers for plain corner sloping native TIFFs.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub row_stream_rows: usize,
    /// Opt-in final delivery format; selected together with `compression: "DEFLATE"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_format: Option<FinalFormat>,
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

fn default_products() -> Vec<String> {
    vec!["depth".into(), "wse".into()]
}

impl Default for OutputOptions {
    fn default() -> Self {
        Self {
            products: default_products(),
            compression: Compression::default(),
            row_stream_rows: 0,
            final_format: None,
        }
    }
}

impl OutputOptions {
    pub fn validate(&self) -> Result<(), MapError> {
        if self.row_stream_rows > 4096 {
            return Err(MapError::Invalid(
                "row_stream_rows must be at most 4096".into(),
            ));
        }
        if self.products.is_empty()
            || self.products.len() > 2
            || self.products.iter().any(|p| p != "depth" && p != "wse")
            || (self.products.len() == 2 && self.products[0] == self.products[1])
        {
            return Err(MapError::Invalid(
                "products must be unique selections of depth and wse".into(),
            ));
        }
        if (self.compression == Compression::Deflate) != self.final_format.is_some() {
            return Err(MapError::Unsupported(
                "DEFLATE compression and final_format are selected together".into(),
            ));
        }
        if let Some(format) = &self.final_format {
            format.validate()?;
        }
        Ok(())
    }
}

/// Extract known wire extensions, then apply the unchanged strict base parser.
/// `None` preserves the exact legacy completion header.
pub fn parse_request(mut wire: Value) -> Result<(Request, Option<OutputOptions>), MapError> {
    let object = wire
        .as_object_mut()
        .ok_or_else(|| MapError::Invalid("request must be an object".into()))?;
    let mut options = serde_json::Map::new();
    for name in ["products", "compression", "row_stream_rows", "final_format"] {
        if let Some(value) = object.remove(name) {
            options.insert(name.into(), value);
        }
    }
    let selected = if options.is_empty() {
        None
    } else {
        let selected: OutputOptions = serde_json::from_value(Value::Object(options))
            .map_err(|e| MapError::Invalid(e.to_string()))?;
        selected.validate()?;
        Some(selected)
    };
    let req: Request =
        serde_json::from_value(wire).map_err(|e| MapError::Invalid(e.to_string()))?;
    if selected.is_some() && !req.geotiff_outputs {
        return Err(MapError::Unsupported(
            "products/compression/row_stream_rows/final_format require native geotiff_outputs"
                .into(),
        ));
    }
    Ok((req, selected))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn row_stream_control_is_opt_in_and_bounded() {
        let legacy: OutputOptions = serde_json::from_str("{}").unwrap();
        assert_eq!(legacy.row_stream_rows, 0);
        assert!(
            serde_json::to_value(&legacy)
                .unwrap()
                .get("row_stream_rows")
                .is_none()
        );
        let selected: OutputOptions = serde_json::from_str(r#"{"row_stream_rows":4096}"#).unwrap();
        assert!(selected.validate().is_ok());
        let too_large: OutputOptions = serde_json::from_str(r#"{"row_stream_rows":4097}"#).unwrap();
        assert!(too_large.validate().is_err());
        assert!(serde_json::from_str::<OutputOptions>(r#"{"row_stream_rows":-1}"#).is_err());
    }

    #[test]
    fn final_format_is_additive_and_validated() {
        let legacy: OutputOptions = serde_json::from_str("{}").unwrap();
        assert!(legacy.final_format.is_none() && legacy.validate().is_ok());
        assert!(
            serde_json::to_value(&legacy)
                .unwrap()
                .get("final_format")
                .is_none()
        );
        let wire = r#"{"compression":"DEFLATE","final_format":{"nodata":-9999,
            "full_grid":{"width":26316,"height":28916,"row_offset":1018,"col_offset":7787,
            "transform":[0.5,10,0,99.5,0,-10]},"overview_factors":[2,4],
            "overview_resampling":"average"}}"#;
        let selected: OutputOptions = serde_json::from_str(wire).unwrap();
        selected.validate().unwrap();
        let format = selected.final_format.as_ref().unwrap();
        assert_eq!((format.block_size, format.zlevel), (256, 6));
        for bad in [
            r#"{"compression":"DEFLATE"}"#,
            r#"{"final_format":{"nodata":-9999}}"#,
            r#"{"compression":"LZW","final_format":{"nodata":-9999}}"#,
            r#"{"compression":"DEFLATE","final_format":{"nodata":0.1}}"#,
            r#"{"compression":"DEFLATE","final_format":{"nodata":1e300}}"#,
            r#"{"compression":"DEFLATE","final_format":{"nodata":0,"block_size":100}}"#,
            r#"{"compression":"DEFLATE","final_format":{"nodata":0,"zlevel":0}}"#,
            r#"{"compression":"DEFLATE","final_format":{"nodata":0,"overview_factors":[4]}}"#,
            r#"{"compression":"DEFLATE","final_format":{"nodata":0,"overview_factors":[2,8]}}"#,
            r#"{"compression":"DEFLATE","final_format":{"nodata":0,"full_grid":{"width":4,
                "height":4,"row_offset":4,"col_offset":0,"transform":[0,1,0,4,0,-1]}}}"#,
        ] {
            let options: OutputOptions = serde_json::from_str(bad).unwrap();
            assert!(options.validate().is_err(), "{bad}");
        }
        for unparsable in [
            r#"{"compression":"DEFLATE","final_format":{}}"#,
            r#"{"compression":"DEFLATE","final_format":{"nodata":0,"tile":256}}"#,
            r#"{"compression":"DEFLATE","final_format":{"nodata":0,"overview_resampling":"nearest"}}"#,
        ] {
            assert!(serde_json::from_str::<OutputOptions>(unparsable).is_err());
        }
    }
}
