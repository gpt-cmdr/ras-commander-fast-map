# Changelog

## Unreleased

Render standalone storage-area water surfaces as level pools
(`storage-area-level-pool/1`) in the `sloping` and `horizontal` requests.
Optional `storage_areas_file` (framed little-endian WKB polygons),
`storage_area_count` and per-profile `storage_area_values_file` (one float32
WSE per area) are all-or-nothing; requests without them are unchanged. Pixel
ownership uses the 2D terrio pixel-centre burn on the same window; visibility is
the native float32 `wse >= terrain + 0.001` test. Storage-area pixels also owned
by a 2D cell, pixels inside two storage areas and nonfinite storage-area WSE are
refused. Boundary requests refuse storage areas. On BaldEagleCrkMulti2D p07, Max
and one output time match native HEC-RAS 7.0.1 sloping stored maps in every
valid-mask pixel and float32 bit, including all storage-area pixels.

## 0.10.0

Add the separate `boundary` CLI and Rust boundary API with method ID
`deterministic-zero-depth-contours/1`. Generate deterministic zero-depth polygons
for horizontal and plain corner-sloping physical 2D surfaces, using one original
unmodified terrain TIFF plus terrain-HDF mask, perimeter and stitch metadata.
Keep negative samples, full hydraulic topology and final bbox/polygon clipping.
Coordinates use a 0.001 projected-CRS-unit lattice; reports explicitly disclose
native vector differences. Existing raster methods and arithmetic are unchanged.

Share geometry, terrain, ownership and stencils across bounded profiles and tile
workers in one Rayon pool. Add planned contour-memory admission, atomic projected
GeoJSON products, per-profile statistics and explicit aborted-batch cleanup.
Reject face/shallow/depth-weighted options and unsafe or case-colliding profile
IDs. Include the unmodified `clipper2-rust` 1.2.0 dependency and Boost Software
License 1.0 notice for integer polygon union and clipping.

## 0.9.0

Add explicitly owned persistent sloping sessions through `sloping-server` and
the Rust `PreparedSession` API. Retain one geometry/stencil set, terrain block
cache, Rayon pool and a bounded last-window cache across serialized requests.
Plain sloping weights have a bounded 32 MiB prefix cache; cell-face rendering
keeps the original donor-history path. The retained window/key/weight payload
is bounded to 64 MiB and included in planned admission. Existing one-shot
commands, method IDs, profile concurrency and numerical operation order remain.
Report actual reuse and weight bytes. Source identity changes invalidate caches;
changed geometry/AOI bytes invalidate window reuse. Dropping a session frees it.
The immutable 0.8.0 extraction receipt remains historical; current source
integrity is enforced by this release's source manifest.

## 0.8.0

First standalone distribution as ras-commander-fast-map, extracted with history
from ras-commander rust/step05 at c7f910139af3c4df18ae421473dfbebbcf85fec5.
No numerical methods changed. Retains horizontal, plain/cell-face sloping,
depth weighting, shallow-to-horizontal, bounded timestep concurrency, terrain
windows, polygon masks and the separate Step05 method. The ras2fim-batch CLI,
method IDs and request formats remain compatible. Adds package/build identity,
crates.io metadata, wheel/release workflows and extraction source fingerprints.
