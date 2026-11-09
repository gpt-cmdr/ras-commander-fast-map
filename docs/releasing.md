# Release procedure

The Rust crate and Python wheel share one version in Cargo.toml and
pyproject.toml. The executable remains ras2fim-batch for compatibility. Official
builds report the source revision through `ras2fim-batch method`.

1. Update both versions and CHANGELOG.md. Preserve numeric contracts and notices.
2. Run scripts/verify_sources.py --write for intentional distribution changes,
   then verify without --write. Extraction0.8.0 must retain its numerical hashes.
3. Run cargo fmt, clippy, all-target tests and cargo publish --locked --dry-run.
4. Merge the reviewed PR with CI green. Qualify Windows/Linux installed wheels
   against Commander real-model masks/bits, timesteps, AOIs and concurrency.
5. Tag that clean qualified main commit vX.Y.Z and push the tag. Release CI
   builds wheels and source distribution, verifies provenance, then attaches
   artifacts and SHA256SUMS to a GitHub Release.
6. Set PUBLISH_PYPI=true only after PyPI trusts this repository's release.yml
   workflow and pypi environment, or provide that environment's PYPI_API_TOKEN.
   PyPI pending trusted publishers can bootstrap a new project. No token should
   appear in files, command output or repository history.
7. Publish the crate from the qualified tag with cargo publish --locked, using
   the maintainer's configured Cargo credential provider. The workflow's
   optional crates job uses environment crates-io / CARGO_REGISTRY_TOKEN when
   PUBLISH_CRATES=true. Inspect the package list before publication; crate
   versions cannot be replaced.
8. Verify both registries, wheel installation on Windows/Linux, executable
   identity and `cargo install ras-commander-fast-map --locked`.

The crate contains only Rust sources and its source/provenance/license files;
no Python/GDAL/HEC dependency is required to use it. The wheel is a binary
distribution, not a Python numerical fallback. The source distribution supports
explicit local compilation on other systems; only qualified wheel platforms
should be added to Commander's automatic dependency marker.

Do not retag an immutable published version or silently skip failed artifacts.
Retain public qualification summaries while keeping external model files,
computed rasters and native reference caches outside the repositories.

Wheel builds retain their complete CycloneDX dependency graphs. Before smoke
checks, checksums or artifact upload, run
`python scripts/sanitize_wheel_sbom.py --input-dir dist-raw --output-dir dist`
to replace build-local workspace identifiers and their references
with portable identifiers. The check must preserve component/dependency counts,
validate every reference and regenerate `RECORD`; executable, license and other
payload bytes must remain identical. Unrecognized local paths or inconsistent
graphs fail the build. Keep unsanitized local wheels outside publication folders.

The native boundary request exposes `additional_terrains`, `contour_method`,
`diagnostic_units` and `one_d` fields on `boundary_session::Request`. Rust
struct-literal callers must supply these fields. Treat that source compatibility
change deliberately when choosing the crate's minor release and documenting
request construction; JSON defaults do not supply omitted Rust struct fields.

If registry authentication is configured after the GitHub Release exists,
dispatch release.yml at that tag with `publish_existing=true`. It verifies the
tag/version and existing release checksums, then publishes to each enabled
registry without rebuilding or replacing the release assets. Enable only the
registries that have not yet received this version when retrying publication.

## Mapping integration gates

Unmodified effective-terrain schema `/1` retains its numerical contract.
Terrain modifications require separate qualification. Row streaming is opt-in
(0 default, at most 4096 rows), restricted to plain corner/native TIFF output;
full ownership and ground remain resident. Installed-wheel/native fixture
checks must identify the exact artifact, input scope and options. Windows local
evidence does not replace Linux installed-wheel qualification or review.
Retain unresolved native-repeat variation as a qualification limitation;
withhold timing summaries for unstable cases and avoid all-model parity claims.
