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

If registry authentication is configured after the GitHub Release exists,
dispatch release.yml at that tag with `publish_existing=true`. It verifies the
tag/version and existing release checksums, then publishes to each enabled
registry without rebuilding or replacing the release assets. Enable only the
registries that have not yet received this version when retrying publication.
