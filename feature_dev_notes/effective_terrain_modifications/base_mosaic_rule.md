# Base effective terrain (the #19 mosaic rule)

The base rule rebuilds `RasTerrain.export_rasmapper_terrain(downsample_factor=1,
rasterize_modifications=False)` from the terrain's source TIFFs, the level-0 tile
`Mask` of each source and the `Stitch TIN` in the terrain HDF.

## Rule

1. **Stitch TIN first.** Linear interpolation in the containing triangle. Weights
   must be >= -1e-9; later triangles win.
2. **Sources in priority order.** A source answers if all four pixel centres around
   the point (its "quad") are valid. Once a higher-priority source has a valid pixel
   *containing* the point, the source also needs at least one quad centre inside its
   footprint (mask bit 0). Simpler variants (masks ignored, masks required on all
   corners, ownership by the containing cell) each failed on at least one terrain.
3. **Value.** The mean of the two diagonal triangulations of the quad (equivalently
   four triangles around the quad centre). Not bilinear: bilinear reproduces 0.8% of
   values exactly, 73% after rounding. Fractional offsets are binary32, arithmetic
   binary64 (binary64 offsets put 412 Bald Eagle values one 1/32 step off).
4. **Rounding.** Binary32, then 1/32 with ties to even (.NET `Math.Round`).
5. **Lattice.** The finest source (ties by priority) over the union of source
   extents, snapped outward (ras-commander's export lattice).

Priority comes from the `Priority` attribute, or for older terrains with none the
terrain VRT order, highest first.

## Qualification (T07, HEC-RAS 7.0.1)

| Terrain | Valid px | Mask diff | Value diff |
|---|---:|---:|---:|
| Bald Eagle Terrain50 (20 ft dtm over 36.5 ft dem) | 197,179,058 | 0 | 8 (stitch-TIN corners, max 1.53 ft) |
| T1 coarse > fine (synthetic) | 767,502 | 0 | 0 |
| T2 fine > coarse | 767,879 | 0 | 0 |
| T3 holed coarse > fine | 687,908 | 0 | 5 (stitch-TIN corner, max 2.16 ft) |
| T4 30 ft offset > 20 ft > 36.5 ft | 767,879 | 0 | 0 |
| T5 holed coarse > 30 ft offset > 20 ft | 734,567 | 0 | 0 |

The residual stitch-TIN pixels are unstable in RAS Mapper itself on these
terrains: two exports of the same T2 terrain disagree on two TIN pixels.

## Gaps exposed by the modified corpora (T14)

Measured on the base layer of real modified terrains, reproduced with the
modifications switched off (schema 1 against `rasterize_modifications=False`):

| Corpus terrain | Stitch-TIN px differing | Base (non-TIN) differences |
|---|---|---|
| A | 748 of 38,900. RAS Mapper differs on 414 TIN px across 5 identical-input exports, but 567 of ours match none of them | 0 |
| B1 | 597 of 22,554, **stable in RAS Mapper**: a rule gap | 0 |
| B2 | 612 | 1 |
| B4 | 1,391 | **478** mosaic px at 1/32 ft |
| B3 | 3,337 | **27,289 mask px** (nodata disagreement in the mosaic) |

Leads:
- B-series stitch-TIN pixels are deterministic in RAS Mapper, so the TIN rule can be
  derived further; the "unstable in RAS Mapper" explanation does not cover them.
- B-series source cell sizes differ in the 10th digit (3.280833333 vs
  3.280833333333). The lattice/ownership rule likely treats near-equal cells
  differently from RAS Mapper. Start the B3/B4 mosaic work there.

Until these are fixed, even the unmodified versions of these terrains are not
exact, which is why the whole feature is parked rather than only the
modification part.
