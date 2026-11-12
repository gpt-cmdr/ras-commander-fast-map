# Grouped depth maximum contract

`ras2fim-batch depth-groups request.json new-output-directory` renders each saved
time through the existing plain sloping mapper, then reduces finalized depth
arrays after each job's global-lattice AOI mask. Only finite non-nodata depths
at or above the cutoff contribute. Equal maxima retain the first contributing
Float32 value; accepted values are not rounded or recomputed. All-dry groups
retain their full requested rectangular window as nodata. There is no wet crop,
reprojection, packing, NetCDF schema or hydraulic solve in this command.

Inspect `ras2fim-batch method` for `depth_groups` capabilities. The request and
report schemas are `ras-commander-depth-groups-request/1` and
`ras-commander-depth-groups-report/1`. The separate reduction method is
`masked-float32-depth-group-maximum/1`. Ordinary mapping method IDs are unchanged.
A private real-model numerical check matches complete masks and valid Float32
bits against independently reduced native per-time depths for the physical 2D
component of private mixed fixture C p67: three selected profiles, inclusive 0.5-foot cutoff,
full and cropped windows, original/reversed/single-profile groups, both resident
NONE and forced local-spill LZW output. Candidate topology, terrain and physical
ownership are independent of native flood-reference masks. This is an explicitly
prepared 2D component of a mixed model, not public mixed-model API qualification.
The custom saved-time reduction is not generally equivalent to rendering the
native Max-WSE profile. Performance qualification remains untested.

## Native product controls

Existing sloping CLI/session requests can add `products: ["depth"]` and
`compression: "NONE"` alongside `stream_outputs: true, geotiff_outputs: true`.
Products must be a nonempty unique selection of `depth` and `wse`; compression
accepts exactly NONE or LZW. Omitting both controls preserves both files, LZW and
the legacy completion header. Explicit controls add `selected_products` and
`compression` to the existing native completion record. The renderer still
allocates both internal products. Per-frame depth files remain on this route.

The public Rust `sloping_session::Request` is unchanged. Rust callers use the
new `native_output::OutputOptions` and `sloping_session::run_with_output_options`.
`native_output::parse_request` handles wire extensions and strictly parses the
remaining base request; it does not discard unknown keys. Older engines reject
the new fields. Supervisors must check capabilities and validate selected
products without silently changing writers or accepting missing products.

## Request

The model is one existing sloping request containing the complete global mesh,
physical-cell polygons and staged WSE selectors. Paths are relative to the
grouped request directory unless absolute. All jobs share this compatible
parent. Multiple incompatible parents require separate requests/supervisor
cohorts under the caller's global CPU and memory budget.

```json
{
  "schema": "ras-commander-depth-groups-request/1",
  "model": {
    "schema": "ras-commander-sloping-request/1",
    "terrain": "terrain.tif",
    "epsg": 3857,
    "crs_identity": {"kind": "epsg"},
    "terrain_transform": [0, 1, 0, 256, 0, -1],
    "window": [0, 0, 256, 256],
    "geometry_file": "cells.wkb",
    "mesh_files": ["mesh.json"],
    "owner_cells": [[0, 0]],
    "profiles": [
      {"id": "time-0", "values_files": ["time-0.f32"]},
      {"id": "time-1", "values_files": ["time-1.f32"]}
    ],
    "threads": 4,
    "max_inflight_profiles": 1,
    "max_working_bytes": 268435456,
    "window_cache_bytes": 67108864,
    "hydraulic_cache_bytes": 0
  },
  "source_indices": [0, 1],
  "jobs": [
    {
      "job_id": "reach-a-plan",
      "reach_id": "reach-a",
      "window": [0, 0, 128, 256],
      "aoi_file": "reach-a.wkb",
      "groups": [{"group_id": "band-000", "timesteps": [0, 1]}]
    },
    {
      "job_id": "reach-b-plan",
      "reach_id": "reach-b",
      "window": [128, 0, 128, 256],
      "aoi_file": "reach-b.wkb",
      "groups": [{"group_id": "band-000", "timesteps": [1, 0]}]
    }
  ],
  "depth_units": "feet",
  "depth_cutoff_ft": 0.5,
  "products": ["depth_max"],
  "compression": "NONE",
  "sink": "grouped-geotiff",
  "max_accumulator_bytes": 67108864,
  "scratch": {"path": "local-scratch", "node_local": true, "max_bytes": 1073741824}
}
```

The geometry/selector files must satisfy the ordinary sloping contracts; the
example's coordinates and owner list are illustrative. Each source index pairs
with the profile at the same position. Indices and profile IDs are unique.
Every supplied source must be requested. Each group is nonempty and has no
duplicate index; sharing indices between groups/jobs is intentional and allowed.
Job IDs and within-job group IDs use existing portable, case-insensitive ID
rules. Input and receipt ordering is preserved even if source execution order
differs from a group's list. `Max` selectors, velocity, derived products and
slopingPretty are refused by this initial grouped contract. Storage-area inputs
remain subject to ordinary sloping validation and terrain-scope restrictions.

### Staged cell subsets

`staged_cells_files` (optional, in the model; capability `staged_cells:
"ras-commander-staged-cells/1"` in both `sloping` and `depth_groups`) lets a
caller stage only the cells a reach can see. It lists one file per mesh, each a
little-endian `u32` array of strictly ascending global cell ids, and every
profile values file then holds exactly those cells' WSE in that order.
`owner_cells` stay global ids.

The engine loads the compact stencil closure of the owner cells: every face
endpoint of an owner cell (its vertices), the complete face ring of each such
vertex, and both cells of every face in those rings, ghost cells included.
Corner and face values depend only on those inputs, are computed per element in
the same ring order, and the stencils, physical perimeter rings and pixel
ownership are unchanged, so rendered Depth/WSE are bit-identical to the
complete mesh. A subset that lacks an owner cell or any closure cell is
refused; extra cells are admitted. A mesh without owner cells needs any
nonempty subset and is not read.

The load holds one complete mesh array at a time; that transient (largest
array plus the staged list) is admitted against the working budget before any
payload read, and the geometry allowance of the array plan uses the compact
rows' binary size in place of the complete mesh's. The report carries
`staged_cells` (staged, owner and closure counts, parent counts, equivalent
bytes and load transient). Velocity, shear, storage areas, terrain scope, the
stored tile grid, `reduce_max`/derived products and ownership export are
refused with a subset. Without the field the request is unchanged.

`depth_units` explicitly means the mapped depths' vertical units; it must be
`feet` or `metres`. It is not inferred from horizontal CRS units. The feet cutoff
defaults to 0.5 and is converted with the exact implementation expression
`float32(depth_cutoff_ft * 0.3048)` for metres (the multiplication is binary64).
For feet it is cast directly to Float32. Negative/nonfinite cutoffs are refused.
The receipt records both requested feet and effective model-unit threshold.

## Resources and sharing

Execution uses one worker, one in-flight rendered frame and the model's bounded
Rayon pool. For each source selector, hydraulic state is pinned while every
requesting reach renders its own window. LRU eviction cannot force hydraulic
reconstruction between these reaches. Windows and terrain use the existing
bounded caches. A session accepts successive grouped requests and retains
compatible parent preparation; source staging/waves are the caller's job.

The plan includes mapping state, cache ceilings, resident accumulators, output
and transfer allowances, row-writer metadata and receipt/index overhead. Admission
inspects descriptors and terrain headers before reading binary terrain/geometry
payloads. It reports required/allowed bytes; it is not a hard process RSS limit.
FIM's 160 MB/320 MB dense packing budget is separate from these controls.

Each accumulator requires four bytes per native window pixel. Groups that fit
`max_accumulator_bytes` stay resident in request order; others spill. If spill
is needed, `scratch` must name an existing explicitly provisioned node-local
directory and allow the entire spill payload. Zero resident bytes spills all
groups. The caller attests locality with `node_local: true`; Windows UNC paths
are additionally refused. On other platforms the caller must provision local
storage. The engine never falls back to the output path. It creates a private
invocation directory and removes only that directory on normal/error return.
The scratch ceiling admits logical bytes; disk exhaustion remains an explicit
I/O failure. Process crashes can leave this private scratch for operator cleanup.

Spill updates and GeoTIFF reads/writes use bounded chunks/rows. terrio's
`GeoTiffWriter` (0.4.2 when this was qualified) permits final delivery directly from spilled rows. No complete
group is rehydrated for GeoTIFF output. A conservative largest-group allowance
also admits a single owned Rust buffer delivery. Downstream consumers must
budget buffers they retain after delivery separately.

## Outputs and receipts

The GeoTIFF sink writes only `job_id/group_id.tif` and aggregate `report.json`.
Each raster is Float32 with NaN nodata on its original lattice, NONE or LZW
compression and the source EPSG. No per-frame depth/WSE files, unit manifests or
floodplain polygons are created. A temporary `.tif.partial` is renamed only after
the writer finishes. Handled failures remove delivered rasters, mark all groups
failed and preserve the aggregate receipt and observed/pending sample entries.

Frame statuses are `complete` when any finite positive depth exists before
the cutoff and `no_flooded_pixels` otherwise. Each frame records that native wet
count separately from its post-cutoff retained count. Counts use no 10,000-unit
display ceiling. Final groups record their retained pixel count and `all_dry`.
Group metadata includes ordered source indices/frame results, full affine,
strict rectangular extent, shape, CRS WKT/EPSG, source lattice/window identity,
units/cutoff, decoded Float32 FNV-1a identity and output identity. Delivery WKT
uses the canonical definition of the validated EPSG code. Its top-level WKT
fingerprint is separate from `lattice.source_crs_wkt_fnv1a64`, which records the
terrain reader's `terrain-info` WKT representation. FNV is an
integrity identity, not a cryptographic provenance guarantee. The report pins
engine version/revision and terrio, and reports native preparation counts,
prepare/render/reduce/write timers and logical scratch peak/read/write bytes.
`preparation_waves` is one staged request; it does not measure upstream HDF waves.

Only aggregate `status: complete` is success. Pending frames, failed groups,
unknown statuses, an aborted report or nonzero process exit must be rejected.
Partial output is never a substitute for a requested sample. Buffers already
delivered to callers remain tentative until the entire command completes.

## Grouped buffers

Use `sink: "grouped-buffer"` for CLI delivery without raster intermediates.
For each group in request order, stdout contains one UTF-8 JSON header line:

```json
{"schema":"ras-commander-depth-group-buffer/1","group":{"job_id":"reach-a-plan","group_id":"band-000","byte_length":131072,"dtype":"<f4"}}
```

The actual `group` object also supplies the shape/affine/CRS/nodata/units/lattice
and ordered sample metadata described above. Immediately after the newline are
exactly `byte_length` bytes of row-major little-endian Float32 values, with no
separator. Then comes the next header or terminal JSON line
`{"command_status":"complete"}`. Read exact lengths; never scan binary data for
newlines. Reject truncated payloads, unexpected identities/lengths, repeated or
missing groups, failure terminal records or missing terminal completion. Pipes
apply synchronous backpressure. `report.json` remains the authoritative receipt.

Rust callers use `depth_groups::run_with_session` and a `BufferSink`; the
`BinarySink<W>` implements this protocol. `OwnedBufferSink::new(callback)` hands
the callback metadata and one owned `Vec<f32>`. It introduces no Python/NumPy copy.
The `sloping-server` command dispatches grouped-schema request files to the same
session and emits one terminal command status, as with ordinary requests.

## Application integration

Applications stage saved selectors, qualify depth units and compatible parent
identities, supervise global memory/CPU budgets and record actual preparation
waves. A caller can use existing model/HDF preparation primitives without adding
a public mapping wrapper to those libraries. Native product/compression options
are wire capabilities, independent of any Python API. Application orchestration
is outside this standalone crate and its binary-only wheel.

Applications retain station/band selection, discharge labels and merges, nearest
EPSG:3857 reprojection, wet crop, block maxima, packing and viewer checks. The
coordinator must authorize the exact two-reach packet before external acceptance
or benchmarks. Generated-fixture software parity alone establishes neither
hydraulic acceptance nor a wall-time speedup.
