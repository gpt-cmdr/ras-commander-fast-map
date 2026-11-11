# Reproduction

## Build

Branch `feat/effective-terrain-modifications` (this repo) and
`feat/fim-effective-terrain-modifications` (gpt-cmdr/ras-commander, editable install).
Both were stacked on each fork's `t13/integration` branch, which carries fast-map
#18-#20 and ras-commander #755/#757/#758 plus the `build_effective_terrain` wiring
(ras-commander draft #788). Rebase onto `main` once those land.

Gates used: `cargo fmt --check`; `cargo clippy --all-targets -D warnings` (only the
pre-existing `chunks_exact_to_as_chunks` at `src/aoi.rs:109`); `cargo test --locked
--all-targets`; `python scripts/verify_sources.py` (refresh with `--write` when
`src`/`tests` change). ras-commander: `tests/test_*fim*.py` and
`tests/test_*terrain*.py`, including `tests/test_fim_terrain_mods.py`.

## References

Always on a **copy** of the project and terrain, never the original:

```python
from ras_commander import RasTerrain
# base (no modifications) and full (with modifications)
RasTerrain.export_rasmapper_terrain(..., downsample_factor=1, rasterize_modifications=False)
RasTerrain.export_rasmapper_terrain(..., downsample_factor=1, rasterize_modifications=True)
```

Needs HEC-RAS 7.0.1 (Windows or Wine). For one layer, reduce an HDF copy to that
layer, or use a small `extent=` export, comparing extent references only against
extent base exports.

Repeat each reference export at least twice: some stitch-TIN pixels vary between
identical RAS Mapper exports, and only differences outside that set count against
the rule.

## Qualification

`scripts/qualify_effective_terrain.py <engine> <terrain.hdf> <export.tif> <work-dir>`
compares the engine output with an export (mask and value differences, by
contributing source, TIN or modification). Modified pixels are those where the
full export differs from the base export.

Python reference evaluators used to derive the rules (`gl.py`, `tinmodel.py`,
`modeval.py`, `modeval_w.py`, `qual2.py`) were kept with the research scratch on
the build host, not in this repo.
