# RAS Commander Fast-Map

<p align="center"><img src="docs/images/logo/ras-commander-fast-map_logo.svg" alt="RAS Commander Fast-Map: Native Rust Mapping Engine" width="450"></p>

Native Rust flood inundation mapping, distributed as the
`ras-commander-fast-map` Rust crate and Python wheel. It maps computed hydraulic
results; it does not run the HEC-RAS hydraulic solver.

## Install

```bash
pip install ras-commander-fast-map
ras2fim-batch method
```

Python wheels provide the compatible `ras2fim-batch` executable on Windows x64
and Linux x64 (glibc 2.28+). A wheel requires no Rust compiler or system GDAL.
Python consumers normally install `ras-commander[fim]`, which adds the public
`RasFim` APIs and Python preparation/GeoTIFF delivery dependencies. Remove the
former `ras2fim-engine` package before installing this package if both would
own the same executable. The command name and JSON contracts remain compatible.

Rust users can install the CLI or use the library independently of Python:

```bash
cargo install ras-commander-fast-map --locked
cargo add ras-commander-fast-map
```

```rust
let methods = ras_commander_fast_map::engine_method_record();
assert!(methods["sloping"].is_object());
```

`engine_method_record()` returns the same engine and method registry as
`ras2fim-batch method`, including each method's capabilities and qualification
limits. The existing `method_record()` describes only the original batch method,
available in the registry as `batch_session_r1`.
The additive `capabilities` list separates the stable versioned API contract
from each method's numerical qualification and intended use. Diagnostic 1D and
coupled transports remain unqualified; a stable transport does not imply
production model preparation or native parity. See the
[method discovery contract](docs/method-contract.md).

See [Rust API documentation](https://docs.rs/ras-commander-fast-map) and the
[RasFim guide](https://rascommander.info/ras/api/fim/) for request contracts and
end-to-end model preparation. The Rust library consumes prepared geometry,
profiles and terrain; HDF extraction belongs to `ras-commander-fast-hdf` and
Commander. The CLI does not read a plan HDF directly.

## Methods and resource controls

`ras2fim-batch one-d-diagnostic <request.json> <new-output-dir>` and the Rust
`one_d` module render caller-prepared stored cross-section TINs and aligned
terrain buffers to Depth/WSE, with allocation admission and reusable geometry.
Commander supplies canonical preparation and mapping through
`RasFim.prepare_one_d_model()` and `RasFim.map_one_d_profiles()`, including
supported stored steady/unsteady profiles and disjoint storage pools.

`coupled-diagnostic` composes aligned XS and 2D buffers using physical 2D
coverage before wet/dry filtering. Commander provides
`RasFim.prepare_coupled_model()` and `RasFim.map_coupled_profiles()` for the
supported unsteady plain-corner Depth/WSE scope. Both canonical workflows
require explicit `allow_unqualified=True`, retain `qualified=False`, and
refuse unsupported geometry and terrain. The ordinary 2D entry points retain
their dimensional guards. Stable transport contracts and named native
comparisons do not imply general numerical qualification. See the
[mapping guide](https://rascommander.info/ras/native-mapping-engine/#prepare-and-map-stored-1d-surfaces)
and [remaining scope](docs/storage-roadmap.md).
1D request schema `fast-map-one-d-diagnostic/2` selects experimental method
`one-d-xs-surface-diagnostic/4`: caller-staged native render ranks, first-claim
ownership and terrain-tile scanline interpolation. The original 1D schema `/1`
retains method `/3` unchanged. Record the original full terrain grid and tile
size; optional windows retain global tile history. See the
[native-layout contract](docs/storage-roadmap.md#native-layout-xs-diagnostic).

Explicit 1D schema `fast-map-one-d-diagnostic/3` selects method `/5` for
caller-prepared normalized-strip M, levees and XS-end obstructions. It also
applies native default strip bounds. Claims remain fixed when clipping removes
a pixel. Blocked-obstruction polygons and steady encroachments are excluded.
Schemas `/1` and `/2` retain their previous behavior; see the
[clipping contract](docs/storage-roadmap.md#explicit-xs-clipping).

Explicit coupled schema `/2` additionally composes prepared storage products, including
dry storage suppression of XS. Physical storage/2D overlap is rejected until
qualified; schema `/1` behavior remains unchanged.

The explicit `rasmapper-stored-tile-grid/1` contract retains the terrain TIFF's
source identity while deriving logical sampling tiles and output framing from
its rounded full extent. Sloping request `ras-commander-sloping-request/2`, XS
request `fast-map-one-d-diagnostic/4`, and coupled request
`fast-map-coupled-diagnostic/3` carry the same complete grid descriptor. Inspect
their separate capability records before use. The admitted scope is one
original 256-tiled north-up square source, plain corner Depth/WSE, and logical
centers exactly representable on the derived full output lattice. Unsupported
layouts, terrain scope, storage, velocity and face modes refuse. Commander
exposes this choice on mixed preparation/direct mapping; prepared mixed models
inherit the sealed choice. Standalone 1D/2D mapping and catalogs do not expose
the keyword. These explicit transports retain numerically unqualified discovery
and diagnostic opt-in even when the named fixture comparisons pass.

### Thread budget

A request's `threads` (or `budgets.threads`) bounds the threads that are busy at
once. terrio runs its own parallel work on one process-wide pool, sized when it
starts by `RAYON_NUM_THREADS`; `ras2fim-batch` sets that variable to 1 before any
thread starts, replacing an inherited value. Engine calls that can reach that
pool run off request workers, which wait without taking other work, and terrio's
decoded-block cache is off (the engine budgets its own block cache). OS threads
can exceed `threads` (idle pool threads, helper threads of blocked workers), busy
threads cannot. Rust callers that do not run the executable must set
`RAYON_NUM_THREADS=1` before terrio is first used; `ras2fim-batch method`
reports `engine.thread_budget`.

### Native runtime comparison scope

Within the stated coverage, the native-runtime checks below compare complete
valid/nodata masks and every valid Float32 bit, with exact CRS and integer-window
framing. They use the
frozen Windows engine artifact, original terrain layouts and complete global
hydraulic geometry. The criterion is numerical equality to the named native
renderer, not calibration or general model qualification.
Runtime versions identify the loaded Mapper, independently of the stored
results' producer. These checks do not rerun the hydraulic solver or establish
solver-version equivalence.

| Fixture and options | Native runtime | Products and coverage | Result |
|---|---|---|---|
| Mixed XS/2D fixture; plain corners, original single source, explicit stored-tile-grid `/1`; saved 57/69/81 and Max | 6.6 and 7.0.1 | Depth/WSE, eight complete products per runtime | Exact |
| Bald Eagle; original two-source terrain and priority-mask history; Max, saved 72 and all 145 saved times | 6.6 and 7.0.1 | Twelve complete spatial/velocity/derived-Max products per runtime | Exact; temporal products are outside this result |
| Baxter; Big/Bigger/Biggest steady profiles and prepared storage/XS composition; GDAL one thread, 128 MiB cache, native CPU one | 6.6 and 7.0.1 | Six complete Depth/WSE products per runtime, 4,838,744 pixels each | Exact; fresh exports and actual loaded renderer hashes verified |
| Lawson and White Oak; selected canonical 1D profiles, native-normalized M and obstruction records | 6.6 | Four stripe-pilot Depth/WSE products | Exact only within the recorded stripes |
| Lawson and White Oak p19/p15; selected canonical 1D profiles | 7.0.1 | Six complete Depth/WSE products, contiguous stripes plus zero-valid exterior scans | Exact |

The 7.0.1 mandatory matrix has **32 of 32 products exact**. These cases do not enable diagnostic defaults or
qualify other profiles, terrain configurations, geometry variants or platforms.
The 6.6 stripe pilots are not full-raster canonical 1D qualification.

Davis original-source raw 7.0.1 stored-map framing remains blocked: its logical
tile centers differ from its derived output-affine centers, so `/1` correctly
refuses the layout. The fifteen fresh native references have no passing
candidate pixel comparisons. Terrain export with modifications, arbitrary
mosaics/stitches and unsupported geometry remain outside this release scope.
See the [mapping qualification table](https://rascommander.info/ras/native-mapping-engine/#native-runtime-comparison-scope)
for the API scope and comparison limits.

Rust callers can construct `sloping::RasterWindow` with `RasterWindow::new()`;
it selects ordinary source-affine sampling. `with_stored_grid()` explicitly
selects logical tile sampling and checks source identity. The public struct's
`native_grid` field is `None` for ordinary source-affine struct literals.

<a id="polygon-aoi-edge-rule-0101"></a>

### Polygon AOI edge rule

Horizontal/sloping method records advertise `aoi_mask="global-terrain-lattice/1"`.
AOI coverage is computed in source terrain pixel units, at global pixel centers,
with y increasing northward. Even-odd scanlines include lower-y/exclude upper-y
endpoints; filled spans include the left endpoint and exclude the right.
For a rectangle this includes its minimum x/y edges and excludes its maximum
x/y edges. Holes and disjoint polygon parts retain the same rule. No epsilon
is added, and ring reversal does not change coverage. A cropped mask equals
the corresponding crop of a full-grid mask, including pixel-center edge ties.

The rule deliberately replaces origin-dependent legacy AOI masking. It need
not reproduce legacy GDAL edge ties. Cell ownership and hydraulic interpolation
are unchanged. AOI rows use the request's bounded Rayon pool; planned geometry
and per-worker crossing scratch are admitted before payload reads. The pure
Rust `aoi::mask` function accepts a framed Polygon/MultiPolygon WKB path, full
source geotransform and integer source window.

- Horizontal: cell-constant WSE with native float32 visibility.
- Sloping: reconstructed cell corners; optional cell faces, depth weighting,
  and shallow-to-horizontal selection.
- ras2fim-2D Step05: the separate globally prepared SciPy triangulation method.

Terrain decoding and ownership rasterization use terrio with GDAL disabled.
Rayon workers share immutable terrain/geometry across bounded in-flight profiles.
Bbox/polygon output masks retain global hydraulic topology. Planned allocation
admission is not a hard RSS limit. Python delivery may use Rasterio/GDAL;
the Rust mapping executable does not link it.

### Multi-polygon/timestep catalogs

Use RAS Commander's `RasFim.map_catalog()` to map multiple watershed/time
groups from computed 2D models. Give each job one selected polygon or bbox,
the complete source HDF and qualified mapping TIFF, and an ordered list of
saved timestep indices or `"Max"`. The Python supervisor stages native HDF
inputs and schedules this Rust engine; the Rust crate does not read model HDF
directly. See the [agent-ready catalog recipe](https://rascommander.info/ras/api/fim/#build-jobs-from-a-watershed-layer)
and [throughput policy](https://rascommander.info/ras/api/fim/#choose-a-throughput-policy).

Start with one reuse worker for overlapping time lists on one model/terrain.
Bound preparation waves with `max_profiles_per_batch`. Test independent
workers for different models or low-sharing workloads within one global
CPU/RAM allowance. Native in-flight timesteps share the same Rayon pool;
additional processes can duplicate geometry, staged data and caches.

Retained hydraulic/multiple-window caches and optional
native terrio LZW output support repeated mapping. Commander retains Rasterio
as its default writer;
select `product_writer="native"` for supported sloping catalogs and inspect
`RasFim.get_engine_info()` capability flags. Native cache ceilings count inside
working-memory admission. The supervisor's per-worker prepared/headroom
reservations and sampled memory guard also apply. Inspect total catalog wall
time, cache reports and peak tree memory before increasing concurrency/caches.

AOIs constrain raster reads and output pixels, while hydraulic dependencies
remain global. A watershed occupying 1% of the terrain does not imply a 100x
speedup. Keep the qualified source TIFF layout; do not crop hydraulic arrays
or retile cell-face terrain per reach. Inspect both overall and per-row
statuses, preserve partial receipts and use a fresh output folder for retries.

<a id="retained-reach-state-011"></a>

### Retained reach state

Native sloping GeoTIFF requests also accept optional `products` (unique selections
of `"depth"` and `"wse"`) and `compression` (`"NONE"` or `"LZW"`). Omission retains
both products and LZW. These controls require `stream_outputs=true` and
`geotiff_outputs=true`; inspect `native_output_products` and
`native_output_compression` capabilities before sending them to older engines.

The additive `depth-groups` command reduces finalized, individually masked saved
timestep depths into one native-grid Float32 maximum per reach/group, with an
inclusive cutoff, bounded resident accumulators, explicit local spill and ordered
sample receipts. It also supports length-delimited binary delivery. See the
[grouped depth contract](docs/depth-groups.md). The native command consumes staged
inputs, not HDF; downstream applications own preparation and orchestration.
Real-model hydraulic and performance qualification of grouped output is pending.

The persistent sloping server additionally advertises `hydraulic_cache`,
`multi_window_cache` and `native_geotiff_outputs`. Requests can set
`hydraulic_cache_bytes` (default 0) and `window_cache_bytes` (default 64 MiB).
Their ceilings are included in planned working-memory admission, independently
of the terrain block cache. Zero disables retention. Both caches use LRU
eviction and include retained input keys and allocation metadata in their plans.

Hydraulic entries bind complete staged WSE file identities and rendering
options to reconstructed global cell/corner/face state. Geometry changes clear
them. Window entries bind terrain/geometry identity, the exact integer source
window, cell addresses, polygon WKB and AOI WKB. They retain ownership, terrain
and up to 32 MiB of plain-sloping weights per window when the budget permits.
Cell-face rendering retains its original donor-history path. Reports include
cache hit flags, retained byte plans and entry counts. No spatial clipping of
hydraulic dependencies is introduced.

With `stream_outputs=true, geotiff_outputs=true`, sloping requests write
lossless LZW `depth.tif` and `wse.tif` beneath each unit folder and stream JSON
completion records instead of arrays. Records include pixel counts and native
write timing. terrio owns encoding; no GDAL/Python dependency is added to the
engine. This route requires an EPSG code or verbatim `crs_geokeys` (see
[Any CRS](#any-crs-verbatim-geokeys)) and excludes velocity.
Commander validates output metadata before delivery. Ordinary array streams
and binary-file output remain compatible. Inspect advertised capability flags
before selecting a delivery path.

<a id="fast-map-012"></a>

### Additional mapped products and terrain

Mapping supports storage-area level pools (`storage_area_surfaces`
`storage-area-level-pool/1` on the `sloping`, `horizontal` and
`whole_simulation` methods), `ras2fim-batch effective-terrain` for unmodified
multi-source terrains (`effective_terrain` `rasmapper-effective-terrain/1`),
the qualified duration rule (`duration_rule`
`rasmapper-backward-window-recession/1`), velocity clipping by the rendered or
horizontal depth (`velocity_depth`), and D x V, D x V^2 and Froude with their
Max over output times (`derived_products`, `derived_max` `reduce_max/1`).
Existing method IDs and request schemas are unchanged; inspect the advertised
flags before using these fields.

Opt-in `row_stream_rows` (1 through 4096, default 0) bounds the
Depth/WSE output buffers while writing native GeoTIFFs. It requires both
stream/output flags above and plain corner sloping, without velocity, storage,
Max reduction or a grouped consumer. Ownership and ground remain full-window
buffers; source coordinates and tile history are unchanged. Unsupported
combinations are refused before payload reads. Completion records retain the
existing `native-geotiff/1` framing. Inspect `native_row_streaming` and the
[performance implementation evaluation](docs/performance-improvements.md)
before enabling this option. Commander callers must explicitly stage the wire
option; it is not enabled by the existing `product_writer="native"` default.

#### Final delivery format (`final_format`)

Private opt-in `final_format`, selected together with `"compression": "DEFLATE"`,
makes the engine write each Depth/WSE product once in its delivery form instead
of an LZW window that a later pass re-encodes. It is additive: requests without
it are unchanged, and `DEFLATE` without it (or it without `DEFLATE`) is refused.
Inspect the `native_final_format` capability before sending it.

```json
"stream_outputs": true, "geotiff_outputs": true,
"compression": "DEFLATE",
"final_format": {
  "nodata": -9999.0,
  "block_size": 256,
  "zlevel": 6,
  "full_grid": {"width": 26316, "height": 28916,
                "row_offset": 1018, "col_offset": 7787,
                "transform": [x0, dx, 0.0, y0, 0.0, dy]},
  "overview_factors": [2, 4],
  "overview_resampling": "average"
}
```

| Field | Type | Contract |
|---|---|---|
| `nodata` | number, required | Written for every invalid (NaN) pixel; finite and exactly Float32 |
| `block_size` | integer, default 256 | Square tile edge; a multiple of 16 from 16 through 4096; overviews use the same tiles |
| `zlevel` | integer, default 6 | zlib level 1 through 9; predictor 1 |
| `full_grid` | object, optional | Delivery grid `width`/`height`, the integer `row_offset`/`col_offset` of the request `window` on it, and the grid's GDAL geotransform. Omitted: the product window is the grid |
| `overview_factors` | integer array, default `[]` | `[2]`, `[2, 4]`, ...: consecutive powers of two |
| `overview_resampling` | string, default `"average"` | Only `"average"` |

The file is a single-band Float32 GeoTIFF (BigTIFF when the uncompressed levels
could pass 4 GB) with the EPSG code (or the request's verbatim `crs_geokeys`), the grid transform, `GDAL_NODATA` and, with
overviews, the `rio_overview` `resampling=average` item. Window pixels keep the
product's Float32 bits; only the invalid marker, placement, tiling, compression
and overviews differ from the LZW route. `full_grid.transform` must equal the
request window's transform at the given offsets (same pixel size bits, offsets
within 1e-8 pixel); it is checked from the request alone, with the writer's
planned buffers admitted before payload reads.

Overviews reproduce GDAL `AVERAGE` overviews of a nodata raster as built by
`build_overviews([2, 4], average)`: each level is resampled from the level
before it, a pixel is the binary64 weighted mean of the valid source pixels it
covers (fractional edge weights where a level's size is odd), rounded once to
Float32, and a valid mean equal to the nodata value moves one Float32 step.
Like GDAL's nodata mask, values within about four units in the last place of
the nodata value read as nodata; a valid product value that would read as
nodata, or is infinite, is refused instead of losing a valid pixel.

Tiles are compressed on the request's Rayon pool, one row of tiles per level in
memory, and written in tile order, so file bytes do not depend on `threads` or
`row_stream_rows`. Tiles holding only nodata share one encoded tile; rows above
and below the window are never materialized. Completion records keep the
`native-geotiff/1` framing and add `final_format` and, on the full-array route,
`depth_final`/`wse_final` counts (`valid_pixels`, `tiles`,
`shared_nodata_tiles`, `file_bytes`). Rust callers can use `final_tiff::Layout`
and `final_tiff::Writer` directly. See the
[evaluation and limits](docs/performance-improvements.md#final-format-single-pass-writer).

Binary32 operation order and method identifiers are preserved. `MIGRATION.json`
retains the immutable 0.8.0 extraction receipt; `SOURCE_MANIFEST.json` binds this
version's current sources. `ras2fim-batch method` reports package/build identity
and capabilities.

<a id="persistent-reach-sessions-09"></a>

## Persistent reach sessions

`sloping-server` accepts serialized JSON-line commands with absolute `request`
and `output` paths. Each request uses the existing sloping schema and streams
the same profile headers/binary arrays, followed by a single
`{"command_status":"complete"}` line. The process retains terrain blocks,
mesh/stencils and one Rayon pool. Send `{"close":true}` or close stdin to release
the session. Failed commands terminate the process; callers must reject partial
products. Commander owns process lifetime and timeouts through prepared models.

At most the last small window's owner/terrain arrays, exact geometry/AOI key
and plain-sloping weights are retained within a separate 64 MiB payload bound.
Weights use at most 32 MiB; pixels beyond the bounded prefix keep their original
evaluation path. Cell-face/donor-history weights are not cached. Large windows
still share terrain blocks and geometry, without retaining their output arrays.
Plans include the retained payload budget and the original 64 MiB decoded tile
cache; this is allocation admission, not a hard process RSS limit. Native
`PreparedSession` objects provide the same reuse without the CLI.

<a id="deterministic-inundation-boundaries-010"></a>

## Deterministic inundation boundaries

Generate projected-CRS inundation polygons from signed depths with the separate
`deterministic-zero-depth-contours/1` method:

```bash
ras2fim-batch boundary boundary-request.json new-boundary-output
```

Commander prepares the model and terrain metadata for this command. Rust callers
can use `boundary_session::Request` and `boundary_session::run` directly. The
request schema is `ras-commander-boundary-request/1`:

| Field | Contract |
|---|---|
| `schema` | `ras-commander-boundary-request/1` |
| `interpolation` | `horizontal` or `sloping` |
| `model` | Existing `ras-commander-sloping-request/1` model, global physical-cell polygons, full hydraulic meshes, profile files, CRS, AOI and resource controls |
| `terrain_metadata` | Original terrain shape/transform, 256 × 256 tile grid, selected global tile IDs, mask/perimeter numeric descriptors, and optional paired stitch-point/triangle descriptors |

Boundary generation supports physical 2D cells with horizontal or plain corner
sloping surfaces. Cell-face interpolation, shallow-to-horizontal controls and
face depth weighting are rejected. Supply one original, unmodified float32
terrain TIFF and its matching terrain HDF metadata. Retiled/cropped TIFFs,
terrain modifications and multi-source mosaics are outside the schema `/1` scope.
For schema `/1`, Commander rejects models containing 1D elements and boundary
requests refuse storage-area surfaces. See the native serial contract below
for schema `/2`.

The method retains valid negative depths to locate the zero-depth crossings.
The intrinsic cell-wet gate uses float32 `WSE > minimum_elevation + 0.001`;
there is no added pixel-depth cutoff or 0.5-foot postprocessing. Masks/perimeters
and stitch triangles come from the terrain HDF; the TIFF alone is insufficient.
Bbox/polygon selection limits terrain tiles, while hydraulic topology stays
global. AOI clipping follows contour construction, preserving edge samples.

Quantized integer clipping produces coordinates at 0.001 **projected CRS units**.
Tile/profile results are combined in stable order, independent of worker order.
Each profile produces one GeoJSON FeatureCollection containing a MultiPolygon;
its explicit legacy GeoJSON `crs` member identifies projected coordinates. These
coordinates are not reprojected to RFC 7946 longitude/latitude. The report uses
`ras-commander-boundary-report/1` and includes profile status, polygon/hole/vertex
counts, area in squared CRS units, timings and effective concurrency. Empty
boundaries are explicit. Output requires a new or empty directory; a failed
batch removes delivered profile polygons and records an aborted report.

Profiles share geometry, terrain blocks, ownership and stencils. `model.threads`
and `model.max_inflight_profiles` bound work in one Rayon pool; tile workers
also share that pool. Planned admission covers prepared data and per-profile
contour allowances before payload reads. It is not a hard process RSS limit.

These deterministic polygons are **not an exact reproduction of RASMapper
vectors**. Stable all-subject integer union and quantized intersections can
produce different holes and boundary vertices. Repeated native Max exports can
also vary with unchanged inputs. Use the native stored-map API when its vector
output is required, and retain comparison evidence for the intended model.
This separate polygon layer does not change the existing Depth/WSE raster
methods, their float32 arithmetic or their qualification claims.

### Native serial contour contract

The `native_boundary` capability selects
`rasmapper-serial-zero-depth-contours/1` through boundary request schema `/2`.
Set `contour_method` to that exact method ID. This separate contract retains
native first-subject/subsequent-clip NonZero union, classic integer intersection
rounding, serial source/tile/stitch order and native thin-hole filtering.
It does not redirect schema `/1` to a different algorithm.

The model can additionally carry stored XS signed-depth sampling and level-pool
storage. The optional `one_d` descriptor uses `boundary_xs::XsInput`; physical
2D coverage takes precedence over stored storage coverage, then XS coverage,
before missing-water or dry filtering. XS raster samples and arbitrary stitch
points use distinct native arithmetic. Selected profiles must agree across
all components.

Keep every original terrain source in native order. The primary descriptor
retains the complete global stitch TIN; `additional_terrains` is an ordered
list of `{terrain, crs_identity, terrain_metadata}` records without duplicate
stitch descriptors. Each source retains its original 256-pixel tile layout,
all row-major tile IDs, masks and perimeters. Sources are never flattened or
retiled. All source caches and active tile buffers enter planned admission.

Masks and perimeters are read by active tile row; their full backing files
count toward prepared disk storage separately from working RAM. Empty tile
units retain their original positions in the merge sequence. The complete
global stitch TIN remains available. Ambiguous physical 2D claims at fuzzy
polygon edges are refused because native parallel insertion does not establish
a reproducible owner there.

Native comparison requires RASMapper's explicit serial polygon-loop control;
one-CPU affinity alone does not establish that control. Commander supplies it
through `StoreMapPerformanceOptions(native_polygon_serial=True)`, verifies the
effective flag and records the actual runtime identity. Ordinary native parallel
exports can produce different merge orders. Read the capability's qualification
status and the named comparison evidence before claiming vector equality.
Retain both rendering-helper and merge-host identities: native equal-height
intersection ordering also depends on the .NET host's sorting behavior.

Commander exposes this method through `method="native_serial"` on its existing
boundary mapping/comparison APIs. It contours the complete source history and
applies an optional final unrounded GEOS polygon intersection in GIS delivery,
recorded as `unrounded-geos-intersection/1`. Direct Rust AOI clipping remains an
integer-lattice operation and has a distinct clipping contract. Terrain
modifications, face options, steady encroachment and blocked-obstruction
polygons remain outside this method's admitted scope.

## Velocity (sloping requests)

Add per-mesh `velocity_normals_files` (HDF `Faces NormalUnitVector and Length`
as float32 x, y, length, with **length replaced by the face polyline length**)
and per-profile `face_velocity_files` (float32 face-normal velocities; for Max,
the summary `Maximum Face Velocity` with Max WSE as the profile values) to a
`sloping` request. Velocity magnitude is written as `velocity.bin`, or as a
third streamed array when the stream header has `"velocity": true`. Its values
do not depend on the WSE render mode; RAS Mapper clips them with the active
mode's depth, so velocity is drawn where the clipping depth > 0 (method
`rasmapper-sloping-velocity/0`). `velocity_depth` selects that depth:
`rendered` (default, this request's `sloping` or `slopingPretty` depth) or
`horizontal` (RAS Mapper's horizontal render: the owning cell's WSE over
terrain, skipping cells with `wse <= minimum + 0.001`, float32).

`derived_products` (`depth_x_velocity`, `depth_x_velocity_squared`, `froude`;
method `rasmapper-depth-velocity-products/1`) adds per-profile arrays from the
clipping depth `d` and velocity `v`, in RAS Mapper's operation order: `d * v`,
`(d * v) * v`, and `float32(v / sqrt(float32(g * d)))` with the request's
float32 `gravity` (32.174 or 9.80665). They are written as `<product>.bin` or
streamed after velocity in the listed order (header `"derived"`).
`reduce_max: true` keeps the pixelwise maximum of each derived product over
every profile in the request and writes only `max/<product>.bin`: RAS Mapper's
Max D x V, D x V^2 and Froude are per-output-time products reduced this way
(the valid area is the union over times), not Max depth times Max velocity.
The sloping method record advertises `velocity_depth`, `velocity_render_modes`,
`derived_products`, `derived_method_id` and `derived_max` (`reduce_max/1`).

**slopingPretty (cell-face stencil).** `velocity_with_faces`
(`rasmapper-with-faces-velocity-derived/1`) records that velocity, the per-time
derived products and their `reduce_max/1` Max are qualified in slopingPretty
with every option RAS Mapper offers there, listed in
`velocity_with_faces_options` (`reduce_shallow_to_horizontal`,
`use_depth_weighted_faces`). RAS Mapper's rule is the same as in plain sloping:
velocity values do not depend on the render mode, and every velocity-based
product is clipped where the depth rendered with the *same* slopingPretty
options is > 0 (derived Max is the per-time union), as RAS Mapper's stored
maps show. Shallow reduction and depth
weighting therefore change the mask, not the velocity values. Native HEC-RAS
7.0.1 stored maps (Max, all four option combinations, 0 mask and 0 bit
differences): bridges p05 velocity 974,095 / 974,142 / 974,216 / 974,256 pixels
(plain / shallow reduction / depth-weighted / both) and D x V, D x V^2, Froude
Max over 433 output times 974,110 / 974,191 / 974,231 / 974,291; Davis p02
velocity 21,402 / 21,591 / 20,703 / 21,114 and derived Max over 217 times
21,315 / 21,862 / 20,880 / 21,287. The earlier "slopingPretty depth gap" on
bridges (75 + 28 pixels) was a reference made with shallow reduction on (the
ras-commander `store_maps` default for a bare `slopingPretty`) compared with a
plain-slopingPretty run; with matching options depth, WSE and velocity are
identical.

Current private exactness work and its bounded acceptance gates are recorded
in [other-map qualification](docs/other-map-qualification.md), including the
Davis square-cell velocity correction, complete temporal tests and remaining
public-wrapper terrain metadata qualification.
The [private performance qualification](docs/performance-qualification.md)
records CLB09 measurements, profile-concurrency settings and their limits.
The [expanded corpus qualification](docs/expanded-corpus-qualification.md)
records larger-model native comparisons, storage coverage and remaining gaps.

Retained native HEC-RAS 7.0.1 comparison records collected before the
source-grid velocity correction (these scopes require a post-correction
recheck; the frozen-runtime table above reports separately completed gates):
BaldEagleCrkMulti2D bridges p05 (973,532 Max
and 402,500 output-time pixels), dam-break p07 2D cells (Max and one output
time, valid masks equal to depth) and a 2,500 x 2,500 Spring Creek eBFE AOI
(1,101,529 pixels) match bit for bit; Davis p02 Max differs in 5 of 20,560
pixels by at most 2 ulp. A true sloping native run shows the valid mask equals
the sloping depth mask on bridges Max (973,902 pixels, values and mask
identical); the 973,532-pixel figure above came from a horizontal-mode
reference. Horizontal clipping matches native horizontal velocity on bridges
Max (973,532 pixels) and 03JAN1999 00:00 (402,500); slopingPretty is
qualified separately (above). Derived products, native 7.0.1: bridges 03JAN1999 00:00
sloping (402,616 pixels) and horizontal (402,500) bit for bit for all three;
`reduce_max` over all 433 output times bridges Max sloping (973,956) and
horizontal (973,531) bit for bit; Davis p02 (horizontal Max over 217 times and
one sloping/horizontal output time) equal masks, at most 5 pixels within 2 ulp
(the Davis velocity residual). These Davis comparisons do not qualify the
original-source raw native grid: fresh 7.0.1 comparison stops at framing before
pixels, and stored-tile-grid `/1` does not admit this layout. Storage areas,
boundary requests and shear stress are outside this method. Max shear stress
is its own method, `rasmapper-face-shear-max/1`: see
[Max shear stress](docs/shear-stress.md).

## Whole-simulation products (arrival time, duration, percent inundated)

```bash
ras2fim-batch whole-simulation whole-request.json new-output-dir
```

Schema `ras-commander-whole-simulation-request/1` embeds a non-streaming
horizontal request whose profiles are the stored output times in order, plus
`times_hours` (one per profile, strictly increasing), `threshold`,
`cell_minimum_file` (float32 cell minimum elevations in cell order), optional
`products` (default `arrival_time`, `percent_inundated`) and
`allow_unqualified` (required for irregular intervals). Output times are evaluated sequentially over
the mapped 2D-owned and optional storage-area pixels, in parallel, with 32
bytes per mapped pixel. Products are written to `whole_simulation/<product>.bin`
(float32, NaN = never wet).

A pixel is wet when its horizontal depth is `> threshold` and its cell passes
RAS Mapper's thresholded gate `wse > minimum + threshold`. Optional storage-area
pixels use depth `> threshold` without a cell gate for arrival and percent
inundated; duration requests containing storage areas are rejected. Arrival is the first wet time and percent is
`100 * wet / n`. Duration runs from arrival to RAS Mapper's recession time
(`duration_rule` `rasmapper-backward-window-recession/1`): each cell's WSE
series is split into monotone windows at its local extrema (PostProcessing's
`Increasing Regions` rule, `v[k] >= v[k-1]` rising), and the windows are searched
from the last. With `L` the last wet time, the recession is `L` when it ends a
window, is one step before a falling window's end, or starts that falling
window (the native round-half-even bisection gives up there), and `L + 1`
otherwise. Duration keeps 8 bytes per cell extremum, admitted against the
budget. Times are float32 hours. Irregular output intervals are
refused unless `allow_unqualified` is set. Initially wet pixels, multi-period
pixels and threshold-equal samples are reported.

Qualification (native HEC-RAS 7.0.1, threshold 0.5 ft, storage-area-free
models): Davis pipes/conduits p02 (217 output times, 4,879 valid pixels) and
BaldEagleCrkMulti2D bridges p05 (433 times, 959,043 pixels) match in every
valid-mask pixel and every arrival and percent value bit for bit, including
initially wet, multi-period and threshold-equal samples. RAS Mapper gives the
same products in horizontal and sloping render modes on both models, so this
horizontal pass covers sloping maps too. Duration matches the same references
in every pixel (the earlier first-dry-time rule differed on 103 Davis and 11
bridges pixels).
These retained temporal comparisons do not qualify original-source raw native
Davis framing. Fresh raw-grid arrival, duration and percent-inundated references
remain uncompared because ordinary 2D has no compatible stored-tile-grid route.
The top-priority original-source Bald Eagle temporal route separately refuses
28 wet pixels dependent on lower-priority sources/stitches.
Bridges runs in 8.9 s at 383 MiB peak against about 25 s for the native call.

## Top-priority source terrain (no RAS Mapper export)

RAS Mapper writes stored maps per terrain source, on each source's own grid.
`sloping` (plain) and `horizontal` requests may map directly on the original
TIFF of the highest-priority source of a multi-source terrain, instead of a
RAS Mapper terrain export, by adding:

```json
"terrain_scope": {"kind": "top_priority_source", "source_count": 2,
  "mask": {"path": "mask.u1", "dtype": "|u1", "shape": [513, 65536]}}
```

`mask` is the source's full level-0 tile mask from the terrain HDF
(`Terrain/<source>/0/Mask`, tiles row-major); bit 0 is the source footprint.
Owned pixels outside the footprint are refused unless the result cannot
depend on their terrain: sloping pixels of cells that fail the native wet gate
are never drawn. Foreign pixels of wet cells, foreign pixels in horizontal
rendering and cell-face sloping are refused (method
`top-priority-source-footprint/1`). Boundary requests refuse it.

Qualification (plain sloping, native HEC-RAS 7.0.1, two-source Terrain50):
BaldEagleCrkMulti2D bridges p05 Max on the priority-1 `dtm_20ft` TIFF matches
in all 973,902 valid pixels and float32 bits, with 28 foreign 2D pixels all in
dry cells; the dam-break p07 2D cells also match for Max and one output time.
Lower-priority sources are not mosaicked: RAS Mapper's export resamples them
with a rule close to, but not, pixel-centre bilinear (84% of float32 values
reproduced), so a native mosaic is unqualified.

## Any CRS (verbatim GeoKeys)

`effective_terrain`, `sloping` (both schemas), `horizontal` and the 1D/coupled
`final_format` delivery accept a CRS with no EPSG code, such as ESRI:102630
(NAD83 / Alabama West, US survey feet). They advertise `crs_any: true`,
`crs_identity_kinds` and `output_crs_kinds` on their method records (the 1D
and coupled records on `native_final_format`). Other methods still require an
EPSG code.

Such a request gives `crs_geokeys` instead of `epsg` (`epsg` is then 0 or
absent; giving both is refused):

```json
"crs_geokeys": {"directory": [1, 1, 0, 18, 1024, 0, 1, 1, "..."],
  "doubles": ["30.0", "-87.5", "..."],
  "ascii": "NAD_1983_StatePlane_Alabama_West_FIPS_0102_Feet|NAD83|"},
"crs_identity": {"kind": "geokeys"}
```

The three members are the GeoKeyDirectoryTag, GeoDoubleParamsTag and
GeoAsciiParamsTag values of a GeoTIFF in that CRS (`ascii` without its NUL),
as `ras2fim-batch terrain-info` prints them (`geokeys`, `geokeys_fnv1a64`).
`doubles` are decimal strings (the shortest form that reads back as the same
binary64), which the engine parses exactly. JSON numbers are refused there:
serde_json's default number parser can be one unit in the last place off for
17-digit decimals, so they would not be verbatim. Copy the object unchanged.
Every engine-written GeoTIFF (effective terrain, LZW/NONE products and
`final_format`) stores these three tags unchanged, so GDAL, rasterio and terrio
decode the same CRS from the output as from the source. Nothing is converted
from WKT or PROJ, and no CRS is re-encoded or approximated. Products written
through terrio get the tags after terrio closes the file: their first image
directory is rewritten at the end of the file and the pixels are unchanged.
EPSG requests write the same files as before.

The terrain identity is checked before any read:

| `crs_identity.kind` | With `crs_geokeys` |
| --- | --- |
| `geokeys` | The terrain's own GeoKey tags must equal `crs_geokeys` exactly |
| `untagged` | The terrain carries no CRS at all (no GeoKey tags, none terrio reads). RAS Mapper writes such terrain sources for CRSs without an EPSG code; the caller declares the CRS (for example from the terrain HDF) |
| `caller_verified_wkt` | The WKT hash pin, as before |
| `epsg` | Refused: there is no code |

The keys are checked only as a georeferencing model. They must be a
well-formed directory with a projected, geographic or user-defined
`GTModelTypeGeoKey`, PixelIsArea (or no raster type), and values stored in the
double and ASCII tags. Reports (effective terrain, sloping, its
`physical_ownership`, horizontal, 1D/coupled delivery) give `output_crs:
{"kind": "geokeys", "fnv1a64": ...}`; depth groups refuse `crs_geokeys`. An effective-terrain request with `epsg: 0` and no keys is now
refused: older engines wrote `ProjectedCSTypeGeoKey 0`, which GDAL reads as a
local CRS in metres.

Round trip (rasterio 1.5.2, GDAL 3.12.2, pyproj 3.8.0, Windows, this engine's
effective terrain from a 40 x 40 GDAL-written source): the output CRS equals the
source's under GDAL (`==`, identical WKT) and pyproj (`equals`, `is_exact_same`,
same linear units and datum). This held for ESRI:102630, ESRI:102003,
ESRI:102008 and ESRI:54009 (Mollweide); custom Transverse Mercator (international
feet), Lambert conformal (Clarke 1866), Albers (US survey feet, towgs84) and
oblique Mercator; a geographic CRS on a custom sphere; and EPSG:2277. The
installed-wheel smoke repeats it for ESRI:102630 sloping products.

## Storage-area surfaces

Sloping requests can export physical 2D coverage with
`export_physical_ownership: true` when the method record advertises that flag.
The root `owners.bin` is little-endian UInt32: zero outside physical cells,
otherwise `owner_cells` position plus one. Coverage includes dry and terrain-nodata
pixels and comes from the same burn as the sloping products. The report records
the ownership grid and payload identity. This export refuses AOI, terrain-scope
and embedded-storage combinations before payload reads. See the
[method contract](docs/method-contract.md) for details.

`sloping`, `horizontal` and `whole-simulation` requests can render separate
storage-area polygons as level pools (method `storage-area-level-pool/1`, advertised as
`storage_area_surfaces` in `ras2fim-batch method`). These requests still require
2D cell inputs. The separate prepared-buffer `storage` transport below supports
storage-only rendering without synthetic 2D cells.
Whole-simulation support covers arrival time and percent inundated, not duration.
Supply all three together:

| Field | Contract |
|---|---|
| `storage_areas_file` | Framed little-endian WKB Polygons/MultiPolygons, one per storage area (same framing as `geometry_file`) |
| `storage_area_count` | Number of storage areas in that file |
| `profiles[].storage_area_values_file` | Little-endian float32 storage-area WSE for that profile (Max or one output time), one per area, finite |

Each storage-area pixel (pixel centre inside the polygon, on the request window,
clipped by the AOI mask) gets the area's WSE; it is visible when float32
`wse >= terrain + 0.001`, with depth `wse - terrain`, in every render mode. The
caller sizes `window` to cover the storage areas as well as the 2D cells. In
whole-simulation requests a pool pixel is wet when its depth is strictly greater
than the threshold; there is no cell gate. Pixels owned by both a 2D cell and a
storage area, or by two storage areas, are refused (RAS Mapper's ordering for
those layouts is not qualified), as are velocity with storage areas, duration
with storage areas, schema `/1` boundary requests with storage areas, and
storage-area pixels outside a top-priority source's footprint. Native schema
`/2` boundaries use the separate native serial contract above.

Qualification against native HEC-RAS 7.0.1 stored maps (RAS Mapper terrain
export, valid masks and float32 bits):

| Model | Products | Storage-area pixels | Result |
|---|---|---|---|
| BaldEagleCrkMulti2D p07 | Max Depth/WSE: horizontal, sloping, slopingPretty (with and without shallow reduction); 02JAN1999 13:00 horizontal and sloping | 466,912 (Max), 422,515 (13:00) | identical |
| BaldEagleCrkMulti2D p04 (15,503 cells, 2D levee structures) | Max Depth/WSE: horizontal, sloping, slopingPretty (both); 13:00 sloping | 461,639 (Max), 421,927 (13:00) | identical |
| p07 and p04 | Arrival time and percent inundated, 0.5 ft (865 output times) | 464,106 / 458,764 | identical |

Duration requests containing storage areas are rejected by the current engine.
Historical duration comparisons found 8 (p07) and 46 (p04) one-interval differences,
all in 2D pixels, with matching storage-area pixels; that evidence does not
establish an implemented or qualified duration capability. Both models have one
storage area connected to the 2D area; multiple storage areas and
storage-area-to-storage-area connections are not covered by this evidence.

### Storage-only prepared-buffer transport

`ras2fim-batch storage <request.json> <new-output-dir>` accepts
`schema="fast-map-storage/1"`, `storage_areas_file` (the framed WKB file above),
`storage_area_count`, `levels_file` (one little-endian Float32 WSE per area),
`terrain_file` (row-major little-endian Float32 terrain; NaN nodata), `width`,
`height`, window `geo_transform`, `threads` (1–4), and `max_working_bytes`.
All files resolve relative to the request. Counts, north-up grid, geometry
allocation and raster buffers are admitted before payload reads. Input geometry
retains global area order; callers preserve the original terrain grid and use
an exactly aligned window transform. No hydraulic or terrain extraction occurs
inside the Rust library.

Outputs are `depth.bin`, `wse.bin` (Float32, NaN nodata), `owners.bin` (UInt32,
0 outside, area index + 1 inside), and `report.json`. Owners include dry pools
and terrain-nodata pixels. This is the same `storage-area-level-pool/1` kernel,
with a new transport rather than a new pool algorithm. Discovery key `storage`
advertises the schema and explicitly reports standalone transport qualification
as pending. This does not extend the historical native comparisons above.
Overlapping pools remain refused. The labels/products can feed coupled schema
`/2`, which permits a truly empty 2D component and applies coverage-first storage
precedence over XS even for dry pools. The compositor still refuses storage/2D
overlap. No pool raster arithmetic is delegated to Python.

## Qualification and limits

The retained [five-model comparisons](https://rascommander.info/ras/development/fim-mapping-performance/)
cover raw Depth/WSE validity masks and float32 bits against native HEC-RAS 7.0.1,
without an added depth cutoff. Those published timing observations predate this
packaging extraction; no new speedup is claimed for renaming/distribution.

Mapping covers physical 2D cells. Commander rejects models with 1D river/cross-
section geometry. Storage areas are mapped only when the request supplies them. Cell-face
matching preserves the original source TIFF grid and 256-square tile layout;
arbitrary retiled/cropped terrain and multi-source mosaics are unqualified.
Depth weighting needs stored corner ground elevations. When absent, explicit
corner preparation still uses Windows/HEC-RAS 7.0.1 through Commander and
pythonnet; it has not been implemented in Rust. Once prepared, ordinary mapping
runs on Windows/Linux without HEC-RAS. Source terrain must match the effective
terrain used by the native reference.

## Build and verify

```bash
cargo fmt --check
cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings -A clippy::chunks_exact_to_as_chunks
cargo publish --locked --dry-run
python scripts/verify_sources.py
```

CI builds and smoke-tests Windows/Linux wheels. Tagged releases attach the
wheels, source distribution, provenance and checksums to GitHub Releases.
See [release instructions](docs/releasing.md) for PyPI/crates.io publishing.

## License and attribution

Our code retains its MIT license. Step05/SciPy reuse retains BSD-3-Clause notices;
see `LICENSE`, `THIRD_PARTY_NOTICES.md` and `licenses/`. The crate license
expression is `MIT AND BSD-3-Clause`. The unmodified Clipper2 dependency retains
its Boost Software License 1.0 notice. Dependency notices accompany distributions.
HEC-RAS assemblies and model data are not distributed.
This independent project is not affiliated with or endorsed by HEC/USACE.
