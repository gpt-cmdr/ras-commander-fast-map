# RAS Commander Fast-Map

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
expression is `MIT AND BSD-3-Clause`. Dependency notices accompany distributions.
HEC-RAS assemblies, decompiled vendor source and model data are not distributed.
This independent project is not affiliated with or endorsed by HEC/USACE.
