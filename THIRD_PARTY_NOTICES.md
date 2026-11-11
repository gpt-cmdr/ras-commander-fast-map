# Mapping engine provenance

The Step05 workflow and its sample-preparation method originate in RAS2FIM-2D,
Copyright (c) 2026, The University of Texas at Austin. The repository's BSD
3-Clause license and notices remain in effect for that work.
The full notice is retained in [licenses/ras2fim-LICENSE.txt](licenses/ras2fim-LICENSE.txt).

The Rust interpolation and point-location implementation follows SciPy's
`LinearNDInterpolator` and Delaunay transform/walk logic. The Python adapter
uses SciPy to prepare the ordered triangulation and transforms. SciPy is
distributed under the BSD 3-Clause license; its notice is retained in
[`licenses/scipy-LICENSE.txt`](licenses/scipy-LICENSE.txt). Relevant sources:

- [SciPy interpolation source, v1.18.1](https://github.com/scipy/scipy/blob/v1.18.1/scipy/interpolate/_interpnd.pyx)
- [SciPy Delaunay source, v1.18.1](https://github.com/scipy/scipy/blob/v1.18.1/scipy/spatial/_qhull.pyx)

Raster I/O, approximate reprojection and horizontal polygon rasterization use the statically linked
[`terrio` 0.4.2](https://github.com/opengeos/terrio) dependency, under its MIT
license, retained in `licenses/terrio-LICENSE.txt`. Terrio ports GDAL scanline
rasterization rules; its ownership fill preserves feature order and pixel-centre
inclusion. `wbvector` 0.1.6 and `wbraster` 0.2.1 provide geometry and typed raster
data; their MIT/Apache notices are retained in `licenses/`.
The engine does not link GDAL. The surrounding Python workflow
continues to use rasterio and its raster stack for preparation and file output.

The opt-in final delivery GeoTIFF writer is this project's own code. Its tile
compression uses `flate2` with the pure-Rust `zlib-rs` backend. Its overview
arithmetic reimplements the masked `AVERAGE` rule of GDAL's
`GDALResampleChunk_AverageOrRMS`, `ARE_REAL_EQUAL` nodata mask and
`GDALGetNoDataReplacementValue` (GDAL is MIT licensed), following the
description of those functions in the `gdal_overview` port of later terrio
releases (MIT); no GDAL or terrio source is copied and GDAL is not linked.

Deterministic inundation-boundary integer union and AOI clipping use the
unmodified [`clipper2-rust` 1.2.0](https://github.com/larsbrubaker/clipper2-rust)
crate under the Boost Software License 1.0 (`BSL-1.0`). Its source-distributed
license is retained in
[`licenses/dependencies/clipper2-rust-1.2.0/LICENSE`](licenses/dependencies/clipper2-rust-1.2.0/LICENSE).
`Cargo.lock` pins the crates.io archive and checksum; the distribution source
manifest binds the retained license. This pure Rust dependency introduces no
GDAL or HEC-RAS runtime requirement. Its integer intersections and union behavior
are not claimed to reproduce RASMapper vector output.

The explicit native-serial boundary path uses a private Rust clipping core in
`src/boundary_clip_legacy/`, adapted from the classic core of
[`solidmesh-clipper` 0.2.0](https://github.com/larsbrubaker/solidmesh/tree/05a748e8683f25bc1bfc24f39b806fb70ae3b156/crates/clipper-library).
That Rust source carries MatterHackers' BSD-2-Clause notice and the original
Angus Johnson Clipper Boost Software License 1.0 notice. Compatibility changes
follow the horizontal-edge and scanbeam lifecycle and original-edge join predicates in
[official Clipper 6.2.1 revision 482](https://svn.code.sf.net/p/polyclipping/code/!svn/bc/482/trunk/C%23/clipper_library/clipper.cs)
under the Boost license. Its host-specific crossing-list sorts follow Microsoft's
[reference-source depth-limited quicksort and introsort](https://github.com/microsoft/referencesource/blob/ec9fa9ae770d522a5b5f0607898044b7478574a3/mscorlib/system/collections/generic/arraysorthelper.cs)
under the MIT license. Full license texts, the original Rust package's complete
third-party notice and immutable source checksums are retained in
[`licenses/vendored/boundary_clip_legacy/`](licenses/vendored/boundary_clip_legacy/).
The vendored core uses the existing Clipper2 integer-point types. Neither
`solidmesh-clipper` nor `solidmesh-math` is a runtime dependency. Numerical
qualification is documented separately from this software provenance.

Other Rust dependencies and their exact resolved versions are recorded in
`Cargo.lock`. Retain their applicable notices when distributing compiled
binaries. Resolved dependency license texts are retained under
`licenses/dependencies/`; `licenses/UPSTREAM_NOTICES.json` records pinned upstream
sources or selected Apache-2.0 alternatives for archives that omit license text.
This provenance describes software reuse; it does not imply
endorsement by upstream projects.

The separate plain-sloping method is an independent implementation qualified
against HEC-RAS 7.0.1 stored-map output and native intermediate values. HEC-RAS
assemblies are not included in the engine or wheels. This comparison does not imply HEC or USACE endorsement or support.
