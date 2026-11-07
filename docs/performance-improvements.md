# Private performance implementation and evaluation

Historical private evidence: artifact identities below predate public 0.13.0.
Integrated release validation is recorded separately; these observations do not
certify the new artifact or qualify untested models/options.

This work implements six improvements recommended by the independent Fable
review. It is private qualification work on version 0.11.0, not a released
performance guarantee. Existing method IDs, scalar binary32 expressions,
source coordinates and native tile/donor history remain unchanged.

| Improvement | Implementation | Evaluation |
| --- | --- | --- |
| Instrument missing phases | Scope, storage ground, finalize, velocity, derived, stream and native row-write timers | Release library tests pass; prepared-fixture receipts record phase timings |
| Output delivery | Bounded 256 KiB native strip target and incremental row writer | Fair public writer pair passes six base/overview checks; wider-strip timing inconclusive |
| Align source block rows | First partial stripe, then source-block-aligned stripes | Synthetic redundant cache hits fall from 112 to 0; masks/bits match |
| Reuse terrain validity | Cache validity separately from numeric nodata; scope consumes mask plus cached validity | Scope counts/refusals match; combined redundant hits fall from 240 to 0 |
| Velocity allocation and passes | Borrow donation scratch and use the existing request pool for independent pixel operations | 22 prepared native array checks pass; short four-thread microbenchmarks improve |
| Row streaming | Opt-in native TIFF row rendering with the original full-window affine arithmetic | Eight native TIFF checks and synthetic comparisons pass; 256 rows reduce sampled peak about 30%, with higher wall time |

The integrated all-target suite passed. The final release library suite has
104 passing tests and five opt-in benchmarks ignored. Strict Clippy passes
with an invocation-only allowance for the new Rust 1.99
`chunks_exact_to_as_chunks` lint on pre-existing code; all other warnings are
errors. Installed-wheel smoke passes for native selection/compression, grouped
maxima/spill/buffers, plain/enhanced options, geometry, streams/AOIs and the
existing diagnostic paths. Dependencies were not changed.

## Contracts and limits

`row_stream_rows` is an optional native-output wire extension. Zero keeps the
ordinary path; positive values up to 4096 select bounded output-row buffers.
It requires native GeoTIFF output and plain corner sloping, and currently
refuses velocity, storage, derived Max reduction and grouped consumers. It
uses one in-flight profile. Full ownership, ground and prepared hydraulics
remain resident, so this is bounded output memory rather than a fully streamed
geometry/terrain mapper. The ordinary array-stream protocol is unchanged.

The scope guard still checks original level-zero mask bytes and source
validity. A separate validity bitmap prevents a valid elevation equal to the
renderer sentinel from being treated as missing. Source-count reporting and
foreign-pixel/storage refusals remain part of acceptance. No single-source
shortcut bypasses these semantics.

Native LZW TIFFs are not equivalent to the benchmark's final full-grid,
DEFLATE-256, nodata -9999 rasters with average overviews. Any delivery timing
comparison must include the same final formatting for both routes. Redundant
formatting is not declared eliminated merely by selecting the native writer.

Performance results require their fixture, CPU/in-flight configuration, cache
state, clock boundary and complete mask/Float32-bit checks. Microbenchmarks
and synthetic memory results do not establish a Spring River speedup. Raw
models, executables, rasters and machine-specific receipts remain outside the
repository in the private implementation evidence workspace.

## Measured observations

The frozen candidate executable SHA256 is
`8f469baa8fda0c549683f3bbef8180a0a553315e08505099da1fcfbbcce22a1f`.
This is the build used for timing and synthetic observations below. Repacking
after documentation changes produced final wheel executable SHA256
`6f4260833dbe4f6a9e8786ee0d6b79cfd58db5e9046b6c61e7f06ed730fd1227`.
Its method records match, and the exact final executable separately passes
all 30 native comparisons and installed-wheel smoke. Do not silently assign
the earlier build's timing observations to the final executable.
On CLB09, seven prepared fixture cases pass 22 complete native-array checks:
Depth/WSE, velocity Max, saved-time six products, all-217 derived Max, a source
crop, Davis temporal and Bald Eagle storage-inclusive temporal. Native TIFF
rows 0/1/7/256 add eight exact Davis Depth/WSE comparisons. Release unit tests
pass 104 with five opt-in benchmarks ignored. The installed wheel contains
the same executable hash.

Six counterbalanced short kernel trials on admitted CLB09, with a shared
four-thread pool, observed median 8.002 ms scalar versus 5.804 ms pooled for
three derived products over two million pixels. A general five-corner donation
test over 250,000 evaluations observed 26.065 ms with copied weights versus
16.557 ms with borrowed weights. Equality checks remain active. These are
kernel observations, not complete-model speedups; one-thread and small-array
performance remains unmeasured.

Three wide LZW writer pairs observed overlapping timing ranges; their medians
were 0.2509 s with legacy strips and 0.2552 s with the larger strips. No speed
gain is established. The bounded incremental API is needed for row streaming;
its value must not be conflated with that inconclusive strip-timing result.

A separate one-CPU public-API qualification pair used the same final 681 by
569 grid, DEFLATE-256, nodata -9999 and direct average overviews for both
writers. All six base/overview checks pass. Single supervised observations
were 2.799 s rasterio and 2.629 s native, with sampled tree peaks 187.77 MB
and 188.56 MB. No established speed or memory gain is claimed. The common
formatter remained included. An earlier CLB01 attempt was refused before
mapping because background CPU stayed near 100%; it is retained separately.

## Larger synthetic checks and memory tradeoff

A corrected 8,359,948-pixel, one-cell synthetic source uses a fractional
US-survey-foot origin, a non-aligned window, nodata holes and a terrain-scope
mask. Twelve full-array comparisons pass, including complete masks, valid
Float32 bits, affine/CRS and scope records. The initial generator omitted one
vertex incidence pair and failed the old baseline's binary-length check;
the bad fixture, audit and separately authorized corrected run are retained.
No production-engine failure was established. A separate staging-order error
occurred before mapping and was also preserved.

Without scope, legacy versus aligned terrain preparation observed 112 versus
zero cache hits, with 128 tile reads in both. With scope, legacy versus cached
validity observed 240 versus zero hits, again 128 reads in both. All tiles fit
the cache: these observations demonstrate removal of redundant requests, not
a reduction in decodes or a River speedup. The new scope timer was about
0.061 s. Single full-process observations were 0.362 s old scope and 0.353 s
new scope. Cached validity increases retained window memory; it does not
establish a memory improvement by itself.

Within the same new candidate, native full-array output peaked at 196.2 MB
sampled live-tree working set. Streaming 256 rows peaked at 137.8 MB, about
30% lower, with single wall observations 0.353 s versus 0.382 s. One-row and
seven-row modes peaked near 131 MB but took 1.540 s and 0.505 s. Keep streaming
opt-in: fixed overhead can also increase RSS on tiny grids. These observations
do not establish a universal optimal stripe size or a large-model speedup.

The initial improvement experiments did not run full River/Creek end-to-end comparisons. The subsequent [matched rerun](performance-comparison-rerun.md) qualifies River plain Max Depth/WSE with three warm pairs. Creek initially matched but native-repeat export variation stopped its warm comparison; no Creek median is qualified.
Large velocity coverage, one-core/small-array timing thresholds, cold-cache
experiments and a final-format single-pass writer remain separate follow-up
work. No approximation, guard override, dependency change or release was used
to obtain the reported results.
