# Changelog

## Unreleased

### Parallel native 1D render and 1D phase timers

`one-d-diagnostic` schemas /2, /3 and /4 accept an optional `threads` field
(1-64, default 1; schema /1 accepts only 1). Whole source-terrain tile rows are
rendered on one request pool; every tile keeps its own claim scratch, candidate
order and per-scanline incremental arithmetic, so Depth/WSE are bit-identical
for every worker count. Extra-worker scratch is admitted before any read. The
native and clipped 1D method records advertise `render_workers`; method IDs
and arithmetic are unchanged. Reports add `threads` and a `timing` object
(read, allocate, surface prepare, render, write, total seconds).

Measured on the Whisky Chitto p04 staged schema /2 requests (4,483 XS, 12,919 x
17,409 grid, 3 profiles, warm medians of 3, Linux i9-11900 container pinned to
4 CPUs): render 0.99 s at 1 worker, 0.49 s at 4; call total 4.58 s to 4.10 s;
outputs SHA256-equal to v0.17.0.

### terrio 0.8.0

The engine's terrio dependency moves from 0.4.2 to 0.8.0 (crates.io, still
`default-features = false` with only `geotiff`). This removes the C `zstd-sys`
build dependency and with it every C compiler requirement; terrain decode is
faster (Spring River p02, 4 CPUs, warm medians: `terrain_cache.decode_seconds`
3.94 to 3.65). terrio 0.8.0 runs internal parallel work on a process-wide pool
of its own, sized from `RAYON_NUM_THREADS`, so the CLI bounds that pool to the
request's `threads` (or session `budgets.threads`, default 1) before any thread
starts, unless the caller set the variable, and terrain readers start it once on
the opening thread (concurrent first use raced to start and discard pools).
Terrain reads stay one source tile per terrio call, so decode never fans out
beyond the request pool. Spring River p02 Depth/WSE (stock and final routes)
and Whisky Chitto 1D outputs are bit-identical to 0.4.2.

### 1D terrain read with terrio; parallel inline composition

`one-d-diagnostic` schemas /2-/4 accept `terrain_tiff` (the request grid's
single-band Float32 GeoTIFF) as an alternative to a caller-staged
`terrain_file`. The engine checks size and geotransform, decodes tile-row
stripes with terrio on the request pool and maps GDAL Float32 nodata
(`ARE_REAL_EQUAL`) to NaN, exactly as a masked GDAL read; method records
advertise `terrain_tiff` and reports add `terrain_read_seconds`. Inline storage
composition runs in independent pixel chunks on the request pool with the
unchanged kernel. On Whisky Chitto p04 (full grid, 8 profiles) Commander
mapping took 22.1 s against 38.4 s, with identical products.

### Inline storage composition for 1D requests

`one-d-diagnostic` schemas /2-/3 accept an optional `storage` block (polygon
file, area count, one level file per profile and the window geotransform). The
engine rasterizes physical storage labels once, then for every profile applies
the unchanged level-pool kernel and coverage-first composition with zero 2D
cells in memory, replacing a `storage` and a `coupled-diagnostic` process and
their full-grid raw files per profile. Products and composition counts are
bit-identical to the three-call route; per-profile reports carry the storage
and composition method records and counts. Method records advertise
`inline_storage` (not with the stored grid contract). On Whisky Chitto p04
(full grid, 8 profiles, `final_format`), Commander mapping took 38.4 s against
124.9 s with separate storage and composition calls.

### Engine-written 1D and composition delivery GeoTIFFs

`one-d-diagnostic` schemas /2-/4 and coupled composition schemas /1-/2 accept
the opt-in sloping `final_format` block with `epsg`, `flooded_depth_range` and
an optional `aoi_mask_file` (one byte per window pixel). The engine applies the
mask, reports `depth_finite_pixels` and `flooded_pixels`, and writes
`depth.tif`/`wse.tif` once through `final_tiff` (tiled DEFLATE, numeric nodata,
optional full-grid placement and GDAL-average overviews) instead of raw `.bin`.
Product values are unchanged; NaN and masked pixels become nodata. Every option
and the mask are validated and the writer admitted before outputs exist.
Composition accepts `threads` (DEFLATE workers) with `final_format`. The 1D
native/clipped and non-stored-grid coupled method records advertise
`native_geotiff_outputs` and `native_final_format`. On Whisky Chitto p04 (full
12,919 x 17,409 grid, 8 profiles, storage composition) Commander mapping took
124.9 s against 166.5 s with Python-written GeoTIFFs; files were 249 MB
against 541 MB, with identical valid bits and nodata masks.

### Multi-profile 1D requests

`one-d-diagnostic` schemas /2-/4 accept `levels_files` (1-256 profiles) as an
alternative to `levels_file`; exactly one is required. Inputs, terrain and the
prepared surface are read and built once; profile `i` is rendered into reused
buffers and written to `profile-{i:04}/` with its own report in the
single-profile shape. The top-level report lists every profile and carries the
shared timing. All levels are read and admitted before any output is created.
Outputs are bit-identical to one request per profile; method records advertise
`multi_profile`. Single-profile requests and their output layout are unchanged.
On Whisky Chitto p04 (3 profiles, 4 workers, warm medians of 3), one request
took 5.88 s against 12.50 s for three single requests.

## 0.17.0 - 2026-11-10

### Final-format native delivery

Add opt-in `final_format` with `compression: "DEFLATE"` for native sloping
Depth/WSE GeoTIFFs: full-grid placement at an integer offset, a numeric nodata
value, 256-default tiles, zlib DEFLATE and cascaded average overviews matching
GDAL 3.10.3 `AVERAGE` masks and Float32 bits. Tiles are compressed on the
request pool with one tile row per level in memory; nodata-only tiles share one
encoded tile; file bytes are independent of thread count and row batching.
Existing requests, method IDs and product arithmetic are unchanged. The
capability is advertised as `native_final_format`; `depth-groups` refuses
`DEFLATE`, and sloping refuses `final_format` with the stored tile grid
contract or shear stress. Add the public `final_tiff` module and a
`final_geotiff` example.

Add a direct dependency on `flate2` with its pure-Rust `zlib-rs` backend, a
package terrio 0.4.2 already resolved; no package is added to `Cargo.lock`.
See [the evaluation and limits](docs/performance-improvements.md#final-format-single-pass-writer).

Pre-port evidence (unqualified timing): Spring River p02 Max Depth/WSE,
sloping, 4 CPUs, 1 in-flight, Commander 0.117.0 plus its `final_format` staging
change, Windows MinGW build of the private pre-port writer source, not this
0.17.0 build. Five old/new pairs: 0 mask and
0 bit differences at base, overview 2 and overview 4 for both products. Wall
time about 61 s against 134 s (about 27 s against 131 s excluding the
comparison step) on an 8-vCPU VM at about 20% background load, above the 20%
quiet gate, so these timings are not qualified benchmarks. A refusal of EPSG
codes the writer cannot encode now happens from the request, before any read.

## 0.16.0 - 2026-11-09

Advertise slopingPretty velocity, per-time D x V, D x V^2 and Froude and their
`reduce_max/1` Max as qualified with every slopingPretty option: sloping
method flags `velocity_with_faces` (`rasmapper-with-faces-velocity-derived/1`)
and `velocity_with_faces_options` (`reduce_shallow_to_horizontal`,
`use_depth_weighted_faces`). No numerical change: velocity is clipped by the
depth rendered with the same options, as in RAS Mapper. Native HEC-RAS 7.0.1
Max on bridges p05 and Davis p02, all four option combinations: 0 mask and
0 bit differences. The earlier bridges slopingPretty mask difference came from
comparing a plain run with a shallow-reduction reference.

### Max shear stress (`rasmapper-face-shear-max/1`)

Add RAS Mapper's Shear Stress (Max) for physical 2D cells to `sloping`
requests through an optional `shear_stress` block, advertised as
`shear_stress` with `shear_stress_depth: ["rendered", "horizontal"]`. The
engine computes every face's shear at every stored output time
(least-squares facepoint velocities, `Faces Area Elevation` hydraulic radius,
first Manning's n, `k*|V|*V*n^2/R^(1/3)`), keeps each face's Max, averages it to
facepoints and paints it with the plain sloping weights, clipped by the
rendered or horizontal Max depth. Native HEC-RAS 7.0.1: BaldEagleCrkMulti2D
bridges p05 and Davis p02, sloping and horizontal, 0 mask and 0 binary32
differences, and per-face Max equal to RAS Mapper's PostProcessing
`Maximum Shear`. Max only; storage areas, stored output times and slopingPretty
options are refused. See [docs/shear-stress.md](docs/shear-stress.md).

Rust source compatibility: `sloping_session::Request` struct literals must
include `shear_stress` (use `None`). Serialized requests are unchanged.

## 0.15.0 - 2026-11-09

Add the explicit native-serial inundation-boundary contract with canonical
cross-section, storage-area and mixed geometry, original terrain mosaics,
complete tile/stitch history and bounded workers. Preserve native flat merging,
host-specific crossing order, integer rounding and original-edge joins.
Keep the deterministic boundary default and its request semantics unchanged.

Associate unchanged flat rings for GIS delivery without a second union.
Refuse unsupported or ambiguous geometry and failed intersections. Retain
complete BSD, Boost and MIT source provenance for the private clipping core.
Sanitize build-local wheel SBOM identities while preserving the dependency
graph, executable bytes and license payloads.

Rust struct-literal callers must supply the added boundary request fields.
Native serial qualification remains
an explicit opt-in with fixture, runtime, profile and platform limits. See the
[native contour contract](README.md#native-serial-contour-contract) and the
Commander mapping guide for valid-vector and raw-diagnostic comparison scopes.

## 0.14.0 - 2026-11-08

Add an explicit native stored-terrain grid contract for plain sloping
Depth/WSE and mixed XS/2D composition. Bind raw source identity, logical tile
sampling and derived delivery framing separately, with strict capability,
layout and component-report validation. Commander supplies canonical prepared
1D and mixed Depth/WSE workflows with explicit diagnostic opt-in; its mixed
API seals the stored-tile-grid choice. Ordinary 2D entry points retain their
dimensional guards and source-grid defaults.

Retain complete native mask/Float32 comparison results for the named 6.6 and
7.0.1 fixture scopes. The 7.0.1 mandatory matrix has 32 of 32 exact
products; fresh Baxter checks also pass against 6.6; 6.6 Lawson/White Oak coverage is
limited to stripe pilots. Diagnostic defaults and numerical qualification flags
remain unchanged. Davis original-source raw native framing is not admitted by
the `/1` center-equivalence contract; velocity/temporal grid extensions and
terrain export with modifications remain outside this scope. See the
[runtime comparison scope](README.md#native-runtime-comparison-scope).

Rust source compatibility: struct literals for `sloping::RasterWindow`,
`sloping_session::Request`, `one_d_session::Request`,
`coupled_session::Request` and `coupled_session::Component` must include their
new optional grid fields. The coupled request also includes `window`.
Use `None` for source-grid requests; `RasterWindow::new()` and
`with_stored_grid()` provide explicit construction paths. These public source additions require this deliberate
minor release. Existing serialized request
defaults and method semantics remain intact.

## 0.13.0  - 2026-11-07

### Bounded native delivery and performance qualification

Align terrain preparation to original source block rows and reuse classified
source validity in terrain-scope checks while preserving footprint counts and
refusals. Borrow donation scratch and run independent velocity/derived pixel
passes in the existing request pool without changing per-pixel arithmetic.
Add separate scope, storage-ground, finalize, velocity, derived and delivery
timers.

Add bounded native TIFF strips and opt-in `row_stream_rows` for plain corner
sloping Depth/WSE. Preserve original global coordinates and legacy defaults;
refuse unsupported storage/velocity/reduction combinations. Full ownership and
terrain remain resident. Streaming trades lower output memory for throughput.

Record matched RASMapper reruns with six qualified configurations and three
warm pairs each. Creek's native raw exports varied on repetition, so its
exactness gate stopped the run and no Creek median is qualified. See
[the rerun evidence and limits](docs/performance-comparison-rerun.md).

### Derived-map operation selection and measured profile concurrency

Select known derived-map operations once per frame and directly collect their
Float32 buffers, preserving the existing formulas, masks and unknown-product
helper behavior. Private CLB09 Windows tests retain complete native masks and
bits. A small paired wall-time reduction was observed; CPU benefit was
inconclusive. Independently measured profile concurrency improves the tested
Davis reduction without changing the global thread budget. Defaults remain
unchanged; see [performance qualification](docs/performance-qualification.md)
for the fixture, artifact identity, settings and measurement limits.

### Experimental stored-surface 1D diagnostic

Add a separate `one-d-xs-surface-diagnostic/3` Rust kernel and
`one-d-diagnostic` CLI. Preserve caller-supplied global XS TIN ordering and
segment-local indices; bind reusable geometry to ordered cross-section WSE
profiles. Admit allocations before binary payload reads, reject invalid
geometry, and emit aligned Float32 Depth/WSE buffers with common masks.
Synthetic tests cover local indices, edge ordering, missing data, binary32
visibility, budgets and CLI transport. Native parity is explicitly unqualified;
existing mapping methods and their numerical behavior are unchanged.

Early native comparisons suggested last-stored-segment ownership at XS
overlaps, including dry suppression. Preserve that legacy diagnostic; broader
junction-aware evidence motivates the explicit native-layout method below. Remaining
private XS fixture A rounding, storage and terrain-layout gaps keep this method experimental.
Add `coupled-diagnostic` for explicitly aligned single-profile 1D/2D component
buffers. Physical 2D coverage wins before visibility, preserving selected
Float32 bits and preventing wet-only fallback. Reject mismatched grids, CRS,
profiles, masks and owner labels; cap diagnostic JSON and admit output/writer
scratch before binary reads. Neither diagnostic is a public Commander mixed
mapping API or a general native qualification claim.

Add explicit coupled schema `/2` and method
`one-d-two-d-storage-coverage-first-diagnostic/1` for prepared storage products.
Storage coverage overrides XS even when dry; copy selected Float32 bits.
Reject physical storage/2D overlap, mismatched storage metadata and invalid
products before output mutation. Admit the additional storage arrays before
reads. Preserve schema `/1` and its method semantics.

### Explicit XS clipping and qualified private XS fixture A inputs

Add 1D schema `/3` and method `one-d-xs-surface-diagnostic/5` with required
normalized Float64 strip M and per-XS Float32 barrier records. Preserve original
claim order and tile scan history while applying levees, default strip limits,
and native XS-end obstruction tests. Keep schemas `/1` and `/2` unchanged.
Allocation admission includes M planes, staged records, decoded barriers and
retained barrier copies. Reject incomplete/nonfinite inputs and unsupported
clipping scopes before creating outputs. No HDF or HEC runtime enters Rust.

Private installed-wheel private XS fixture A checks cover three steady profiles across five
registered terrain sources. Storage composition, clipping and independently
qualified terrain-read masks match complete native Depth/WSE masks and bits in
all fifteen cases. Terrain masks come from terrain-HDF metadata/stitch geometry,
never flood-reference masks; raw source masks alone were rejected. This does not
qualify arbitrary terrain collections, blocked polygons or encroachments.

### Native-layout XS diagnostic

Add explicit 1D request schema `/2` and method `one-d-xs-surface-diagnostic/4`.
Caller-prepared native render ranks select ownership before wet/dry filtering.
Scan-convert stored triangles using global double plane/edge increments restarted
at original terrain tiles, then cast unclamped weights to Float32. Preserve
schema `/1` and method `/3`. Global source windows keep original tile history;
rank payloads, geometry, tile scratch and output allocations are admitted before
reads. Add a pure staging helper for confluence/split promotion from XS endpoints
and caller-decoded River names. No HDF or native runtime dependency enters Rust.

Coupled storage schema `/2` permits zero physical 2D cells only with an entirely
unowned dry 2D component. Storage still suppresses XS before visibility. Coupled
schema `/1` retains its positive-cell requirement. Full native qualification and
known clipping limits are recorded in the private roadmap; no speedup is claimed.

### Storage-area qualification groundwork

Clarify that current storage-area mapping requires 2D inputs and whole-simulation
storage products exclude duration. Advertise those restrictions explicitly and
test duration refusal. Sloping profile reports add `storage_render_seconds`
without changing existing render timing or numerical behavior. Add an external
Float32 GeoTIFF mask/bit qualification runner and private-development roadmap
in `docs/storage-roadmap.md`; no new native parity or speedup is claimed.

### Selected native products and grouped depth maxima

Add optional native GeoTIFF product selection and explicit NONE/LZW compression,
preserving existing Rust request structs, entry points and omitted-field defaults.
Add versioned `depth-groups` requests, ordered per-sample receipts, per-reach AOI
reduction of finalized Float32 depths, shared hydraulic preparation, bounded
accumulator spill to explicit local scratch, row-based GeoTIFF delivery and a
length-delimited grouped-buffer sink. Grouped scope initially covers saved
timesteps with plain sloping and explicit feet/metres depth units. No grouped
real-model hydraulic or performance qualification is claimed. See
`docs/depth-groups.md` for capability checks, resource controls and failure rules.

## 0.12.0  - 2026-10-31

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
velocity or whole-simulation duration together with storage areas, boundary requests with storage areas and,
on a top-priority source terrain, any storage-area pixel outside the source
footprint.

Native HEC-RAS 7.0.1 stored maps, BaldEagleCrkMulti2D p07 (87,039 cells) and
p04 (15,503 cells, 2D levee structures), one storage area each: Max and
02JAN1999 13:00 Depth/WSE in horizontal, plain sloping, slopingPretty and
slopingPretty with shallow reduction, and whole-simulation arrival time and
percent inundated (0.5 ft), match in every valid-mask pixel and float32 bit,
including all storage-area pixels (p07 Max 466,912; p04 Max 461,639).

### Effective terrain without HEC-RAS

- Add `ras2fim-batch effective-terrain` (`rasmapper-effective-terrain/1`,
  advertised as `effective_terrain` by `ras2fim-batch method`): RAS Mapper's
  effective terrain of an unmodified multi-source terrain from its source
  TIFFs, level-0 tile masks and stitch TIN, without HEC-RAS. Qualified against
  HEC-RAS 7.0.1 `export_rasmapper_terrain` on BaldEagleCrkMulti2D Terrain50
  (197,179,058 valid px: identical mask, 8 stitch-TIN corner values differ) and
  five synthetic HEC-RAS terrains (four bit-identical, 5 px on the fifth).
  Terrain modifications are refused.

### Duration, velocity depth and derived products

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
