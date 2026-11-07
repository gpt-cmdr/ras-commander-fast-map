# Private mapping roadmap

Historical private evidence: artifact identities below predate public 0.13.0.
Integrated release validation is recorded separately; these observations do not
certify the new artifact or qualify untested models/options.

Development order: storage areas, standalone 1D Depth/WSE, coupled 1D/2D.
These are mapping features for computed results, not hydraulic solver work.
Commander owns HDF staging, native comparisons and delivery supervision; this
crate remains independent of Python, GDAL and HEC-RAS.

Current priority: 1D correctness. Performance work is deferred at the user's
request while the host is busy; it is not a gate for this development step.

## Storage-area milestone

Current level-pool rendering requires accompanying 2D inputs. Native evidence
recorded in README covers BaldEagle p04/p07 under HEC-RAS 7.0.1; it is not a
storage performance qualification. Arrival/percent products support pools;
duration, velocity and boundary requests with pools remain rejected.

1. Reproduce p04/p07 full-mask and Float32-bit comparisons from external inputs.
2. Add generated end-to-end fixtures for multiple pools, terrain nodata,
   wet/dry transitions, AOI crops, persistent reuse and installed-wheel delivery.
3. Implement genuine zero-2D storage-only requests deliberately, with allocation
   admission and empty-mesh validation tests before enabling the capability.
4. Qualify multiple connected pools and storage-to-storage connections. Keep
   overlap refusal until native ordering is established by reference evidence.
5. Measure preparation, storage rendering, output writing and end-to-end wall
   time. Optimize retained storage geometry/terrain first if measurements justify
   it; cache keys and admitted retained bytes must cover all inputs and options.

## External parity runner

Run `python scripts/qualify_storage_areas.py manifest.json new-report.json` in
an environment containing numpy and rasterio. Files and reports stay external.
Manifest paths are relative to the manifest directory. Example:

```json
{
  "schema": "fast-map-storage-qualification/1",
  "fixture": "BaldEagleCrkMulti2D p07",
  "engine_revision": "exact tested commit",
  "native_version": "HEC-RAS 7.0.1 (record exact build)",
  "options": {
    "profile": "Max", "mode": "horizontal", "products": ["depth", "wse"],
    "terrain": "record input hashes and original grid/layout",
    "threads": 1, "max_inflight_profiles": 1,
    "compression": "LZW", "scope": "complete mixed-model raster"
  },
  "products": [
    {"id": "Max-depth", "reference": "native/depth.tif", "candidate": "fast/depth.tif"},
    {"id": "Max-wse", "reference": "native/wse.tif", "candidate": "fast/wse.tif"}
  ]
}
```

The runner checks single-band Float32, CRS, transform, shape, complete validity
masks and exact valid-value bits, block by block. It records file hashes and
per-product failures and refuses to overwrite an existing report. It does not
generate native references, verify the supplied provenance, or establish speed.
List every expected product explicitly; a partial manifest proves only its listed
outputs. Compare complete mixed-model rasters as well as separately identified
storage coverage during qualification.

## Performance acceptance

Use identical hardware, terrain, profiles, AOIs, products and compression for
RASMapper and Fast-Map. Record software identities, input hashes, CPU/RAM limits,
cache conditions, failures, peak process-tree memory and every trial duration.
Alternate at least five measured runs of each engine. Report median and spread;
a win must exceed observed timing noise on each declared acceptance workload.

Separate cold end-to-end exports (including staging and writing) from warm
persistent catalogs. Process restart alone does not clear filesystem caches.
Use Commander's `RasFim.map_catalog()` recipe for polygon/time groups; start
with shared-model reuse, then vary process workers and in-flight profiles
independently under one global budget. Preserve partial receipts. No speedup
claim is accepted before full parity passes. Do not extrapolate from AOI area.

## Subsequent milestones

Standalone 1D starts with reference-driven cross-section/reach surface semantics,
then simple reaches, bends, junctions and structures. Commander stages geometry
and results; Rust renders explicit versioned capabilities. Unsupported cases
fail explicitly. Gate on native masks/bits and reproducible performance.

## Experimental 1D contract

`one-d-diagnostic` is a separate experimental method, not a replacement for
Step05 and not a production claim of RASMapper parity. The library consumes
the stored global cross-section interpolation surface. Commander owns native
surface preparation, HDF extraction, coordinate alignment and terrain staging.
The Rust module adds no dependency on those preparation tools.

The CLI request uses schema `fast-map-one-d-diagnostic/1`, explicitly requires
`scope="xs-surface-only"`, and rejects unknown fields. Required fields:

| Fields | Prepared payload |
| --- | --- |
| `points_file`, `point_count` | Little-endian Float64 rows `[x,y,upstream_weight,lateral_coordinate]` |
| `triangles_file`, `triangle_count` | Little-endian Int32 rows of three segment-local point indices |
| `tin_info_file`, `segment_count` | Little-endian Int32 rows `[point_start,point_count,triangle_start,triangle_count]` |
| `xs_ids_file` | One little-endian Int32 `[upstream,downstream]` row per segment |
| `cross_section_count`, `levels_file` | One little-endian Float32 WSE per XS, in the stored XS index order; NaN means missing |
| `terrain_file` | Row-major aligned Float32 terrain; NaN means nodata |
| `width`, `height`, `geo_transform` | Positive grid dimensions and six-value north-up, same-CRS pixel-corner transform |
| `max_working_bytes` | Admission budget for geometry, construction scratch and raster buffers |

Paths resolve relative to the request file. Geometry slices must be contiguous,
cover all stored records, and retain their original order. Degenerate triangles,
negative or out-of-range indices, nonfinite geometry and invalid weights fail
explicitly. The diagnostic refuses missing endpoint levels for the whole
segment. Method `/3` uses the last stored segment in overlaps, then the lowest
triangle index within that segment. Coverage is resolved before wet/dry
filtering. The inclusive barycentric edge rule remains experimental.

The CLI creates a new output directory with little-endian Float32 `depth.bin`,
`wse.bin` and `report.json`. Both products have a common NaN mask. Reports
include the method and `native_parity_qualified=false`. The command runs
serially; library geometry is immutable and can be shared in an existing
bounded caller pool. The admission estimate is not a process RSS limit.

Next correctness gates: exact native interpolation/visibility arithmetic;
full-mask and valid-value-bit comparison on a standalone reach; bends and
cross-section transitions; structures and junctions with explicit ownership;
profile/AOI consistency; Commander staging and public API integration. A
diagnostic crop from a mixed model does not qualify full 1D or coupled mapping.

Initial external evidence (HEC-RAS 7.0.1, Muncie p04, registered `Terrain`,
plain sloping, 02JAN1900 06:00:00 and 12:00:00): method
`one-d-xs-surface-diagnostic/2` matches complete masks and all valid Float32
bits for both Depth and WSE in a 91-by-164-pixel XS crop. The stored surface
contains 60 segments, 2,635 points and 3,778 triangles; all are retained during
the cropped runs. This evidence establishes only that crop and those profiles.
The model also contains 2D geometry, so this is not standalone-model or coupled
mapping qualification. Performance was not assessed.

The observed weighted sum must remain `f32(w*us + f32(1-w)*ds)` with separate
Float32 operations; rewriting it as `ds + w*(us-ds)` changes output bits.
The native export uses the stored XY on its output lattice in this fixture.
A generic CRS conversion prompted by slightly different terrain WKT parameter
rounding breaks those bits. Preserve the native prepared coordinate contract;
do not generalize this fixture observation into permission to ignore CRS
differences for other models.

External regression: set `FAST_MAP_ONE_D_FIXTURE` to a staged directory with
`request.json`, its payloads, and native `expected-depth.bin` /
`expected-wse.bin`, then run `cargo test --test one_d
external_native_crop_masks_and_bits -- --ignored`. Native fixtures remain
opt-in and outside version control.

## Expanded 1D evidence and open failures

Bridge Hydraulics p03 and JunctionHydraulics p01 each pass two complete-domain
Depth/WSE comparisons against HEC-RAS 7.0.1 plain sloping exports. These are
real model geometry/results on explicitly controlled terrain because the
examples have schematic XY and no source CRS. Bridge profiles are
10FEB1990 12:00 and 11FEB1990 00:00; junction profiles are 03SEP1998 18:00 and
04SEP1998 12:00. Native terrain creation quantizes elevations and can replace
NaN cells with -9999 while retaining a NaN nodata tag. Preparation preserves
the native valid elevations and the independently known original terrain mask;
it never borrows a flood-reference mask.

private XS fixture A p01 adds three steady profiles (three registered steady profiles), five real terrain
tiles, junctions, structures and two storage areas. Complete tile comparisons
expose important failures. Method `/3` fixes later-segment XS precedence;
common-valid WSE bit differences across the fifteen tile/profile cases fall
from 11,624 to 23. Complete WSE mask differences remain 31,049 and only one
tile/profile passes both criteria. These sums count repeated profile/tile
pixels, not unique geographic pixels. Missing storage pools, dry pool overrides,
source-tile ownership, additional unexplained mask differences and rare
Float32 rounding differences remain. A whole-model native export is deliberately
compared against the XS-only diagnostic so unsupported scope stays visible.
Do not interpret the controlled-terrain passes as general 1D qualification,
and do not add coordinate nudges or numerical tolerances to hide the failures.

## 3DEP diagnostics and expanded external fixtures

Private external bundles dated 2026-11-04 retain USGS 3DEP downloads, request
metadata, hashes and native HEC-RAS 7.0.1 plain sloping Depth/WSE exports.
The Muncie p04 bundle covers its complete downloaded 307-by-195 grid at the
06:00 and 12:00 profiles above. Coverage-first composition matches complete
masks and valid Float32 bits for both products at both profiles. This is
mapping evidence on reused hydraulic results, not a new hydraulic simulation.

private XS fixture A p01 uses a 1,964-by-939 grid and three registered steady profiles profiles. An external
probe interpolates levee lateral limits from station/elevation geometry and
saved endpoint WSE, then applies independently prepared storage polygon
ownership, including dry suppression. This reproduces complete Depth/WSE masks
and bits for Big and steady profile 2. steady profile 3 still has 60 extra wet pixels and two
common-valid bit differences in each product. Sampled residual masks lie in
stored XS segment 59; their cause remains unresolved. The probe does not use
reference-derived masks and is not implemented in the Rust engine. The engine
remains XS-only; these probe results must not be reported as engine passes.

The external `geometry-matrix-20261104` bundle adds Bald Eagle p15 (89 cross
sections, one 14,206-cell 2D area, storage, two lateral structures, a bridge
and an inline dam) and p18 (two connected 2D areas with 3,359 and 1,066 cells).
Each has two independent native Depth/WSE pairs on the same 1,819-by-1,250
3DEP grid, requested at 20 m output spacing. This is a resampled output
resolution, not a claim of native source resolution. Native profiles are
03JAN1999 00:00 and 04JAN1999 12:00 for p15, and 02JAN1999 00:00 and 12:00
for p18. `validate.py` verifies source and staged hydraulic payload hashes,
aligned CRS/grid, consistent product masks and exact recovered ground values.
`artifact-sha256.json` covers the exports and independent little-endian
`expected-depth.bin` / `expected-wse.bin` reference buffers.

Engine comparisons on 2026-11-05 use the installed diagnostic wheel (executable
SHA256 `6c96f1ff9a7b7d268b0d78d46a869b69eaf039feca907325729642e87f9d5df6`),
plain sloping, one thread, one in-flight profile and a 512 MiB admission budget.
Both p18 profiles match complete native Depth/WSE masks and valid bits with
all 4,425 physical cells and 5,198 centers including ghosts retained. The p15
XS/2D composition matches the later profile exactly; the earlier profile has
63 extra wet pixels and no common-valid bit differences. All 63 lie within
the storage polygon outside physical 2D coverage. An external geometry-derived
storage override using the saved 631 ft pool level suppresses them and matches
both products at both profiles. This initial run was probe evidence; the
explicit Rust storage extension below is qualified separately. All 14,206 physical cells and 15,399 centers are
retained in p15; no public mixed-model API guard was removed.

Separate 400-by-400 2D-component crops reproduce the corresponding full-grid
Depth/WSE masks and bits at both profiles for each plan. These checks retain
global mesh geometry; they do not qualify arbitrary crops or XS crop behavior.
The external bundle records requests, engine identity, comparison counts and
crop windows in `engine-comparisons.json` and `crop-comparisons.json`.

Further private XS fixture A steady-profile investigation locates right-edge blocked
obstructions beside residual segment 59. A global obstruction-clipping probe
was rejected because it removes native-wet pixels, including at a junction.
The two remaining steady profile 3 WSE bit differences are one ULP at segments 28 and
171. Neither obstruction semantics nor arithmetic has been changed in Rust.

The retained p18 plan rejected a terrain association update. Its disposable
project therefore binds the original terrain filename to a Commander-created
3DEP terrain copy, with the replaced file backed up and binding hashes recorded.
Both profiles pass the independent ground checks; source models are unchanged.
The bundles convert vertical metres using standard HEC Feet (0.3048 m).
Hydraulic vertical datums remain unverified; no vertical datum correction was
invented. No performance measurements or public release are implied.

## Coupled diagnostic contract

`ras2fim-batch coupled-diagnostic <request.json> <new-output-dir>` consumes
schema `fast-map-coupled-diagnostic/1`. It composes prepared component products;
it does not stage models or replace Commander's guarded public APIs.
Both diagnostic CLI JSON documents are limited to 65,536 bytes before parsing.

The request has `xs` and `two_d` objects, each containing `profile_id`, `crs`,
`width`, `height`, `geo_transform`, `depth_file` and `wse_file`. Profiles, CRS
identities and grids must agree exactly. Profile IDs are at most 256 bytes and
CRS identities at most 16,384 bytes. `two_d_owners_file` contains one
little-endian UInt32 physical-cell label per pixel: zero is unowned and
positive labels are row index plus one, bounded by `two_d_cell_count`.
Ownership must come from the same global polygon burn as the 2D component.
All product files are aligned little-endian Float32 buffers with a common
Depth/WSE NaN mask. `max_working_bytes` admits the input/output arrays, writer
scratch and bounded report metadata before payload reads.

Method `one-d-two-d-coverage-first-diagnostic/1` copies both products from the
2D component wherever there is physical 2D coverage, including dry pixels;
otherwise it copies the XS component. It performs no interpolation, maximum
or wet-only fallback. Invalid masks, negative depths, infinities, out-of-range
owners and valid 2D products outside physical ownership are rejected before
output mutation. New output directories contain `depth.bin`, `wse.bin` and
`report.json`; reports retain `native_parity_qualified=false`.

Explicit schema `fast-map-coupled-diagnostic/2` requires a `storage` object with
`component`, `owners_file` and `area_count`. Its `component` has the same fields
and exact profile/CRS/grid requirements as `xs` and `two_d`. The owners file is
one little-endian UInt32 per pixel, zero unowned and positive source row plus
one, bounded by `area_count`. Ownership must be derived from global storage
polygons using the same burn and source ordering as the storage products.
Schema `/1` refuses storage; schema `/2` refuses its omission. Arrays require
40 bytes per pixel instead of 28, plus the same admitted writer/report allowance.

Method `one-d-two-d-storage-coverage-first-diagnostic/1` selects storage products
wherever storage owns a pixel, including dry suppression of wet XS. All products
are copied bit-for-bit. Physical storage and 2D coverage must be disjoint:
overlap is rejected even when dry, because its precedence is not qualified.
Storage products outside storage coverage, invalid masks/values and out-of-range
owners are rejected before output mutation. Reports add `storage_owned_pixels`
and `dry_storage_suppressed_wet_xs` counters. The Rust compositor does not infer
storage polygons, interpolate pool levels or implement levee/structure clipping.

Installed-wheel qualification on 2026-11-05 (engine SHA256
`72492eb848e0d0d7b7960c82c9ddfdd568fda779c4602c8a0f07ba030ec9d0f8`):
schema `/2` matches complete native Depth/WSE masks and valid bits for both
Bald Eagle p15 profiles on the full 1,819-by-1,250 3DEP grid above. Prepared
storage products and 1,837 physical storage labels come from the Rust horizontal
kernel and its polygon burn; these labels have no overlap with physical 2D
coverage. The earlier profile suppresses 63 wet XS pixels; the later suppresses
none. Both pool products are entirely dry, so this evidence qualifies dry
suppression in this fixture, not native wet-pool or overlap semantics. Wet
product bit preservation is covered separately by synthetic tests.

The same wheel preserves exact results for the previous p15 schema `/1` later
profile, both Muncie 3DEP profiles and both Bald Eagle p18 profiles. Requests,
counts, engine hash and independent reference comparisons are retained in the
external `storage-extension-20261105/native-validation.json`. Full Rust tests,
the opt-in coupled regression, formatting, Clippy (with the existing
`chunks_exact_to_as_chunks` lint exception) and installed-wheel packaging smoke
passed. General native parity remains unqualified and performance is deferred.

Initial coupled evidence: Muncie p04, HEC-RAS 7.0.1, registered `Terrain`, plain
sloping, 02JAN1900 06:00 and 12:00, a 200-by-200 interface crop. All global
5,391 physical polygons, 5,765 centers including ghosts and 60 XS surfaces
are retained. Coverage-first composition matches complete Depth/WSE masks
and bits at both profiles. At 06:00 a wet-only 2D fallback creates 51 extra
wet pixels; maximum depth also gets 11 common-valid values wrong. This proves
dry suppression for this interface, not general coupled-model qualification.
The fixture records the known source-WKT rounding discrepancy; it does not
claim that public CRS validation passed or authorize arbitrary CRS relabeling.

For the opt-in CLI regression set `FAST_MAP_COUPLED_FIXTURE` to a directory
containing the request and independent `expected-depth.bin` / `expected-wse.bin`,
then run `cargo test --test coupled external_coupled_masks_and_bits -- --ignored`.

Coupled 1D/2D then establishes interface ownership, native precedence, aligned
profiles and global dependencies across river, mesh and storage surfaces.
Independent maps cannot be assumed composable by maximum depth. Gate on full
model/AOI consistency and mixed-model native comparisons.

Private development uses a separate private repository retaining provenance and
licenses, with public publishing disabled. Reviewed qualification is required
before any release. Run fmt, tests, clippy, source-manifest verification and
installed-wheel smoke for implementation milestones.

## Native-layout XS diagnostic

Explicit `fast-map-one-d-diagnostic/2` selects method
`one-d-xs-surface-diagnostic/4`; schema `/1` retains the earlier `/3` contract.
Both remain experimental, with `native_parity_qualified=false`. Geometry, source
ordering and XS profiles retain the existing binary contracts. Schema `/2` adds:

| Field | Meaning |
| --- | --- |
| `render_order_file` | Required little-endian UInt32 rank per stored segment; exact permutation of `0..segment_count`. |
| `tile_size` | Required original terrain TIFF square tile size, 1 to 4096. Part of numerical identity and cache keys. |
| `window` | Optional `{col,row,width,height}` in the original source grid. Terrain buffer contains only that window, row-major. |

`width`, `height`, and `geo_transform` always describe the original full source
terrain. The source grid must be square and north-up. Output metadata describes
the requested window and records the original source grid and offsets. Reusing a
crop-local grid changes arithmetic and is not equivalent. Unknown fields,
incomplete native options, malformed ranks and insufficient admission budgets
fail before output creation. Schema `/1` rejects the native options.

Caller staging computes render ranks by visiting XS IDs in ascending order. At
confluences promote the first stored same-River continuation; at splits promote
the symmetric continuation. A feature that is both confluence and split promotes
neither. Append unpromoted surfaces in stored order, removing duplicates. River
names are decoded and storage padding removed by staging, compared exactly and
case-sensitively; Reach names do not affect promotion. The pure Rust staging
helper `one_d_order::render_ranks` accepts those decoded names and endpoint IDs.
The rasterizer consumes only the resulting numeric permutation.

The first ranked surface and first stored triangle claim each covered source-tile
pixel before terrain validity, missing profile values or visibility. Interpolation
uses a global double plane and native per-tile scanline increments, inclusive
pixel-center spans, and an unclamped Float32 weight. The existing separate
Float32 weighted WSE sum, plot tolerance and depth subtraction remain unchanged.
Tile candidates use an R-tree over immutable geometry; cropped renders evaluate
the same global row and column history as full renders. Geometry/indexing, tile
claim scratch and buffers are included in checked allocation admission.

Coupled storage schema `/2` accepts `two_d_cell_count=0` for XS-plus-storage
mapping. Its aligned 2D products must be entirely NaN and its ownership entirely
zero. Schema `/1` continues to require positive physical 2D counts. Storage/2D
overlap remains refused. Storage must use stored `Storage Areas/Water Surface`
values, not an approximately equal variable from another result table.

Explicit levee/end-obstruction clipping is available through schema `/3` below.
Native degenerate-triangle ties and broader terrain-layout qualification remain
separate work. Performance testing follows
full-raster correctness; diagnostic tests or sampled matches do not establish
general native parity.

### Larger-corpus engine checks

On 2026-11-05, the private method `/4` executable with explicit native ranks,
256-pixel source terrain tiles, and Rust storage composition matches complete
native HEC-RAS 7.0.1 plain-sloping Depth/WSE masks and every valid Float32 bit:
private mixed fixture B p04 at 09/10/11MAR2016 21:00 on the full 12,919-by-17,409 grid,
and private mixed fixture C p67 at 02JAN2000 04:30/10:30/16:30 on the full 2,445-by-4,543
grid. Both use the same RASMapper-exported terrain with rasterized modifications
at 4x LiDAR spacing (approximately 4 metres); no hydraulic solver was rerun.
private mixed fixture B includes the stored registered storage area level pool; private mixed fixture C retains the previously
qualified Rust 2D components and physical ownership. These fixture results do
not qualify arbitrary models or native terrain tile layouts.

The seven retained Muncie/bridge/junction 1D fixtures also match full masks/bits.
Two Bald Eagle p15 storage compositions, two Muncie 3DEP compositions, and two
Bald Eagle p18 2D profiles retain exact complete native masks/bits. Across 15
private XS fixture A XS-only tile/profile cases, no previously matching pixels regress; known
storage/clipping masks remain. The 13 residual valid-value differences are
inside an unstaged pool whose native WSE equals the stored pool level. private XS fixture A
requires storage composition and levee/end-obstruction clipping before claiming
full parity. External evidence retains executable hashes, request fingerprints,
complete comparison counts and the inputs needed for reproduction. No timing
or speedup claim accompanies these correctness checks.

## Explicit XS clipping

`fast-map-one-d-diagnostic/3` selects experimental method
`one-d-xs-surface-diagnostic/5`. It retains all schema `/2` fields and the same
source-global tile/window contract. Required additional fields are:

| Field | Binary contract |
| --- | --- |
| `normalized_m_file` | One little-endian Float64 native normalized-strip M per global TIN point. Raw stored M is not a substitute. |
| `xs_clip_file` | Eight little-endian Float32 values per XS: left levee M/elevation, right levee M/elevation, left-end M/elevation, right-end M/elevation. |
| `clipping_scope` | Must be `levees-and-xs-end-obstructions-only`. Blocked-obstruction polygons and steady encroachments are excluded. |

Each barrier is either a finite M/elevation pair (M in [-2,2]) or two NaNs.
A partial pair is invalid. Left-end records contain the M of their right edge;
right-end records contain the M of their left edge. Caller staging selects the
native first touching obstruction at each XS end and converts stations using
native Float32 bank/overbank arithmetic. The engine does not inspect geometry
files or infer whether excluded features exist; staging must reject unsupported
feature inputs before requesting this scope.

The native double M plane follows the same stored triangle vertex order, global
tile geometry, row history and column increments as the WSE weight plane, then
casts pixel M to Float32. Levees activate only with records at both XS and both
elevations strictly above their corresponding XS levels. Native default strip
bounds are always interpolated from -2/+2 in separate Float32 operations. End
obstructions clip when both elevations exceed the endpoint levels or when equal
elevations exceed the interpolated WSE. Comparisons at M boundaries are strict.
A clipped pixel retains its first geometry claim and cannot expose a lower
priority wet surface. Terrain validity and plot-tolerance checks remain separate.

Schema `/1` and `/2` reject clipping options. Constructor and transport admission
include normalized-M inputs, per-triangle M planes, raw/decoded XS records and
retained clip copies. Incomplete options, malformed records, invalid dimensions
or insufficient budgets fail before output creation.

### private XS fixture A qualification update, 2026-11-06

HEC-RAS 7.0.1 plain-sloping native references for private XS fixture A p01 three registered steady profiles
on five registered terrain source grids now match complete Depth/WSE masks and
all valid Float32 bits with the private installed Windows wheel. These fifteen
checks use schema `/3`, native stored pool levels and Rust storage composition.
The XS geometry retains all 172 surfaces and 173 cross sections; no solver rerun
or geometry pruning is involved. Prepared native strip M changes 1,034 of 8,193
stored point M values; the fixture has 21 right levees and two qualifying right-end
obstructions, with no supported-path blocked polygons or steady encroachments.

The remaining old source-grid discrepancies were terrain validity, not another
XS formula. Source-HDF bit0 alone and subtracting earlier finite TIFF coverage
both remove genuinely native-wet seam pixels and were rejected. Qualified
staging instead reconstructs the native tile-local bit0 expansion and level-0
stitch-TIN bounding-box restoration from terrain HDF metadata. No reference flood
mask, mismatch location or fitted elevation enters that preparation. Retained
finite source elevations remain bit-identical; source grids and 256-pixel tile
layout remain unchanged. This is fixture-specific qualification of source
history, not a general native stitching/elevation implementation. Other terrain
collections require independent qualification or an explicit shared native
terrain export. Earlier unqualified per-source private XS fixture A results are retained as
historical controls.

The method remains experimental. Broader obstruction/encroachment support,
general terrain layouts, Linux wheel qualification and public Commander
preparation integration are separate work; performance claims remain deferred.

A separate shared-terrain control also passes all three private XS fixture A steady profiles
with the installed Windows wheel: the current registered terrain was exported
through Commander's RASMapper API with rasterized modifications at the verified
20-foot source spacing, then loaded as PreparedTerrain. The single full grid is
3,196 by 1,514 pixels (4,838,744 pixels), EPSG:2227 US survey feet, with 256-pixel
tiles. Geometry and result-HDF associations were bound in an isolated working
copy; retained hydraulic/TIN payload hashes are unchanged. Native plain-sloping
Depth/WSE and Rust `/5` plus storage match every complete mask and valid bit.
The full export and PreparedTerrain elevations/masks also match exactly; native
Depth equals WSE minus that shared terrain bit-for-bit. Three real clipped-XS
320-by-320 AOIs crossing global tile boundaries retain their full-grid masks and
bits. These are crop/full checks rather than independent native cropped exports.
