# Roadmap

## Bar for un-parking

The effective terrain may replace the HEC-RAS export for a terrain only when, on the
real corpora, whole terrains (base plus modifications) match the export with an
identical valid mask and identical values, except pixels where RAS Mapper itself
varies between identical exports (shown by repeat exports). Callers keep sending
every modified terrain to RAS Mapper until the user approves a change against this
evidence.

## Work, in order of payoff

1. **Fix the base rule on the corpora** (`base_mosaic_rule.md`). Without this, even
   unmodified versions of these terrains are not exact.
   - B-series stitch-TIN pixels (deterministic in RAS Mapper, so derivable).
   - Near-equal source cell sizes (10th digit) in the lattice/ownership rule: the
     B3 nodata mask (27,289 px) and B4 1/32 values (478 px).
   - Re-check corpus A's TIN pixels that match none of 5 RAS Mapper exports.
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
