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

- Horizontal: cell-constant WSE with native float32 visibility.
- Sloping: reconstructed cell corners; optional cell faces, depth weighting,
  and shallow-to-horizontal selection.
- ras2fim-2D Step05: the separate globally prepared SciPy triangulation method.

Terrain decoding and ownership rasterization use terrio with GDAL disabled.
Rayon workers share immutable terrain/geometry across bounded in-flight profiles.
Bbox/polygon output masks retain global hydraulic topology. Planned allocation
admission is not a hard RSS limit. Python delivery may use Rasterio/GDAL;
the Rust mapping executable does not link it.

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
