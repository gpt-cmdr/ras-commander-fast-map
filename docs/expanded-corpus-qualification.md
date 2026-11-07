# Private expanded corpus qualification

Historical private evidence: artifact identities below predate public 0.13.0.
Integrated release validation is recorded separately; these observations do not
certify the new artifact or qualify untested models/options.

This campaign extends the small-model checks toward the five-model corpus in
the [Commander FIM comparison](https://rascommander.info/ras/development/fim-mapping-performance/).
Results below apply only to the specified model, products, rendering options
and grid. They do not establish universal mapping parity or a release qualification.

## Inputs and units

| Fixture | Physical 2D cells | Stored times | Selected result | Original terrain grid |
| --- | ---: | ---: | --- | --- |
| Spring River | 422,715 | 121 | p02, 100Yr | 28,916 × 26,316 |
| Spring Creek | 340,163 | 145 | p01, SPR_100yr | 24,003 × 18,643 |
| Bald Eagle dam | 87,039 plus storage | 865 | p07 | Prepared 1,192 × 819 for this storage lane |
| Davis | 2,716 | 217 | p02 | 681 × 569 |
| Bald Eagle bridges | 24,718, historical fixture count | Not newly bound | Canonical p05 computed result unavailable | Not newly bound |

River and Creek computed results and registered terrain were recovered from
their original public FEMA archives using validated byte ranges, CRC32 and
SHA256 receipts. No hydraulic solver was run. River matches the documented
plan identity; Creek is the positively identified 100-year plan of the named
model, but its exact historical documentation plan identity is not proven.
The local official example archive contains bridge inputs and terrain, but no
computed p05 result. The available mixed p15 dam model is not a substitute.

Public Commander result-unit metadata resolves US Customary units without
contradictions: depth/elevation in feet, velocity in ft/s and flow in ft³/s.
River uses EPSG:3433, NAD83 / Arkansas North; Creek uses EPSG:2278, NAD83 /
Texas South Central; Bald Eagle uses EPSG:2271, NAD83 / Pennsylvania North.
All three projected coordinate systems use US survey feet. No coordinate or
hydraulic-unit conversion was needed or applied. The vertical datum is not
established by this horizontal CRS metadata.

## Reproducible protocol

The tested private fast-map version is 0.11.0, selected by explicit executable
path and SHA256
`d45e82ad97d8c0033d9b49a1d59304ce2c85a6119bb5186cb52d7350819fc12c`.
Public Commander is 0.113.0. Native HEC-RAS is 7.0.1, with Mapper library SHA256
`8f246aae48a5833288d12568203919693cc15992d750514058467916a203fd7e`.
An independently installed newer wheel is not the tested executable.

Each native sample uses an immutable, fresh model clone without inherited
postprocessing caches. Global hydraulic geometry, original source terrain
bytes and 256-pixel tile history are retained. Reference flood masks are never
mapping or formatter inputs. Saved-time tests verify native timestamp
membership and its exact index before export, preventing a silent Max fallback.

The primary supervised-process clock includes fresh process startup/import,
per-run input hash verification and final source recheck, public HDF preparation,
stock API writes and a common Float32 GeoTIFF formatter: nodata -9999,
DEFLATE, 256-pixel blocks and average 2×/4× overviews. External staging, copying,
installation and comparison are excluded. API and formatter phases are also
recorded separately. Comparison streams raster blocks and
checks every valid/nodata mask pixel and every valid Float32 bit, including
direct overview datasets. A single qualification pair is not a timing median.

CPU affinity, thread counts and process-tree memory are recorded. Admission
requires three system CPU samples below 20% and at least 6 GiB available RAM.
Native memory policy stays enforced with a 4 GiB minimum reserve and its
default fractional reserve. An owned process tree is aborted if available
memory remains below 3 GiB for two seconds. Failed attempts are retained;
there are no automatic retries or disabled safeguards. No idle-desktop or
cold-cache claim is made.

## Verified results

| Workload | Host / logical CPUs | Native seconds | Fast seconds | Exact array checks |
| --- | --- | ---: | ---: | ---: |
| River plain sloping Max Depth/WSE | CLB01 / 4 | 461.307 | 181.300 | 6 |
| Bald Eagle arrival/percent, all 865 times and storage | CLB09 / 1 | 58.162 | 20.400 | 6 |
| Bald Eagle plain sloping Max Depth/WSE | CLB09 / 1 | 11.962 | 8.497 | 6 |
| Bald Eagle plain sloping saved-time Depth/WSE, index 300 | CLB09 / 1 | 12.827 | 8.286 | 6 |
| Davis derived Max, all 217 times, three products | CLB09 / 1 | 38.833 | 14.351 | 9 |

These are single preparation-through-delivery qualification pairs. Every listed
base and direct-overview comparison has zero mask and valid-bit differences.
Do not combine timings across hosts or interpret these observations as medians.

River delivery covers all 760,953,456 original source-grid pixels. Both base
bands contain 56,822,200 valid pixels. Fast-map's geometry-derived internal
window is [1018, 7787, 27370, 18228]; delivery independently pads onto the full
original grid. Native stock outputs already cover that full grid. The result
is plain corner sloping, distinct from the documentation's weighted-face mode.
Sampled peak live process-tree working-set sums were 3.198 GB native and
13.022 GB fast; those sums can double-count shared pages. Minimum available
RAM was 31.694 GB and 23.426 GB respectively.

River's CLB09 preflight was refused before any mapping helper launched:
the estimated worker needed 12,252 MiB plus an effective 6,127 MiB reserve,
against about 9,350 MiB available. The full-grid pair therefore ran on CLB01
under its unchanged memory policy. This is not a CLB09 large-model timing.

Bald Eagle uses the retained, registered native-prepared 100-foot grid,
including Reservoir Pool. Temporal maps have 53,055 valid base pixels; Max
Depth/WSE have 53,573; saved index 300 has 26,491. All 18 array checks were
also independently repeated on returned rasters. Original bytes and relative
terrain associations are preserved. Missing land-cover links are disclosed:
this is computed-results mapping, not solver-readiness qualification. It does
not reproduce the canonical 20-foot, two-terrain-source benchmark scope.

## Remaining coverage

The bounded campaign ended with 33 accepted base/overview comparisons across
the five paired workloads above. River's saved-time six-product attempt and
one separate Creek Depth/WSE attempt each failed the unchanged 120-second
CPU-idle admission gate before any mapping pipeline or helper launched.
Background geometry and terrain-preparation jobs were observed read-only and
left untouched. Failure receipts are retained separately from successful runs;
there were no hidden retries. Creek passed input and native-memory preflight,
but has no new mapping result. All owned mapping controllers have stopped.

River saved-time velocity and derived maps, Creek mapping, repeated large-model
timing samples and additional rendering modes remain unqualified. Ordinary
horizontal and weighted/shallow modes need explicit harness adapters and their
own native checks; the public APIs support more options than this initial
adapter. `slopingPretty` must not receive the plain-sloping-only terrain-HDF
option. Weighted/shallow velocity remains a public API refusal.

Storage velocity, derived products and duration remain unsupported. The public
1D/mixed guards remain unchanged; retained private mixed fixtures B/C diagnostic Depth/WSE
evidence is separate. The missing bridge computed result, multi-source 20-foot
storage coverage, AOI/catalog workloads and full-simulation derived reductions
on these large models remain explicit gaps. A successful row never qualifies
an untested mode or product.

Model data, rasters, native binaries and raw machine-specific receipts stay
outside this repository in the private campaign evidence workspace. Nothing
in this campaign has been published or released.

## Subsequent matched timing rerun

The [2026-11-07 rerun](performance-comparison-rerun.md) replaces single-observation timing expectations with three-warm-pair medians for River, Davis and the prepared100ft Bald Eagle cases. Creek initial exports matched, but its warm native exports varied at19basepixels while fast outputs remained stable, so exactness stopped that configuration with no median. The earlier bounded campaign remains historical evidence, not a universal repeat-parity claim.
