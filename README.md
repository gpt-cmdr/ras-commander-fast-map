# RAS Commander Fast-Map

<p align="center"><img src="docs/images/logo/ras-commander-fast-map_logo.svg" alt="RAS Commander Fast-Map: Native Rust Mapping Engine" width="450"></p>

Native Rust 2D flood inundation mapping, distributed as the
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
let methods = ras_commander_fast_map::method_record();
assert!(methods.is_object());
```

See [Rust API documentation](https://docs.rs/ras-commander-fast-map) and the
[RasFim guide](https://rascommander.info/ras/api/fim/) for request contracts and
end-to-end model preparation. The Rust library consumes prepared geometry,
profiles and terrain; HDF extraction belongs to `ras-commander-fast-hdf` and
Commander. The CLI does not read a plan HDF directly.

## Methods and resource controls

### Polygon AOI edge rule (0.10.1)

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

### Retained reach state (development)

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
engine. This route requires an EPSG-identifiable CRS and excludes velocity.
Commander validates output metadata before delivery. Ordinary array streams
and binary-file output remain compatible. These are development capabilities;
use an explicit qualified build until the next release is published.

Binary32 operation order and method identifiers are preserved. `MIGRATION.json`
retains the immutable 0.8.0 extraction receipt; `SOURCE_MANIFEST.json` binds this
version's current sources. `ras2fim-batch method` reports package/build identity
and capabilities.

## Persistent reach sessions (0.9)

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

## Deterministic inundation boundaries (0.10)

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
terrain modifications and multi-source mosaics are outside this boundary scope.
Commander rejects models containing 1D elements; standalone storage-area surfaces
are excluded.

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

## Velocity (sloping requests)

Add per-mesh `velocity_normals_files` (HDF `Faces NormalUnitVector and Length`
as float32 x, y, length, with **length replaced by the face polyline length**)
and per-profile `face_velocity_files` (float32 face-normal velocities; for Max,
the summary `Maximum Face Velocity` with Max WSE as the profile values) to a
`sloping` request. Velocity magnitude is written as `velocity.bin`, or as a
third streamed array when the stream header has `"velocity": true`. It is drawn
where rendered depth > 0 and does not depend on the WSE render mode (method
`rasmapper-sloping-velocity/0`).

Native HEC-RAS 7.0.1 comparison: BaldEagleCrkMulti2D bridges p05 (973,532 Max
and 402,500 output-time pixels), dam-break p07 2D cells (Max and one output
time, valid masks equal to depth) and a 2,500 x 2,500 Spring Creek eBFE AOI
(1,101,529 pixels) match bit for bit; Davis p02 Max differs in 5 of 20,560
pixels by at most 2 ulp. Storage areas, boundary requests, shear stress and Max
derived products are outside this method.

## Whole-simulation products (arrival time, duration, percent inundated)

```bash
ras2fim-batch whole-simulation whole-request.json new-output-dir
```

Schema `ras-commander-whole-simulation-request/1` embeds a non-streaming
horizontal request whose profiles are the stored output times in order, plus
`times_hours` (one per profile, strictly increasing), `threshold`,
`cell_minimum_file` (float32 cell minimum elevations in cell order), optional
`products` (default `arrival_time`, `percent_inundated`) and
`allow_unqualified` (required for `duration` and irregular intervals). Output times are evaluated sequentially over
the mapped 2D-owned pixels only, in parallel, with 32
bytes per mapped pixel. Products are written to `whole_simulation/<product>.bin`
(float32, NaN = never wet).

A pixel is wet when its horizontal depth is `> threshold` and its cell passes
RAS Mapper's thresholded gate `wse > minimum + threshold`. Storage areas are
not rendered; Commander refuses plans that contain them. Arrival is the first wet time and percent is
`100 * wet / n`. Duration runs from arrival to the first dry time after the last
wet one, or to the last wet time when that dry time would be the final output
or there is none. Times are float32 hours. Irregular output intervals are
refused unless `allow_unqualified` is set. Initially wet pixels, multi-period
pixels and threshold-equal samples are reported.

Qualification (native HEC-RAS 7.0.1, threshold 0.5 ft, storage-area-free
models): Davis pipes/conduits p02 (217 output times, 4,879 valid pixels) and
BaldEagleCrkMulti2D bridges p05 (433 times, 959,043 pixels) match in every
valid-mask pixel and every arrival and percent value bit for bit, including
initially wet, multi-period and threshold-equal samples. RAS Mapper gives the
same products in horizontal and sloping render modes on both models, so this
horizontal pass covers sloping maps too. Duration is opt-in and unqualified:
103 (Davis) and 11 (bridges) native durations are one output interval shorter.
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

## Qualification and limits

The retained [five-model comparisons](https://rascommander.info/ras/development/fim-mapping-performance/)
cover raw Depth/WSE validity masks and float32 bits against native HEC-RAS 7.0.1,
without an added depth cutoff. Those published timing observations predate this
packaging extraction; no new speedup is claimed for renaming/distribution.

Mapping covers physical 2D cells. Commander rejects models with 1D river/cross-
section geometry. Standalone storage-area surfaces are excluded. Cell-face
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
