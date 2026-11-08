# Method discovery and compatibility contract

`ras2fim-batch method` and Rust `engine_method_record()` return the same registry.
The existing per-method keys and records remain available unchanged.
`method_record()` continues to describe the original batch method alone.

The additive `capabilities_schema` is
`ras-commander-fast-map-capabilities/1`. Its `capabilities` array contains one
entry for each advertised top-level method record:

| Field | Meaning |
| --- | --- |
| `key` | Existing registry key for the method |
| `method_id` | Method identity; copies `method_version` when the original record uses that field |
| `method_version` | Original identity field, also retained when present |
| `request_schemas` | Accepted request schema strings for this method |
| `cli_command` | CLI command that dispatches these requests |
| `api_contract` | `stable`: published versioned transport semantics are preserved |
| `intended_use` | `mapping`, `terrain`, `reduction` or `diagnostic` |
| `flags` | Complete original per-method record, including capabilities and limitations |
| `numerical_qualification` | Separate qualification summary described below |

The original records are authoritative for product, option, resource and terrain
limitations. Consumers must check them for the requested operation. Advertising
a method does not certify arbitrary combinations or create a model preparation
API.

## API stability and numerical qualification

`api_contract="stable"` means the request schemas, method identities and
documented transport semantics are preserved. It does not mean that every model
is supported or that outputs are identical to RAS Mapper. Numerical changes
require deliberate method/schema versioning; existing schemas are not silently
redirected to a different algorithm.

## Stored tile grid opt-in

The `sloping_native_grid`, `one_d_clipped_native_grid_diagnostic`, and
`coupled_native_grid_diagnostic` records advertise the exact
`grid_contracts=["rasmapper-stored-tile-grid/1"]` list. They dispatch sloping
schema `/2`, XS schema `/4`, and coupled schema `/3`, respectively. Sloping
selects `rasmapper-plain-sloping/2`; coverage composition selects
`one-d-two-d-native-grid-coverage-first-diagnostic/1`. XS retains its `/5`
numerical method while the explicit transport carries derived delivery framing.
All three records declare `native_parity_qualified=false`.

The descriptor separately binds source dimensions, original source transform,
256 tile size, and derived full output transform. Engines recompute the rounded
extent and padded logical tile geometry before payload reads. A window clips
global integer addresses without shrinking logical edge tiles or resetting
square interpolation seeds. Existing schemas exclude this descriptor.

This scope requires one original north-up square source whose logical centers
equal the derived full delivery lattice bit for bit. Admission checks every
actual row and column within bounded tile work, without full-raster center
arrays. Terrain scope, storage, velocity and face rendering are unsupported.
Source identity cannot be replaced with output georeferencing. Composition
requires identical descriptors, window framing and CRS for both components.

The fixed layout admission limit is 32,768 logical tiles with source dimensions
in `1..=i32::MAX`. This bounds constant-scratch center verification; allocation
budgets still govern terrain, geometry, caches and product buffers.

Rust callers constructing request structs explicitly supply `grid_contract`:
use `None` for source-grid requests. `coupled_session::Request` also has an
optional `window`, and each `coupled_session::Component` carries its own
optional descriptor. `sloping::RasterWindow` has `native_grid`; use
`RasterWindow::new()` for source-grid rendering and `with_stored_grid()` for an
explicit validated layout. The latter derives the selected window transform.
These public fields affect struct-literal source compatibility. Serialized
legacy requests retain their existing defaults and numerical contracts.

`numerical_qualification` contains `status`, `native_version`, `scope` and
`flags`. The summary is derived conservatively from existing explicit fields:

- `unqualified`: the original record explicitly has
  `native_parity_qualified=false`.
- `scoped`: the original record has `native_version_qualified` or
  `qualification_scope`, and is not explicitly unqualified.
- `not_declared`: neither of those declarations exists. This is neither a
  failure verdict nor evidence of parity; read the retained original details.

`native_version` comes only from `native_version_qualified`; `scope` retains
`qualification_scope`, or the original `scope` when present. Missing values are
null. Qualification flags retain existing declarations and known differences
without inferring a status from prose. For example, an effective-terrain record
with a `qualified` comparison table remains `not_declared` in the summary, and
its documented stitch differences remain visible in the flags and original
record. Boundary output retains `native_vector_parity=false`.

Existing `experimental` fields remain unchanged for compatibility. They are not
a complete lifecycle or qualification model. Use the separate contract,
intended-use and qualification fields, together with the original limitations.
Step05 follows its named upstream reference algorithm; API stability does not
turn that distinct reference into RAS Mapper stored-map parity.

## Diagnostic 1D and mixed transports

| Request schema | CLI command | Method ID |
| --- | --- | --- |
| `fast-map-storage/1` | `storage` | `storage-area-level-pool/1` |
| `fast-map-one-d-diagnostic/1` | `one-d-diagnostic` | `one-d-xs-surface-diagnostic/3` |
| `fast-map-one-d-diagnostic/2` | `one-d-diagnostic` | `one-d-xs-surface-diagnostic/4` |
| `fast-map-one-d-diagnostic/3` | `one-d-diagnostic` | `one-d-xs-surface-diagnostic/5` |
| `fast-map-coupled-diagnostic/1` | `coupled-diagnostic` | `one-d-two-d-coverage-first-diagnostic/1` |
| `fast-map-coupled-diagnostic/2` | `coupled-diagnostic` | `one-d-two-d-storage-coverage-first-diagnostic/1` |

These published transport contracts are frozen and remain diagnostic and
numerically unqualified. The older 1D method retains its distinct ownership and
interpolation; it is not an alias for the native-layout or clipped method.
Ordinary Commander production preparation for 1D/mixed models remains unsupported.
Callers must not treat raw stored TIN M as native-normalized M or infer barrier
selection from the transport alone.

Before promoting the 1D path, qualification must verify native obstruction
selection and normalized M, explicit degenerate-triangle policy, CRS identity,
and complete Depth/WSE masks and valid Float32 bits on the relevant real-model
corpus. The White Oak, Lawson and South Shaver preparation comparisons remain
required gates; discovery metadata does not change their results. Mixed-model
preparation and ownership require their own qualification. No new qualification
or numerical behavior follows from this discovery contract.

The `storage` registry entry uses the existing level-pool kernel through one
standalone prepared-buffer transport. It requires no 2D cells. Its exported
physical coverage labels include dry and terrain-nodata pixels, allowing coupled
schema `/2` to apply storage precedence before visibility. Standalone transport
native qualification is pending; the kernel's historical comparisons do not
automatically qualify new prepared inputs or storage/XS composition.

Sloping schema `/1` accepts optional `export_physical_ownership` (default
`false`), advertised by the same flag in its method record. It writes one
little-endian UInt32 `owners.bin` from the product's physical polygon burn:
zero is unowned; other values are positions plus one in `owner_cells`.
Dry and terrain-nodata pixels retain physical coverage. The report's
`physical_ownership` block identifies kind, dtype, count, path and grid.
The initial export refuses AOI, terrain-scope and embedded-storage combinations
before payload reads. Cached and ordinary execution share this contract.
Legacy requests with the flag absent or false keep their product behavior.
