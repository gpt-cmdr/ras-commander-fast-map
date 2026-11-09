# Private classic clipping core notices

`src/boundary_clip_legacy/` adapts the classic Rust core from
solidmesh-clipper 0.2.0. The copied Rust files retain the upstream BSD-2-Clause
and original Clipper Boost Software License 1.0 obligations. The offset module is excluded.
Compatibility changes follow selected algorithms from the official
Boost-licensed Clipper 6.2.1 source and Microsoft's MIT-licensed reference-source
legacy quicksort and modern introsort. Tile/TIN work selects the legacy policy;
the final flat merge selects the modern policy. Compatibility includes the
original-edge join predicates; it does not replace the core wholesale.
This is a private Rust implementation; neither the original
C# source nor a HEC-RAS binary is included.

- LICENSE-solidmesh-clipper-BSD-2-Clause.txt: complete upstream Rust package license.
- solidmesh-clipper-THIRD_PARTY_NOTICES.md: complete source-distributed notice.
- LICENSE-Clipper-BSL-1.0.txt: official Clipper license at immutable revision 482.
- LICENSE-Microsoft-MIT.txt: Microsoft reference-source license at a pinned commit.
- UPSTREAM.json: immutable source locations, archive and source checksums,
  per-file Rust upstream checksums, adaptation scope and notice checksums.

These notices describe source reuse. Numerical qualification is recorded
separately. `Cargo.lock` records resolved dependencies; the vendored Rust core
has no dependency on either solidmesh package.

Upstream checksums describe original downloaded bytes. The notice inventory
also supplies LF-normalized hashes for checkouts that convert text newlines.
