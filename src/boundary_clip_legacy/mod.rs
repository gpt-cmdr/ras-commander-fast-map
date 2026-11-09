//! Private classic integer clipping compatibility core.
//!
//! Base: solidmesh-clipper 0.2.0, Lars Brubaker, BSD-2-Clause; its classic
//! Clipper 6.4.2 algorithm derives from Angus Johnson, Boost Software License1.0.
//! Source-faithful compatibility changes use the official Boost-licensed
//! Clipper6.2.1 horizontal lifecycle and original-edge join predicates, plus
//! Microsoft .NET legacy quicksort and modern introsort (MIT). Tile/TIN work
//! retains legacy sorting; the global merge selects the modern host policy.
//! This private core serves only the separately qualified native-serial method.
//! It uses existing clipper2-rust integer points and has no additional runtime dependencies.
mod clipper_base;
mod engine;
mod horizontals;
mod intersections;
mod joins;
mod out_polys;
mod poly_tree;
mod statics;
mod types;
pub(crate) use engine::Clipper;
pub(crate) use types::{ClipType, IntPoint, PolyFillType, PolyType};
