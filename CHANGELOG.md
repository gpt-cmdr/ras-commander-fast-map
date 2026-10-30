# Changelog

## Unreleased

### Terrain modifications on the effective terrain (`rasmapper-effective-terrain/2`)

`ras2fim-batch effective-terrain` accepts request schema
`ras-commander-effective-terrain-request/2` with `modification_layers` (one
entry per `/Modifications` layer of the terrain HDF; `modifications` must equal
their number) and applies them after the mosaic, as
`RasTerrain.export_rasmapper_terrain(rasterize_modifications=True)` does in
HEC-RAS 7.0.1. Rules were fitted pixel by pixel on exports of RAS-Mapper-authored
terrains (no vendor code): ground lines are a triangulated template (centerline
split at profile stations, crest at Top Width/2, toe at Max Reach/2 = total
width, H:V slopes, beveled convex and mitered concave joints, square ends,
Delaunay quad diagonals); constant polygons fill pixel centres inside (even-odd);
ellipses fill centres strictly inside; SetValue / SetIfHigher / SetIfLower, a
nodata pixel inside takes the modification value; layers in descending Priority
(Priority 0 last). Refused: ground-line SetValue/Add, Max Reach <= Top Width,
polygons with boundary elevations, rotated ellipses, unknown kinds. Schema 1 is
unchanged; sources may carry their own caller-verified `crs_identity` when
their WKT text differs from the first source's. Qualification (outside the
stitch TIN): HCFCD Prop_Topo 0 differences; Kingswood TerrainAlt1 3 px off by
1/32 ft at exact ties; TerrainAlt2 432 px (96 above 1/32, max 0.84 ft) on one
levee/channel pair with short segments, so it is not yet exact.

### Storage-area level pools

Render standalone storage areas as RAS Mapper level pools
(`storage-area-level-pool/1`, advertised as `storage_area_surfaces` by the
`sloping`, `horizontal` and `whole_simulation` methods). Optional
`storage_areas_file` (framed little-endian WKB polygons), `storage_area_count`
and per-profile `storage_area_values_file` (one finite float32 WSE per area) are
all-or-nothing; requests without them are unchanged. A pixel whose centre is
inside a storage-area polygon (terrio burn on the request window, clipped by the
`global-terrain-lattice/1` AOI) gets the area's WSE; it is visible when float32
`wse >= terrain + 0.001`, depth `wse - terrain`, and a horizontal request's
strict `min_depth` applies as to 2D pixels. Whole-simulation requests accumulate
pool pixels as wet when the rendered depth is strictly greater than the
threshold; a level pool has no cell gate. Refused: storage-area pixels also owned
by a 2D cell, pixels inside two storage areas, nonfinite storage-area WSE,
velocity together with storage areas, boundary requests with storage areas and,
on a top-priority source terrain, any storage-area pixel outside the source
footprint.

Native HEC-RAS 7.0.1 stored maps, BaldEagleCrkMulti2D p07 (87,039 cells) and
p04 (15,503 cells, 2D levee structures), one storage area each: Max and
02JAN1999 13:00 Depth/WSE in horizontal, plain sloping, slopingPretty and
slopingPretty with shallow reduction, and whole-simulation arrival time and
percent inundated (0.5 ft), match in every valid-mask pixel and float32 bit,
including all storage-area pixels (p07 Max 466,912; p04 Max 461,639).
- Add `ras2fim-batch effective-terrain` (`rasmapper-effective-terrain/1`,
  advertised as `effective_terrain` by `ras2fim-batch method`): RAS Mapper's
  effective terrain of an unmodified multi-source terrain from its source
  TIFFs, level-0 tile masks and stitch TIN, without HEC-RAS. Qualified against
  HEC-RAS 7.0.1 `export_rasmapper_terrain` on BaldEagleCrkMulti2D Terrain50
  (197,179,058 valid px: identical mask, 8 stitch-TIN corner values differ) and
  five synthetic HEC-RAS terrains (four bit-identical, 5 px on the fifth).
  Terrain modifications are refused.
- Whole-simulation duration uses RAS Mapper's backward window recession rule
  (`duration_rule` `rasmapper-backward-window-recession/1`) and no longer needs
  `allow_unqualified`. Native HEC-RAS 7.0.1, threshold 0.5 ft: Davis p02
  (4,879 pixels) and bridges p05 (959,043) match in every pixel. Method ID,
  arrival time and percent inundated are unchanged.
- Sloping velocity: `velocity_depth` (`rendered` or `horizontal`) selects the
  clipping depth, reproducing RAS Mapper's horizontal-mode velocity;
  slopingPretty velocity is advertised (`velocity_render_modes`).
- `derived_products` (D x V, D x V^2, Froude; method
  `rasmapper-depth-velocity-products/1`) and `reduce_max` (RAS Mapper's Max of
  those products over every output time). Froude now rounds `g * d` to
  binary32 before the square root, as RAS Mapper does (bit-identical where it
  was within 1 ulp). Qualified bit for bit on bridges p05 (sloping and
  horizontal, one output time and Max over 433 times).

## 0.11.0 — 2026-10-29

- Add byte-admitted LRU retention of reconstructed sloping timesteps across
  reach windows, keyed by complete staged values and rendering options.
- Retain multiple ownership/terrain windows under one configurable ceiling;
  use bounded plain pixel-weight prefixes when space remains.
- Add opt-in direct terrio LZW GeoTIFF delivery with completion records and
  unchanged float32 raster arithmetic.
- Document multi-polygon/timestep catalog scheduling, capability checks and
  bounded cache/concurrency settings through the RAS Commander API.

## 0.10.1 — 2026-10-28

First published release since 0.8.0. It includes every change listed under
0.10.1, 0.10.0 and 0.9.0 below; 0.9.0 and 0.10.0 were never published to
PyPI or crates.io.

### Global terrain-lattice AOI masks

Horizontal and sloping polygon AOIs use `global-terrain-lattice/1` coverage.
AOI vertices are converted once to source pixel units. Scanline crossings and
pixel centers remain in global coordinates; integer output windows only select
which pixels are emitted. Lower-y endpoints and left span endpoints are
included, upper-y and right endpoints excluded; y increases northward. There
is no geometric epsilon. This removes crop-origin rounding differences when an
edge passes through pixel centers. Holes, multipart polygons, ring reversal and
bounded Rayon workers are supported. Worker scratch is admitted before reads.

This intentionally changes AOI edge inclusion relative to legacy GDAL/terrio
masks at numerical boundary ties. Hydraulic cell ownership still uses terrio;
surface reconstruction, intrinsic visibility tolerance and float32 arithmetic
are unchanged. Method records advertise `aoi_mask`; no request schema changed.

### Velocity, whole-simulation products and top-priority terrain

Add the `velocity` method (`rasmapper-sloping-velocity/0`) to `sloping`
requests: optional per-mesh `velocity_normals_files` (HDF face normals with
polyline face lengths) and per-profile `face_velocity_files` render a velocity
magnitude product (`velocity.bin`, or a third streamed array when the header
says `"velocity": true`). It reproduces RAS Mapper's stored-map reconstruction:
C-stencil face vectors, inverse polyline-length weighted corner arcs, cell-face
donated weights with native donation history replay, drawn where depth > 0.
Native HEC-RAS 7.0.1: BaldEagleCrkMulti2D bridges p05 and dam-break p07 2D
(Max and one output time each) and a Spring Creek eBFE AOI match bit for bit;
Davis p02 Max differs in 5 of 20,560 pixels by at most 2 ulp. D x V, D x V^2
and Froude helpers are added; shear stress and Max derived products are not.
The `velocity_probe` example renders velocity for research comparisons.

Add `whole-simulation` (`native-whole-simulation-horizontal/1`): arrival time,
duration and percent time inundated from every stored output time, through the
horizontal renderer over physical 2D cells, in bounded memory.
It uses RAS Mapper's thresholded cell gate, its duration end rule and float32
times. Davis p02 and BaldEagleCrkMulti2D bridges p05 match native HEC-RAS 7.0.1
in every valid pixel, arrival and percent value (horizontal and sloping native
products are identical). Duration is opt-in and unqualified. Irregular output intervals are refused unless explicitly
allowed. The horizontal runner gains an internal profile-consumer hook;
horizontal products are unchanged.

Map on the original TIFF of a multi-source terrain's highest-priority source
without a RAS Mapper export (`terrain_scope`, method
`top-priority-source-footprint/1`). The footprint is bit 0 of the source's
level-0 terrain-HDF tile mask. Owned pixels outside it are allowed only in
sloping cells that fail the native wet gate; everything else outside it is
refused. BaldEagleCrkMulti2D bridges p05 Max (plain sloping, two sources)
matches native HEC-RAS 7.0.1 bit for bit without HEC-RAS.

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
