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
}

impl Compression {
    pub fn name(self) -> &'static str {
        match self {
            Self::None => "NONE",
            Self::Lzw => "LZW",
        }
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
    for name in ["products", "compression", "row_stream_rows"] {
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
            "products/compression/row_stream_rows require native geotiff_outputs".into(),
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
}
