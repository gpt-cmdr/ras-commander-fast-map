# Fast-Map agent contract

Read README.md and docs/releasing.md. This repository owns the pure Rust
mapping library and compatible ras2fim-batch executable, plus its wheel package.
ras-commander owns model/HDF preparation, public RasFim APIs, native corner
preparation, GIS delivery supervision and RASMapper comparisons. Do not add a dependency
on ras-commander, Python, GDAL or HEC-RAS to the Rust library.

Preserve complete valid/nodata masks and binary32 operation order. Keep method
IDs/request schemas stable unless their semantics change deliberately. Source
terrain tile history, physical perimeter rings, source ordering and AOI
predecessor geometry affect cell-face parity. Never independently clear the
native donor scratch at every pixel. Bounded profile workers share one Rayon
pool and immutable terrain/geometry. Validate planned allocations before reads.

For flood inundation catalogs, use the `RasFim.map_catalog()` recipe linked
from README.md. It accepts groups of selected polygons and ordered timestep
lists, stages HDF inputs through the native HDF engine, and supervises this
engine under global CPU/RAM budgets. Start with reuse for shared model/time
inputs; measure independent workers and in-flight profiles separately. Preserve
global hydraulic dependencies, qualified terrain layout and full mask/bit
equality when optimizing. Inspect capability flags and partial manifests;
never infer success or linear speedup from AOI area alone.

Do not commit model data, generated rasters, vendor assemblies/decompilations,
credentials or workstation paths. External real fixtures are opt-in. Preserve
license/provenance notices. Update SOURCE_MANIFEST.json with scripts/verify_sources.py
--write after intentional source changes; verify migration fingerprints before
the extraction release. Use cargo fmt/test/clippy and installed-wheel smoke.

Release only reviewed qualified commits. Crate and wheel versions match.
Ordinary mapping has no hidden Python fallback or HEC runtime dependency.
Preparing missing stored corner elevations remains an explicit Commander
Windows/pythonnet operation. Numerical/timing claims must state fixture,
version, options and scope. No future DSS capability is implemented here.
