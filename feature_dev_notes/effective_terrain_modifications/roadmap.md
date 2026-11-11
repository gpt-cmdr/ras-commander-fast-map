# Roadmap

## Bar for un-parking

The effective terrain may replace the HEC-RAS export for a terrain only when, on the
real corpora, whole terrains (base plus modifications) match the export with an
identical valid mask and identical values, except pixels where RAS Mapper itself
varies between identical exports (shown by repeat exports). Callers keep sending
every modified terrain to RAS Mapper until the user approves a change against this
evidence.

## Work, in order of payoff

1. **Base rule on the corpora: done (2026-11-11)** (`base_mosaic_rule.md`).
   Render-block sampling plus the per-tile export TIN. Whole terrains with all
   modification layers, HEC-RAS 7.0.1, two exports each, pixels where the exports
   disagree excluded:

   | Terrain | Mask diff | Value diff | Where |
   |---|---:|---:|---|
   | B4 | 0 | 0 | meets the bar |
   | B3 | 2 | 0 | on modified pixels |
   | B1 | 2 | 3 | 1/32 ties on modified pixels (item 4) |
   | B2 | 2 | 443 | 439 on modified pixels: the channel/levee joint (item 4) |
   | A | 0 | 322 | 283 off modified pixels: export-TIN cocircular ties whose order RAS Mapper takes from a per-process hash and thread scheduling; 39 on modified pixels |

   Remaining base work: the few B mask pixels, and whether A's tie pixels can be
   matched (they move with RAS Mapper's hash seed, so a fixed order cannot
   reproduce every export).
2. **Control points and polygons with boundary elevations** (corpora C, D, E): the
   largest refused classes by layer count.
3. **Ground-line `System Name` joins, Set/Add ground lines, Max Reach <= Top Width**
   (C, G, H). One Add wall is already close (162 px at 1/32).
4. **B2's short-segment joint where a channel crosses a levee** (433 px), and the
   near-tie 1/32 ft roundings.
5. **Rotated ellipses**, features without a profile, shared priorities (refused
   today; rare).
6. **Memory.** Write strips instead of holding the grid (about 5x the export's
   memory today).
7. **Boundaries.** Rust inundation boundaries need a single-source terrain HDF;
   emitting a matching single-source terrain HDF from the effective terrain was not
   attempted.
8. **Untested:** downsample factors 2/4/8 and `extent` against the export; sources in
   different CRSs and non-square cells are refused.

## When a type qualifies

Advertise it in `ras2fim-batch method` under the effective-terrain contract, keep
every other form refused with a reason, and record the evidence (pixels compared,
differences and where) in `modification_rules.md`. Callers decide separately
whether to use it; ras2cng's policy check must be changed explicitly.
