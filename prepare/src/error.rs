//! Typed refusals, one per ras-commander exception class they replace.

use std::fmt;

/// The class of a refusal. Each maps to the exception the Python preparation
/// (ras-commander 0.124.0 with fim-commander's grouped worker) raises for the
/// same condition, so callers keep their reason-code mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `FimUnsupportedError`: valid input this method does not support.
    Unsupported,
    /// `ValueError` / `TypeError`: malformed request or inconsistent model data.
    Invalid,
    /// `FimAoiError`: the area of interest cannot be used.
    Aoi,
    /// `MeshCellPolygonError`: invalid cells under the `raise` policy.
    MeshCellPolygon,
    /// `MemoryError`: a planned allocation exceeds its admitted budget.
    Budget,
    /// `FimEngineError`: a source changed, or the engine failed or disagreed.
    Engine,
    /// Reading a model, terrain or file failed.
    Io,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Unsupported => "unsupported",
            Kind::Invalid => "invalid",
            Kind::Aoi => "aoi",
            Kind::MeshCellPolygon => "mesh_cell_polygon",
            Kind::Budget => "budget",
            Kind::Engine => "engine",
            Kind::Io => "io",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub kind: Kind,
    pub message: String,
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn new(kind: Kind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new(Kind::Unsupported, message)
    }
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(Kind::Invalid, message)
    }
    pub fn aoi(message: impl Into<String>) -> Self {
        Self::new(Kind::Aoi, message)
    }
    pub fn budget(message: impl Into<String>) -> Self {
        Self::new(Kind::Budget, message)
    }
    pub fn engine(message: impl Into<String>) -> Self {
        Self::new(Kind::Engine, message)
    }
    pub fn io(message: impl Into<String>) -> Self {
        Self::new(Kind::Io, message)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind.as_str(), self.message)
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::io(e.to_string())
    }
}

impl From<ras_commander_fast_hdf::Error> for Error {
    fn from(e: ras_commander_fast_hdf::Error) -> Self {
        use ras_commander_fast_hdf::ErrorKind as K;
        let kind = match e.kind {
            K::Unsupported => Kind::Unsupported,
            K::InvalidRequest | K::StructuralTopology | K::InvalidGeometry | K::Corrupt => {
                Kind::Invalid
            }
            _ => Kind::Io,
        };
        Error::new(kind, format!("fast-hdf: {e}"))
    }
}

impl From<geos::Error> for Error {
    fn from(e: geos::Error) -> Self {
        Error::invalid(format!("GEOS: {e}"))
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::invalid(format!("JSON: {e}"))
    }
}

impl From<ras_commander_fast_map::MapError> for Error {
    fn from(e: ras_commander_fast_map::MapError) -> Self {
        Error::io(format!("terrain: {e}"))
    }
}
