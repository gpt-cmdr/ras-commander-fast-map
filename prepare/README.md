# fast-map-prepare: native preparation of 2D plain-sloping depth groups

`fast-map-prepare` prepares the inputs Fast-Map's persistent sloping session maps from: a
`ras-commander-depth-groups-request/1` and every file it references. It does in Rust what
ras-commander 0.124.0 does in Python for fim-commander's grouped mapper
(`fim_commander.mapping._depth_groups._cohort`, the call behind native-face-catalog and
stage-axis-states), and writes the same bytes. It never runs a mapping engine of its own:
the request is for an unchanged `ras2fim-batch` (production uses 0.19.0).

This crate is standalone (its own lock file, `publish = false`); the published Fast-Map crate
and wheel do not change.

## What it does

From a plan HDF (full or slim), the mapping terrain, AOI polygons and ordered saved-time
groups:

1. 2D-only model scope, result units, mesh inventory (HDF reads through
   `ras-commander-fast-hdf` 0.5.0, the engine behind ras-commander's Rust reads).
2. Cell ownership polygons: fast-hdf's native assembly, then invalid cells (`repair`, `hull`,
   `omit`, `raise`) rebuilt and overlaps resolved with GEOS 3.13.1, the GEOS release shapely
   2.1.2 carries, making the same GEOS calls in the same order.
3. Terrain checks and the model/terrain CRS gate (no PROJ; see Differences).
4. Mesh payloads `mesh-<i>-<key>.bin` and `mesh-<i>.json`.
5. Per job: AOI union and validation, window on the global terrain lattice with rasterio's
   affine arithmetic, ownership rows intersecting the window, cell-overlap validation, AOI WKB.
6. Staged-cell closure `staged-cells-<m>.bin` and float32 WSE files
   `selectors-<w>/values-<k>-<m>.bin` (complete rows read and checked finite).
7. `cells.wkb`, the window-cache plan, `request.json` and the preparation record.

The inventory of every field, dtype and ordering is in the RC-RUSTPREP workstream notes and
summarised in `src/lib.rs`.

## Commands

```text
fast-map-prepare prepare <request.json> <control-dir>
fast-map-prepare worker <config.json>
fast-map-prepare cells <plan.hdf> <policy>
```

`prepare` takes `fast-map-depth-groups-preparation/1`:

```json
{
  "schema": "fast-map-depth-groups-preparation/1",
  "hdf_path": "UPGU1.p02.hdf",
  "terrain_path": "mapping-terrain.tif",
  "engine_path": "ras2fim-batch",
  "invalid_cells": "repair",
  "jobs": [{"job_id": "wb-2417274-p02",
            "aoi": {"wkb_files": ["reach.wkb"], "epsg": 2278},
            "groups": [{"group_id": "band-3514", "timesteps": [678, 679]}]}],
  "controls": {"threads": 1, "max_working_mib": 1024, "max_prepared_mib": 3072,
               "max_profiles_per_batch": 4096, "max_inflight_profiles": 1,
               "window_cache_mib": null, "depth_cutoff_ft": 0.5, "compression": "NONE",
               "max_accumulator_mib": 256, "max_scratch_bytes": 85899345920,
               "stage_cell_subsets": true}
}
```

It writes `<control-dir>/cohort-0/request.json`, the referenced files and
`<control-dir>/cohort-0/preparation.json` (delivery plans, the preparation record and the
prepared-context admission figures).

`worker` is a drop-in for `python -m fim_commander.mapping._depth_groups <config.json>`: it
reads the config fim-commander's `map_depth_groups` writes, prepares each cohort, runs the
engine's `sloping-server`, validates the report and the GeoTIFF headers, delivers
`<output>/<job_id>/<group_id>.tif` and writes `<control>/result.json`. The supervisor (memory
sampling, timeout, receipt) is unchanged.

Bounded corner fits: with `controls.corner_fit_bound = {"mode": "input_range", "tolerance": t}`
(fim-commander `map_depth_groups(corner_fit_bound=...)`; the `prepare` request takes the same
optional control), the request carries `model.options.corner_fit_bound`, inserted where Python's
`_cohort` inserts it, and the engine must advertise
`rasmapper-plain-sloping-bounded-corners/1`. The engine's report is then checked as
fim-commander's `check_corner_bound_report` checks it: the bounded record on the report and on
every frame. Without the control (absent or `null`) the request bytes are unchanged and a report
that carries a bound is refused. The bound is validated as `validated_bound` validates it, with
one refusal of its own: a nonzero tolerance below 1e-4 or from 1e16 up, which Python writes with
an exponent and this crate would write differently. A control key the worker does not know is
refused (`FimUnsupportedError`), so a control added to the Python worker cannot be dropped here.

`cells` prints the ownership rows and invalid-cell record (`load_cells` + `require_cells`),
for parity checks. `version` prints the name, version and build commit.

`wkb_files` hold bare WKB (as in a GeoParquet geometry column), not GeoPackage blobs.

Refusals exit 2 and print `{"status":"refused","kind":...,"message":...}`. The worker writes
`result.json` as `_worker` does (`status: aborted`, `error: "<class>: <message>"`), also for an
internal fault (`RuntimeError: internal error (panic): ...`). For refusals it shares with
ras-commander it records the same exception class, and for the ported ones the same message;
messages from its own HDF, TIFF and JSON parsing differ. It pins itself to the supervisor's CPU
partition (Linux) and sets `OMP_NUM_THREADS` for the engine, as `_worker` does.

## Parity

Value identity is the contract: the request fields and every referenced file must equal
what ras-commander 0.124.0 writes for the same inputs, byte for byte. Exceptions, each
compared another way:

- absolute paths (compared after mapping to the staging roots);
- `model.window_cache_bytes`, which ras-commander charges by the length of the absolute
  staging paths (compared with fim-commander's `plan_window_cache` evaluated on this request);
- timings and the engine's run-specific `lattice.parent_identity` cache key.

Evidence (2026-11-20): UPGU1 and UPGU3 slim p02 plan HDFs, all 222 reaches: identical
(14,208 files, 16.9 GB); the Gen 5.1 canary end to end through `map_depth_groups`: 52 grouped
GeoTIFFs byte-identical. `tests/` checks the invalid-cell policies against ras-commander's
own synthetic meshes and the window arithmetic against values Python computed.

## Differences

### Refusals

The preparation is meant never to accept input ras-commander refuses, and it refuses some input
ras-commander accepts:

- an AOI whose CRS does not declare the terrain's projected EPSG code (ras-commander compares
  with PROJ and reprojects when they differ). The declared code is trusted: a WKT2 `PROJCRS`
  carrying `ID["EPSG",2278]` over a different conversion would be used unprojected. pyproj's
  `to_wkt()`, which fim-commander writes, never produces such a CRS;
- a terrain without an explicit EPSG `ProjectedCSTypeGeoKey`, with an external mask
  (`.msk`), with a GDAL sidecar that could change its CRS or geotransform (`.aux.xml`
  SRS/GeoTransform, world file), or whose size, geotransform or EPSG the engine's `terrain-info`
  reports differently. `terrain-info` uses the engine's own reader, so this check catches engine
  skew, not a GDAL/terrio disagreement;
- model CRS evidence (`HdfUtils.collect_hdf_crs_evidence`: the plan HDF's `Projection`, then the
  `.rasmap` projection file and terrain layers) that does not match the terrain's EPSG
  definition parameter by parameter: projection method (LCC 2SP, Transverse Mercator, Albers),
  datum name, ellipsoid, prime meridian, angular and linear units, axes and parameters within
  PROJ's 1e-10 relative tolerance; WKT with `TOWGS84`, `EXTENSION`, other elements or bare words
  outside `AXIS`. Evidence ras-commander records as unreadable and lets go without a vote is
  refused here: a `.rasmap` that is not UTF-8 XML, a terrain layer HDF that cannot be opened, a
  `.prj` over 1 MiB, and a `.vrt` terrain layer. A GeoTIFF terrain layer votes with its explicit
  EPSG key (rasterio would read its full CRS). `.rasmap` stems are compared with `to_lowercase`,
  not `casefold`;
- unit attributes outside `True/False/1/0` and `SI Units/SI/US Customary`, and the root `SI
  Units` / geometry `SI Units = False` disagreement ras-commander resolves from dataset labels;
- `postprocessing_hdf`, depth-weighted faces, `slopingPretty`, shallow-to-horizontal and staging
  without cell subsets.

### Tiles

Water surface comes from the plan HDF. Where fim-commander would stage a wave from a parent
extract's tiles (`wse_tiles`, `_tile_layout`), only the staged cells' columns are read and only
those values must be finite, as on the tile path; the wave records
`wse_source: plan-hdf-staged-cells/1` instead of `parent-extract-cell-wse-tiles/1`. Elsewhere
complete rows are read and must be finite, as on the HDF path. One acceptance difference: if
tiles exist but cannot be read, ras-commander falls back to complete rows and refuses a
non-finite value outside the staged closure; this preparation keeps the staged-only check.

### Errors

A refusal ras-commander and this preparation share has the same Python class; the message is
ported where Python's is fixed text. Known differences, none parsed by fim-commander:

- a changed source in a multi-job cohort names the first job, not the changed one; a change
  during the engine run is reported after delivery as `RuntimeError`, where Python raises
  `FimEngineError` before the remaining jobs are delivered; the changed-key list repeats keys
  per job;
- engine capabilities come from the supervisor's record in the config, not a fresh
  `ras2fim-batch method`, and the gates are checked in a different order;
- unit-resolution refusals keep Python's prefix with this crate's own detail;
- with interleaved band groups and two non-finite staged selectors, the tile-path refusal can
  name a different selector.

## Building

GEOS 3.13.1 must be available as a shared library:

```bash
GEOS_LIB_DIR=/path/to/geos-3.13.1/lib GEOS_VERSION=3.13.1 cargo build --release --locked
```

GEOS is LGPL-2.1 and linked dynamically; ship `libgeos_c` and `libgeos` 3.13.1 with the
binary. CI builds against conda-forge `geos=3.13.1=h97f6797_0` and uploads the binary with
`libgeos_c.so.1`, `libgeos.so.3.13.1` and the conda `libstdc++.so.6` / `libgcc_s.so.1` they were
built against (pinned), found next to the binary (rpath `$ORIGIN`).
`tests/fixtures/make_fixtures.py` regenerates the fixtures with the oracle pins.
