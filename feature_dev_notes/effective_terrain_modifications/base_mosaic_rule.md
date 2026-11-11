# Base effective terrain (the mosaic rule)

The base rule rebuilds `RasTerrain.export_rasmapper_terrain(downsample_factor=1,
rasterize_modifications=False)` from the terrain's source TIFFs, the level-0 tile
`Mask` of each source and (for now) the stored `Stitch TIN` in the terrain HDF.

## Rule (render-block sampling, 2026-11-11)

Established from an interoperability study of RasMapperLib 7.0.1 (prose findings
only, kept outside this repository; no vendor code used or copied) and checked
pixel by pixel against exports.

1. **Output lattice.** The export is written in 256 x 256 tiles. Tile pitch is
   recomputed as max(tile-grid width / tiles wide, height / tiles tall), and each
   tile's pixel size as max(tile width, tile height) / 256. Pixel centres are
   `tile_min + pixel * (i + 0.5)` per tile. The cell size is the finest source's
   extent-derived size (max of extent width / columns, height / rows), which can
   differ from its GeoTIFF cell size in the last bits (3.280844444000001 vs
   3.28084444399999 on B3).
2. **Render blocks.** A source is sampled by render block, the quad between four
   adjacent cell centres. Blocks are grouped in 256 x 256 tiles whose grid starts
   half a cell inside the source extent; block-tile pitch and block size are
   recomputed from tile extents the same way. The block index is the floor of the
   offset over that size; the fraction is cast to binary32.
3. **Validity.** A block answers only when bit 0 of its mask byte is set. The mask
   has one byte per render block: bit 0 = all four corners valid and the block is
   owned by this source; bits 4/3/2/1 (16/8/4/2) = neighbour above/left/right/below
   is off. Terrain creation clears lower-priority blocks under higher-priority
   valid blocks (grown by a quarter cell). Sources are tried in priority order;
   the first to answer wins. There is no fallback to fewer corners.
4. **Value.** Four triangles on both diagonals of the block. Binary64 arithmetic
   on the binary32 corners and fractions, with the block-centre term
   ((a + b + c + d) / 2) held in binary32. Pure binary32 arithmetic leaves
   thousands of one-step roundings; binary64 throughout leaves 478 on B4.
5. **Rounding.** Binary32, then 1/32 with ties to even.
6. **Gap fill (not reproduced yet).** RAS Mapper does not use the stored stitch
   TIN during the export. For each 256-pixel output tile it collects the edges of
   valid blocks that border off blocks (mask bits 1-4), within a two-block halo,
   triangulates them as constrained Delaunay edges (points merged within
   min(0.01, 0.1 x cell)), deletes triangles on the data side of each edge and any
   triangle wider or taller than 2.0001 x the largest cell, and rasterizes the rest
   over the tile (inclusive barycentric test, value = sum(w z) / sum(w)),
   overwriting source values. Gaps of one cell are bridged; wider gaps are not.
   The segment order depends on hash values and parallel completion order, which
   is a likely source of RAS Mapper's own run-to-run differences.

The tie behaviour that the earlier fitted rule could not explain (pixel centres on
source cell centres choosing different neighbours at 1e-9-cell perturbations) is
fully determined by items 1 and 2.

## Qualification (HEC-RAS 7.0.1)

Five real terrains, two independent exports each, modifications off. "No TIN"
runs the engine with the stored stitch TIN disabled, isolating rules 1-5:

| Terrain | Sources | Valid px | No TIN: values differing | No TIN: valid where export is nodata | No TIN: export valid, ours nodata (gap fill) |
|---|---|---:|---:|---:|---:|
| A | 5 (1 ft, 3 ft) | 164,348,332 | 0 | 0 | 24,952 |
| B1 | 2 | 254,759,886 | 0 | 0 | 13,040 |
| B2 | 2 | 254,759,881 | 0 | 0 | 12,957 |
| B3 | 1 | 254,743,274 | 0 | 0 | 2,762 |
| B4 | 2 (near-equal cells) | 254,746,539 | 0 | 0 | 17,059 |

With the stored stitch TIN enabled (the engine's current behaviour), whole-terrain
differences against the export are: A 20 mask / 690 values; B1 594 / 7; B2 599 / 6;
B3 3,227 / 0; B4 1,389 / 1. All of them lie inside stored-TIN triangles. Before
this rule they were A 20 / 777, B3 30,628 / 0 and B4 1,389 / 479.

The synthetic T1-T5 terrains are unchanged (T1, T2, T4, T5 identical; T3 5
stitch-TIN pixels). BaldEagleCrkMulti2D Terrain50 has not been re-run under this
rule.

RAS Mapper's own repeated exports differ only on A (160 px base, 90 px with
modifications); the B terrains reproduce exactly.

## Next

Reproduce the per-tile export TIN (item 6), then re-qualify with the stored stitch
TIN removed from the request path.
