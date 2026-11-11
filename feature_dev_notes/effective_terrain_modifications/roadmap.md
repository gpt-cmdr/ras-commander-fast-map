# Roadmap

## Bar for un-parking

The effective terrain may replace the HEC-RAS export for a terrain only when, on the
real corpora, whole terrains (base plus modifications) match the export with an
identical valid mask and identical values, except pixels where RAS Mapper itself
varies between identical exports (shown by repeat exports). Callers keep sending
every modified terrain to RAS Mapper until the user approves a change against this
evidence.

## Status (2026-11-11)

Whole terrains with all modification layers, HEC-RAS 7.0.1, two exports each,
pixels where the exports disagree excluded:

| Terrain | Layers applied | Mask diff | Value diff | Where |
|---|---|---:|---:|---|
| B4 | all | 0 | 0 | meets the bar |
| B1, B2, B3 | all | 2 | 0 | export-TIN gap-fill edges |
| A | all | 0 | 322 | export-TIN cocircular ties whose order RAS Mapper takes from a per-process hash and thread scheduling (39 on modified pixels, max 0.0625 ft) |
| C | 13 of 13 | 0 | 4 | 1/32 ft |
| D | 228 of 229 | 0 | 3 | 1/32 ft |
| E | 5 of 6 | 0 | 3 | 1/32 ft |

Done: the base rule (render-block sampling and the export TIN), control points,
boundary-elevation polygons with stored profiles, named and joined ground lines,
ground-line SetValue and Add, Max Reach <= Top Width, B2's channel/levee joint,
rotated ellipses, NaN templates and negative slopes, the HEC-RAS 6.6 differences
that were measured. Contract `/2` is advertised.

## Remaining work, in order of payoff

1. **Boundary elevations sampled from the terrain** (no stored profile and no
   perimeter control point): RAS Mapper samples each source raster and the stored
   stitch TIN. The last refused layer in D and E.
2. **Callers.** Commander's `read_layers` must send the new fields
   (`modification_rules.md`); ras2cng's policy check still sends every modified
   terrain to RAS Mapper and must be changed explicitly.
3. **Shape-Z and multi-part TIN polygons; named ground lines with control points or
   NaN templates, and under 6.6.** No corpus needs them yet.
4. **Export-TIN ties on A.** They move with RAS Mapper's hash seed, so a fixed
   order cannot reproduce every export.
5. **Memory.** Write strips instead of holding the grid.
6. **Boundaries.** Rust inundation boundaries need a single-source terrain HDF;
   emitting one from the effective terrain was not attempted.
7. **Untested:** downsample factors 2/4/8 and `extent` against the export; HEC-RAS
   7.0.2; sources in different CRSs and non-square cells are refused.

## When a type qualifies

Advertise it in `ras2fim-batch method` under the effective-terrain contract, keep
every other form refused with a reason, and record the evidence (pixels compared,
differences and where) in `modification_rules.md`. Callers decide separately
whether to use it; ras2cng's policy check must be changed explicitly.
