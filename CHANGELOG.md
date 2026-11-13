# Changelog

## Unreleased

- **`hecras_version` takes HEC-RAS release labels.** `effective-terrain`
  accepts `6.6`, `6.7`, `6.7 Beta 4`, `6.7 Beta 5`, `7.0`, `7.0.0`, `7.0.1`
  and `7.0.2` (case and repeated blanks ignored) and maps each to a rule:
  7.0.x to the 7.0.1 rule, 6.6 to the 6.6 rule, and 6.7 to a new **6.7 rule**
  (the 6.6 rule, except that boundary elevations without a perimeter control
  point come from the stored profile, as HEC-RAS 6.7 renders them). Any other
  label, including 6.5 and earlier and anything after 7.0.2, is refused under
  either request schema before any read. `ras2fim-batch method` advertises the
  table as `effective_terrain.hecras_versions` (`{label: rule}`), and the
  report records `modifications.hecras_version` (the label) and
  `modifications.hecras_rule`. Requests that omit `hecras_version` or send
  `7.0.1` or `6.6` give bit-identical rasters (158 of the qualification's
  requests re-run: identical files).
- Qualified per release (2026-11-12): HEC-RAS 6.6, 6.7 Beta 4/5, 7.0, 7.0.1
  and 7.0.2 exports, 0 mask and 0 value differences on every layer each rule
  accepts; see `modifications.qualified`.
- **Any CRS (verbatim GeoKeys).** `effective_terrain`, `sloping` (both
  schemas), `horizontal` and 1D/coupled `final_format` delivery accept a CRS
  without an EPSG code, such as ESRI:102630 (US survey feet). The new optional
  `crs_geokeys` request field holds the GeoKey tags of a GeoTIFF in that CRS,
  and every engine-written GeoTIFF stores them unchanged. New `crs_identity`
  kinds: `geokeys` (the terrain's own tags must equal them) and `untagged`
  (RAS Mapper terrain sources that carry no CRS). These methods advertise
  `crs_any: true`, `crs_identity_kinds` and `output_crs_kinds`.
  `terrain-info` prints `geokeys` and `geokeys_fnv1a64`; `doubles` are
  exact decimal strings (JSON numbers are refused, as serde_json's default
  parser can be one unit in the last place off). Request schemas and
  method IDs are unchanged; EPSG requests write the same bytes. New library
  API: `crs` module, `NativeTerrain::open_crs`,
  `effective_terrain::write_geotiff_crs`, `final_tiff::{write_crs, check_crs,
  Writer::create_crs}`.
- **Any CRS on every method.** Depth groups (GeoTIFF keys; buffer metadata
  `crs_wkt` from the request), boundaries (GeoJSON `crs` name = request
  `crs_wkt`), the batch session (`crs_geokeys`), Step05 (terrain
  `crs_geokeys`, `source_crs` `wkt`, `nudge.dst_wkt`), whole simulation,
  storage, 1D and coupled methods advertise `crs_any`. PixelIsPoint keys are
  accepted (written PixelIsArea; CRS keys unchanged) and keys stored in the
  directory are copied with it.
- **Effective terrain `epsg: 0` without `crs_geokeys` is unchanged** (same
  bytes as 0.19.0, labelled `ProjectedCSTypeGeoKey 0`, which GDAL reads as a
  local CRS in metres); its report now carries `crs_warning`. Give
  `crs_geokeys` for a correct label.

## 0.19.0 - 2026-11-12

- **Staged cell subsets** (opt-in, capability `staged_cells`,
  `ras-commander-staged-cells/1`): sloping and `depth-groups` requests may name
  `staged_cells_files`, one ascending `u32` cell list per mesh. Profiles then
  carry only those cells' WSE, and the engine loads only the owner cells'
  stencil closure (owner vertices, their complete face rings, both cells of each
  face) instead of the complete mesh. Depth/WSE are bit-identical; subsets
  missing a closure cell are refused. The array plan's geometry term and every
  per-profile hydraulic reconstruction scale with the closure instead of the
  model. Requests without the field are unchanged.

## 0.18.0 - 2026-11-12

Release summary. Read these before upgrading:

- **Effective terrain results change for schema 1.** `effective_terrain` now
  runs and advertises `rasmapper-effective-terrain/2` for both request schemas.
  Schema 1 requests (unmodified terrains) are still accepted, but their values
  differ from earlier engines (RAS Mapper render-block sampling and its export
  TIN) and their receipts report `/2`; `/1` is no longer listed as compatible.
  Terrain modifications are supported through schema 2; unqualified forms,
  including multi-part polygons and shape-Z polygons without boundary
  elevations, are refused before any read or write so callers can use the RAS
  Mapper export instead.
- **terrio 0.8.0, and the engine keeps its thread budget.** The terrio upgrade
  removes every C compiler requirement. terrio 0.8.0 runs its internal parallel
  work on a process-wide pool of its own; the engine now confines that pool to
  one thread and calls into it from a helper thread, so a request never has more
  busy threads than it was given (checked in CI on Linux and Windows). terrio's
  own internal parallelism is therefore not used, which costs some speed on
  burn-heavy storage requests (about 2.4 s to 2.8-3.3 s in one Windows
  measurement); outputs are unchanged. Rust callers that use the library without
  `ras2fim-batch` must set `RAYON_NUM_THREADS=1` before terrio is first used;
  reports show the pool as `terrio_pool`.
- **1D mapping performance** (opt-in, capability-advertised): parallel render
  over tile rows (`render_workers`), multi-profile requests (`multi_profile`),
  engine-written `final_format` GeoTIFFs (`native_final_format`), inline storage
  composition (`inline_storage`) and a direct terrain GeoTIFF read
  (`terrain_tiff`). JSON requests from earlier releases are unchanged.
- **Rust API changes (minor version):** `one_d_session::Request.levels_file` and
  `terrain_file` became `Option<PathBuf>`, the 1D and coupled request structs
  gained fields, and `one_d_terrain::Source::planned_bytes` changed signature.
  Struct-literal callers must be updated; JSON callers are unaffected.

### Changes in detail

- Shape-Z polygons without boundary elevations (elevations from vertex Z) are
  now refused, as `method_record` and Commander's `read_layers` already state;
  the engine previously applied them although the form is not qualified.
- Multi-part polygons (an interior ring or several parts) are now refused
  before any read or write. HEC-RAS 6.6 and 7.0.1 exports do not keep holes.
  They join a feature's parts into one perimeter-TIN ring, which fills holes
  in every winding and leaves triangulation artefacts for disjoint parts or a
  leading hole ring. The engine previously left holes empty (even-odd), 70-467
  px per case away from the exports. Ellipse rotation was checked against 6.6
  and 7.0.1 exports at eight rotations: a positive rotation turns the ellipse
  clockwise, as the engine already did (0 differences). A regression test
  now pins the direction.

Terrain modifications are no longer parked: `ras2fim-batch method` advertises
`effective_terrain` as `rasmapper-effective-terrain/2` (request schemas 1 and
2), with the qualified modification forms and every refused form under
`effective_terrain.modifications`. Schema 1 requests are still accepted but
run the /2 rule, so their receipts report `/2`; their values differ from /1
engines (render-block sampling and the export TIN), and `/1` is no longer
listed as compatible. This intentionally replaces the release gate that kept the `/1`
numerical contract (`docs/releasing.md`). Whole modified real terrains match the
HEC-RAS 7.0.1 export on values except pixels in export-TIN gap fills and
isolated 1/32 ft ties; see `feature_dev_notes/effective_terrain_modifications/`.

### Effective terrain: control points, boundary elevations, Add, NaN templates, HEC-RAS 6.6

Source: an interoperability study of RasMapperLib 7.0.1 (no vendor code used
or copied), checked against HEC-RAS exports.

Pipeline:
- The mosaic and the export TIN keep binary32 values through the modifications.
- Values are rounded to 1/32 once at the end, as RAS Mapper's TIFF writer does.
- Base output is unchanged.

New forms supported:
- **Ground-line control points.** Points within the Elev Pt Tolerance set the profile at their nearest centerline station. With no stored profile, the ends take the first and last point's elevation.
- **Polygons with a control point inside, or with boundary elevations.** These use RAS Mapper's perimeter TIN, built on the export TIN's Delaunay factory. The boundary profile comes from perimeter control points, or else from the stored profile; points inside the polygon are mass points.
- **Add**, for ground lines and polygons: terrain plus the modification, in binary32 on the unrounded terrain.
- **NaN templates and negative slopes.** A NaN Max Reach draws nothing, a NaN slope is read as 3, and negative slopes need no special case.
- **`hecras_version: "6.6"`.** Ground lines with Max Reach not wider than one cell are not drawn. Named ground lines and terrain-sampled boundaries are refused under 6.6.

New optional request fields:
- Layer `control_points` (`x`, `y`, `elevation`; `null` = NaN).
- Polygon `generate_boundary`, `use_shape_z`, `profile`, `vertex_z`, `elev_pt_tolerance`, and polygon-layer `length_units`.
- Ground-line template values accept `null`.
- Request `hecras_version`.

Qualification (whole terrains vs HEC-RAS 7.0.1 exports; pixels where two exports differ excluded):
- Terrain C, all 13 layers, now unreduced: 0 mask and 4 value differences at 1/32 ft in 379.0M px.
- Terrain E, 5 of 6 layers (a 2,862-point control-point levee layer among them), 1.14G px: 0 mask, 3 values at 1/32 ft.
- Terrain D, 228 of 229 layers (202 boundary-elevation polygon layers and 20 channel layers with control points), 924.6M px: 0 mask, 3 values at 1/32 ft.
- Synthetic cases for each form: at most 2 px at 1/32 ft per case.
- HEC-RAS 6.6 exports of the synthetic cases: every accepted case matches under `hecras_version: "6.6"`.

Still refused:
- Boundary elevations sampled from the terrain. This is the one remaining layer each in D and E.
- Shape-Z polygons.
- Multi-part polygons that need a TIN.
- Named ground lines with control points or a NaN template.
- A NaN Top Width with a finite Max Reach, which makes RAS Mapper's export fail.

### Effective terrain: named ground lines (RAS Mapper's system path)

Ground lines with a `System Name`, including a lone named feature, are now
built as RasMapperLib 7.0.1 builds them instead of being refused
(interoperability study, prose findings only; no vendor code used or copied).
Same-name features with equal elevation type and Elev Pt Tolerance are joined
end to start within 60 ft in feature-ID order. Their profiles are concatenated,
and each member's first point becomes a control cross section that wins at its
station, so at a junction the next feature's start elevation replaces the
previous feature's end. Between cross sections, toe and ridge stream lines are
grown from the centerline to cross-section lines in lateral steps. A changing
template with a positive Transition Percent ends its section early. The system
is drawn once, at its highest member feature ID, with the ribbon TIN and
rasterizer of single features. New optional ground-line feature fields:
`transition_percent` and `elev_pt_tolerance` (default 0, `null` = NaN).

Qualification (HEC-RAS 7.0.1 exports, whole terrain):
- Named writer cases W3, W3n, W4 and M2: no modified pixel differs (before: 1,505 to 1,507 pixels up to 1,032 ft, and 159 pixels up to 77 ft).
- Synthetic systems (joined channels with a junction gap, Transition Percent 0, 0.5, 1 and NaN, a profile not starting at 0, systems split by elevation type, SetValue systems): 1 of 14,179 pixels differs, at 1/32 ft.
- Terrain C, on an HDF copy reduced to its 10 accepted layers, including the named systems and a SetValue channel layer: 0 mask and 4 value differences at 1/32 ft in 379.0M pixels (674.6M on the export grid). Every layer is exact except one 1/32 ft pixel.
- Terrains A and B1-B4 and the earlier synthetic cases are unchanged.

Known difference: RasMapperLib's .NET Framework `Math.Pow` is not correctly rounded. On terrain C this moves four stream-line points by up to 6e-5 ft, with no pixel effect in the measured cases.

### Effective terrain: RAS Mapper's ground-line ribbon TIN and inclusive ellipses

Ground lines (`terrain_mods`) now follow RasMapperLib 7.0.1's export arithmetic
instead of the fitted template (interoperability study, prose findings only; no
vendor code used or copied): binary32 centre profile (stations shifted to 0,
clamped to the length, 100 ft / 100/3.281 m densification with RAS Mapper's
running-start arithmetic, clamped queries); toe and ridge offset lines carrying
the centerline station as M, with intersection or arc joints, square ends,
self-intersection trimming and M-fraction injection of the profile stations;
binary32/binary64 ridge and toe elevations; a five-ribbon zipper TIN (not
Delaunay) rasterized per 256-pixel output tile with the export TIN rasterizer,
the last triangle of a feature winning. Combination is RAS Mapper's binary32
replacement (strict for SetIfHigher/SetIfLower). Ellipses are inclusive
(`<= 1`) at output-tile pixel centres and may be rotated (new optional
`rotation`, degrees). Newly supported: ground-line SetValue and Max Reach <=
Top Width. New optional `system_name` (RAS Mapper builds named ground lines
through a separate system path, measured 1,506 px apart on one channel; now
supported, see the entry above). Newly refused: negative side slopes. New
optional layer field `length_units` (`feet` default, `meters`). Whole terrains
vs HEC-RAS 7.0.1 exports with modifications (export a, pixels where two
exports differ excluded): B2 443 -> 0 value differences, B1 3 -> 0, B3, B4 and
A unchanged, and no remaining difference lies inside a modification footprint.
Synthetic 20 ft terrains with blank System Names: 0 modified pixels differ on a
22.5 km channel, a 21.2 km levee, a combined case, short-segment levees,
ground-line SetValue, Max Reach <= Top Width and five rotated ellipses; 2 of
8,935 pixels at 1/32 ft on a sharp-bend/hairpin channel case. Method IDs and
request schemas are unchanged; the new fields are optional.

### Effective terrain: RAS Mapper's per-tile export TIN (gap fill)

After sampling, each 256-pixel output tile now triangulates the edges of valid
render blocks that border off blocks and rasterizes the TIN over the tile, as
RasMapperLib 7.0.1's export does (interoperability study, prose findings only;
no vendor code used or copied): segments from every visited source with a
two-block halo, hash-ordered per source, constrained Delaunay insertion, data
side and wider-than-2.0001-cells triangles deleted, inclusive edge-function
rasterization. One-cell gaps and source seams are bridged. The terrain HDF's
stored stitch TIN is no longer used for values; `stitch_tin` stays accepted and
validated in the request. On five real HEC-RAS 7.0.1 terrains (modifications
off, whole terrain vs export a, excluding pixels where two exports differ): B3
bit-identical; B4 1 mask pixel; B1 and B2 3 mask pixels; 0 values on all B
terrains; A 0 mask and 300 values (354 vs export a and 406 vs export b
overall), all inside gap fills where the segment order ties decide. The report
gains `export_tin` and `sample_seconds`; `stitch_tin` reports
`{"supplied", "used": false}`. Method IDs and request schemas are unchanged.

### Effective terrain: RAS Mapper's render-block sampling

The base mosaic now samples each source by render block, as RasMapperLib
7.0.1's export does: 256-pixel output tiles with recomputed tile pitch and
pixel size, block tiles half a cell inside each source, validity from bit 0 of
the block's mask byte (first source to answer wins), four-triangle values in
binary64 with the block-centre term in binary32, and the extent-derived cell
size for the default lattice. On five real multi-source terrains (HEC-RAS
7.0.1, modifications off) this leaves no value differences and no extra valid
pixels outside the stitch TIN. B3 drops from 30,628 to 3,227 mask differences
and B4 from 479 to 1 value difference. The remaining differences were pixels
RAS Mapper fills with its own per-tile export TIN, now reproduced (see the
export TIN entry above). Method IDs are unchanged.

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
stitch TIN, at the time of parking): terrain A 0 differences; B1 3 px off by
1/32 ft at exact ties; B2 432 px (96 above 1/32, max 0.84 ft) on one
levee/channel pair with short segments. Superseded by the entries above.

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
3.94 to 3.65). Spring River p02 Depth/WSE (stock and final routes) and Whisky
Chitto 1D outputs are bit-identical to 0.4.2.

Thread budget. terrio 0.4.2 ran its parallel work on the caller's rayon pool;
0.8.0 runs it on a process-wide pool of its own (`terrio-par-*`), started on
first use with `RAYON_NUM_THREADS` threads (else one per core), and offers no
native API to size, replace or bypass it. A request worker waiting on that pool
keeps running its own pool's jobs, so terrio's threads added to the request's:
with a pool as large as the request, `threads = 3` was measured with 7 threads
busy at once. The engine now keeps the budget structurally (`terrio_pool`):

- `ras2fim-batch` sets `RAYON_NUM_THREADS=1` before any thread starts, replacing
  any inherited value, so terrio's pool has one thread for every command and
  for every request of a persistent `sloping-server` session. The request JSON
  is no longer read before dispatch to size it. The engine's own pools always
  set their size.
- Every engine call into terrio that can reach that pool (ownership and storage
  burns, step05 warps, effective-terrain writes, GeoTIFF finalization) runs on a
  short-lived helper thread while the caller blocks in an OS wait; so whenever
  terrio's thread works, the worker that asked for it does not. With one terrio
  thread, burns run as one stripe on the helper. Terrain reads stay one source
  block per call and legacy GeoTIFF strips one block row per call, which terrio
  decodes or encodes on the calling thread without its pool.
- terrio's process-wide decoded-block cache is turned off (0 bytes) whenever the
  engine opens terrain: the engine caches decoded blocks itself under its own
  budgets, and an inherited `TERRIO_CACHEMAX`/`GDAL_CACHEMAX` no longer adds
  unplanned memory.
- terrio's pool is started without reading any payload (an empty in-memory
  rasterize) instead of by a one-pixel terrain read.
- `ras2fim-batch method` reports `engine.thread_budget`. Library callers that do
  not run the executable must set `RAYON_NUM_THREADS=1` before terrio is first
  used (crate docs); otherwise terrio's pool has one thread per core. The
  `one_d_session`, `storage_session` and `effective_terrain` reports add
  `terrio_pool` (`terrio_pool::status()`): the pool size configured when the
  engine first used terrio, `bounded` (false above one thread) and the block
  cache size.
- A terrio call fails with a budget error when its helper thread cannot be
  started, instead of running on the request worker.
- `run` and `step05` report `os_threads.max_running_observed` (Linux) and an
  `expected_max` of `2 * threads + 3` OS threads (request pool, helpers of its
  blocked workers, main, monitor, terrio's thread). New tests sample the
  executable's threads on Linux and Windows and check that busy threads stay
  within `threads`.

Cost: terrio's internal parallelism is gone. Burns, warps and multi-block
writes inside terrio run on one thread. On a 6,144 x 6,144 storage request
with 10,000 areas (4 threads, Windows, 2 runs each) the CLI took 2.76-3.33 s
against 2.34-2.43 s with terrio's pool at 4 threads (which exceeded the
budget). Outputs are unchanged.

### 1D terrain read with terrio; parallel inline composition

`one-d-diagnostic` schemas /2-/4 accept `terrain_tiff` (the request grid's
single-band Float32 GeoTIFF) as an alternative to a caller-staged
`terrain_file`. Admission reads the TIFF header only (no pixel block is decoded
before the budget check), refuses blocks (tiles or strips) over 16 MiB and
plans three block-sized buffers per worker, since terrio decodes whole blocks:
`(tile_cols * block_rows * 12 + 1 MiB) * threads` bytes. Rust API:
`one_d_terrain::Source::planned_bytes(window_width, threads)` became
`planned_bytes(threads)`, and `one_d_terrain::inspect` no longer refuses blocks
taller than 4,096 rows; the 16 MiB block bound replaces that limit.
The engine checks size and geotransform, decodes block-row
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
alternative to `levels_file`; exactly one is required. Rust API: `one_d_session::Request`
fields `levels_file` and `terrain_file` are now `Option<PathBuf>` (with new
`levels_files`, `terrain_tiff`, `threads`, `final_format`, `epsg`,
`aoi_mask_file`, `flooded_depth_range` and `storage` fields), a breaking change
for struct-literal callers; JSON requests are unchanged. A multi-profile run
that delivered earlier profiles before one failed writes an aborted report and
the CLI exits 2 (partial run), not 1. Inputs, terrain and the
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
