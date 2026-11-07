# Private matched RASMapper rerun, 2026-11-07

Historical private evidence: artifact identities below predate public 0.13.0.
Integrated release validation is recorded separately; these observations do not
certify the new artifact or qualify untested models/options.

Six configurations completed an initial pair and three warm alternating pairs, with complete native masks and valid Float32 bits equal, including direct 2x/4x overviews. Creek stopped at an exactness failure and has no qualified timing median. These are stored-map export workloads, not general speed guarantees.

## Three-warm-sample medians

| Workload | Host | Logical CPUs / fast inflight | RASMapper seconds | Fast-map seconds | Native / fast |
| --- | --- | ---: | ---: | ---: | ---: |
| Spring River: Max Depth/WSE | CLB01 | 4 / 1 | 324.664 | 138.653 | 2.342 |
| Bald Eagle: Max Depth/WSE | CLB09 | 1 / 1 | 12.361 | 8.245 | 1.499 |
| Bald Eagle: saved 300 Depth/WSE | CLB09 | 1 / 1 | 11.480 | 8.272 | 1.388 |
| Bald Eagle: arrival/percent, all 865 times + storage | CLB09 | 1 / 1 | 57.401 | 19.815 | 2.897 |
| Davis: derived Max, all 217 times | CLB09 | 1 / 1 | 38.250 | 14.740 | 2.595 |
| Davis: derived Max, all 217 times | CLB09 | 4 / 4 | 23.218 | 10.061 | 2.308 |

The six qualified configurations account for 168 exact base/overview comparisons. Creek initial exports added six exact comparisons before its warm pair failed. In total 52 child pipelines returned successfully, 25 pairs were accepted and one pair failed comparison. Remaining Creek repetitions were not launched; no retries or guard overrides were used.

## Identity, output and clock

Private fast-map0.11.0 executable SHA256 `6f4260833dbe4f6a9e8786ee0d6b79cfd58db5e9046b6c61e7f06ed730fd1227`, Commander0.113.0, native HEC-RAS7.0.1. Mapper assembly SHA256 `8f246aae48a5833288d12568203919693cc15992d750514058467916a203fd7e`. The explicit engine path overrides the independently installed package binary.

The clock includes fresh-process startup/import, per-run input hashes and final rechecks, public HDF preparation, stock API writes and identical final full-grid Float32 DEFLATE-256 output, nodata -9999, and average overview levels2/4. External staging, model cloning, installation and array comparison are excluded. Native models are fresh clones without inherited PostProcessing caches. Initial pairs are excluded from medians. Phase clocks may overlap and must not be added as a disjoint breakdown.

All runs retain the default rasterio product writer with row streaming OFF, matching the earlier comparison contract. The opt-in native row-streaming memory experiment is separate. Affinity, thread/inflight budgets, one native helper, GDAL/BLAS threads1 and enforced memory admission remain fixed. Admission requires three CPU samples below20% and at least6GiB available; native reserve is at least4GiB plus its default fractional policy. The owned-pipeline abort below3GiB for two seconds remains active.

River uses all760,953,456 original source-grid pixels, with56,822,200 valid pixels per base Depth/WSE band. Creek uses447,487,929 source pixels and107,934,719 valid pixels per base band in its initial pair. Native full-grid export and fast geometry-derived internal cropping both deliver the same independently declared full grid; reference flood masks are never mapping inputs.

Davis uses its original681x569 source and verified public CRS pin. Bald Eagle uses the registered native-prepared100-foot1192x819 grid, including storage; it does not represent the canonical20-foot multi-source benchmark scope. Source hydraulic units are English and projected coordinates use US survey feet. No conversion, reprojection, solver run or vertical-datum inference was performed.

Each host/configuration remains separate. No cold-cache or idle-machine claim is made. Historical runs had different load/cache conditions and do not establish a controlled old-versus-new executable speedup. Working-set sums may double-count shared pages; fast-map memory is not universally lower.

## Creek repeat-variation failure

Creek initial Depth/WSE maps matched in all six base/overview checks. During its first warm pair, masks still matched but19 base depth pixels differed in valid Float32 bits. The comparison gate stopped the remaining samples. Native and fast child processes had both returned successfully, so the process-only failure ledger was insufficient; the explicit comparison-failure record is retained alongside unchanged original measurements.

Read-only comparisons show the fast initial/warm rasters match at every base and overview pixel. Native initial/warm exports vary at the same19 base Depth and WSE locations, with additional propagated overview differences. Direct checks at all19 locations confirm the differences already exist in raw native stock exports; the common formatter preserves each raw value exactly. Grid, float32 datatype, CRS and affine metadata match.

Maximum WSE difference is one ULP; maximum absolute difference is3.0517578125e-05feet. Small depth values differ by up to4096ULP after subtraction, so the depth discrepancy must not be described as one ULP. Source hashes, runtime pins and declared controls remained unchanged. This is observed native-export repeat variation upstream of formatting, with cause unresolved; intrinsic nondeterminism is not established. No Creek median or repeatable-bit-parity claim is made.

## Durable evidence

The private campaign `rasmapper-rerun-20261107` retains complete inputs/manifests, supervisor and pipeline receipts, stock and normalized rasters, source checks, raw-point diagnostics, failure ledger, CSV and combined report. Parent verification recomputes medians only for complete equality-checked pairs. Artifact hashes:

- Small archive: `7bb839c35b393546ffee0790f14df5285e9bd6b7d48655f87762c3267057b197`.
- Large archive, including rejected Creek outputs: `5bf4dce1ca208b8f025d3d590be60eedeeaed9749692ef86eea1b22545917afd`.

All owned processes stopped. Model bytes, source code, dependencies and services were not changed by the rerun. Documentation and a session checkpoint were added; nothing was committed or published. Large velocity/weighted/mixed-model qualification and Creek native-repeatability investigation remain separate work.
