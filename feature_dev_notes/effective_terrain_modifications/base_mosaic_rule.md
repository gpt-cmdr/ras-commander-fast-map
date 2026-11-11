# Base effective terrain (the mosaic rule)

The base rule rebuilds `RasTerrain.export_rasmapper_terrain(downsample_factor=1,
rasterize_modifications=False)` from the terrain's source TIFFs and the level-0
tile `Mask` of each source. The stored `Stitch TIN` in the terrain HDF is not
used (the export does not use it); a request may still carry it.

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
6. **Gap fill (export TIN, `src/export_tin.rs`).** RAS Mapper does not use the
   stored stitch TIN during the export. For each 256-pixel output tile it
   triangulates the edges of valid blocks that border off blocks and rasterizes
   the result over the tile, overwriting sampled values:
   - *Segments.* Per source in priority order, the blocks the export visits for
     the tile: the block under each pixel centre, indices skipped inside one
     block tile filled in, and a two-block halo before the first and after the
     last block of each axis (stepping into the neighbouring block tile when it
     exists). Block tiles are visited tile rows -> tile columns -> block rows ->
     block columns. A valid block (bit 0) emits its edges for bits 8 (D->A),
     16 (A->B), 4 (B->C), 2 (C->D), so the data lies right of each segment.
     Corner Z is the binary32 source value (global cells, also across block-tile
     edges), X/Y from the block tile's recomputed geometry, M its cell size.
   - *Order.* Each source's segments are stable-sorted by the signed 32-bit
     `K xor fold32(bits(A.X))`, `K = 7 * 711650207 + 2506965631 mod 2^32`
     (type id 7, measured in a fresh export process), then concatenated in
     priority order. Equal hashes keep the collection order, modelled as an
     8-worker `Parallel.For` over block rows: ranges of `rows / 8`, lists ordered
     by descending offset of their first emitting block (ties: later range).
   - *TIN.* Octagonal super polygon around the segment box (widened to 10 x tol,
     short side >= 1/8 long side, union with the box x1.5), coordinates shifted
     to the lower left. Bowyer-Watson insertion of A then B of every segment
     with a persistent walk start; merge within tol = min(0.01, 0.1 x smallest
     M); circumcircle test inclusive. Every segment is then enforced as a
     constraint edge (crossed edges split with interpolated Z/M; constraint
     flags carry onto the new edge 0).
   - *Deletion.* Triangles touching the super polygon; triangles with an edge
     wider or taller than 2.0001 x the largest M; per segment, the triangle on
     its right and bounded fans around that triangle's vertices.
   - *Rasterization.* Live triangles in slot order; edge functions from the
     bottom-left covered pixel stepped by repeated binary64 addition; inclusive
     `>= 0`; `z = ((w0 z0 + w1 z1) + w2 z2) / S` to binary32; later triangles
     overwrite; writes clipped to the grid; then rounded like every value.

   Gaps of one source cell are bridged; wider gaps are not. The hash seed and
   the parallel order only decide equal-hash (same A.X) segment order, which
   affects cocircular Delaunay ties, a likely source of RAS Mapper's own
   run-to-run differences.

The tie behaviour that the earlier fitted rule could not explain (pixel centres on
source cell centres choosing different neighbours at 1e-9-cell perturbations) is
fully determined by items 1 and 2.

## Qualification (HEC-RAS 7.0.1)

Five real terrains, two independent exports each (a, b), modifications off,
whole terrain, engine as in this tree (request schema 1, stored stitch TIN in
the request but unused). Columns count pixels where the engine and export a
differ, excluding pixels where exports a and b differ:

| Terrain | Sources | Valid px | Exports a vs b | Ours nodata, export valid | Ours valid, export nodata | Values differing |
|---|---|---:|---:|---:|---:|---:|
| A | 5 (1 ft, 3 ft) | 164,348,332 | 160 | 0 | 0 | 300 |
| B1 | 2 | 254,759,886 | 0 | 1 | 2 | 0 |
| B2 | 2 | 254,759,881 | 0 | 1 | 2 | 0 |
| B3 | 1 | 254,743,274 | 0 | 0 | 0 | 0 |
| B4 | 2 (near-equal cells) | 254,746,539 | 0 | 1 | 0 | 0 |

On A, without the exclusion, the engine differs from export a on 354 values and
from export b on 406 (0 mask differences against either). All differences on A
lie in gap fills. These totals equal those of an independent Python prototype of
the same rule (untracked); in that prototype, type id 7 with canonical
(sequential) order gives 519 values on A, and type id 23 with the 8-worker order
82, so the residue is consistent with segment-order ties. Before the export TIN
the engine (stored stitch TIN) differed by A 20 mask / 690 values, B1 594 / 7,
B2 599 / 6, B3 3,227 / 0, B4 1,389 / 1.

Engine time on the qualification build host (release build, 4 threads): 5.0-5.1 s for the B
terrains and 1.2 s for A, of which the export TIN takes 0.1-0.4 s (sampling only:
4.5-4.9 s and 1.1 s); peak working set 1.7-2.2 GiB (B) and 0.75 GiB (A).

Synthetic T1-T5 (one export each, so RAS Mapper's own variation cannot be
excluded): valid mask identical on all five; values T1 identical, T2 and T4
2 px, T3 and T5 4 px (max 1.53 ft), all inside export-TIN gap fills. Under the
stored stitch TIN they were T1, T2, T4, T5 identical and T3 5 px. Two
exports of T2 were already known to disagree on two TIN pixels.
BaldEagleCrkMulti2D Terrain50 has not been re-run.

## Next

Re-run Terrain50, and repeat-export the synthetic terrains to separate RAS
Mapper's own variation from rule gaps. The residual pixels depend on
RAS Mapper's per-process hash seed and parallel scheduling, so bit equality on
them is not expected from a fixed order.
