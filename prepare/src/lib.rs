//! Native preparation of HEC-RAS 2D plain-sloping depth-group requests for
//! Fast-Map's persistent session, value-identical to ras-commander 0.124.0's
//! Python preparation as fim-commander's grouped mapper runs it
//! (`fim_commander.mapping._depth_groups._cohort`).
//!
//! Inputs: a plan HDF (full or slim), the mapping terrain, AOI polygons and
//! ordered saved-time groups. Output: `ras-commander-depth-groups-request/1`
//! and every file it references, for an unchanged `ras2fim-batch` engine.
//! HDF reads use ras-commander-fast-hdf 0.5.0; polygon operations use GEOS
//! 3.13.1, the release shapely 2.1.2 carries.

pub mod aoi;
pub mod bound;
pub mod cells;
pub mod cohort;
pub mod crs;
pub mod error;
pub mod geom;
pub mod grid;
pub mod hdf;
pub mod payload;
pub mod request;
pub mod scope;
pub mod worker;

pub use error::{Error, Kind, Result};
