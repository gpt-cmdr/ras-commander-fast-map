# Modification rules (T14)

References: `RasTerrain.export_rasmapper_terrain(downsample_factor=1,
rasterize_modifications=True)`, HEC-RAS 7.0.1, on project copies. Per-layer
references were made from an HDF copy reduced to one layer, and from `extent=`
exports (2-45 s against 160-280 s for a full export). Extent exports are not
base-consistent with full exports (nodata flips near the stitch TIN), so they were
compared only with extent base exports.

Notation: Hi = SetIfHigher, Lo = SetIfLower, Set = SetValue. A ground line is
`Type=Levee` with `Subtype=Levee|Channel`; RAS Mapper ignores any other Type.

## General behaviour

- **The `.rasmap` `Checked` state is ignored by the export.** Every layer in the
  terrain HDF is applied (corpus A with layers unchecked equals all checked).
  Callers must therefore treat any `/Modifications` member as a modification.
- **Order: descending Priority value.** Priority 0 is applied last and wins
  (synthetic W4: a Lo channel at Priority 0 cuts a 700 ft Set pad at Priority 1 on
  all 1,894 overlap pixels; ascending order is 2.7x worse on B2).
- **Nodata terrain inside a modification takes the modification value** (a channel
  entirely over nodata matched on all 20,843 px).
- **Rounding:** binary32, then 1/32 ties to even, as for the terrain.

## Per type

| Type | Rule | Qualification | Status |
|---|---|---|---|
| Ground line, Hi / Lo (levee, channel) | Triangulated template. Centerline split at every profile station (stations within 1e-3 ft of a vertex are not split); z linear by station, clamped beyond the profile. Crest edge at Top Width/2, level with the centerline; toe at **Max Reach/2** (Max Reach is the total width), `(MaxReach/2 - TopWidth/2)/slope` below (levee) or above (channel). Slopes are **H:V**, Left/Right relative to digitizing direction. Convex joints **beveled** (fan around the vertex), concave joints **mitered**, ends **square**. Each quad split on its **Delaunay** diagonal. A pixel centre takes the linear value of its triangle; overlaps within one feature take max (levee) / min (channel). | A: 4,836 modified px, 0 diff. B1/B3/B4: 0 diff except 3 px at 1/32 on B1. Several C layers: 0 diff; one 3 px at 1/32. **B2: 433 px** (below) | supported |
| Ground line, Set / Add | Not derived. One Add wall is close (162 px at 1/32 of 401). | — | refused |
| Ground lines sharing a `System Name` | One profile shared across the system; at a junction the next feature's start elevation wins (fixed half of one canal, 20,094 -> 10,365 px). Geometry with Transition Percent = 1 not derived. | — | refused |
| Ground line with Max Reach <= Top Width | Crest clipped by the reach in a way not derived (up to 8 ft; 22 px of 4,045 on another). | — | refused |
| Polygon, constant (Generate Boundary Elevations = 0) | Pixel centre inside (even-odd), constant `Elevation Value`; a stored profile is **ignored**. | B: 108 px and 722 px; C: 10,759 / 17,828 / 6,626 px: all 0 diff | supported (Set/Hi/Lo) |
| Polygon with boundary elevations (gen = 1), control points, shape Z | Not derived. | — | refused |
| Ellipse (Circle), rotation 0 | Pixel centre strictly inside `(dx/a)^2 + (dy/b)^2 < 1`; stored values are semi-axes. | A: 86 px, 0 diff | supported; rotated refused |

### How the ground-line rule was found

- T07's nearest-segment trapezoid had slopes as V:H; they are H:V.
- Flat caps matched corpus A's channels at once.
- Bevel joints fixed A's high-ground joints (82 -> 2 px).
- The triangulated template with Delaunay quads fixed B channel z-distortion near
  joints (20 -> 0 px) and A's remaining ties.
- Rejected: an unconstrained global Delaunay (53,029 px off on synthetic W3), and
  applying modifications per source before the mosaic (worse).

## Open discrepancies (measured, not explained)

1. **B2: 433 px off** (96 above 1/32 ft, max 0.84 ft) of 9,475 modified px, around a
   5-vertex levee (19-25 ft first segments against a 50 ft half reach) where a channel
   crosses it. RAS Mapper's levee surface there is 0.05-0.1 ft below the template.
   Profile scaling and extrapolation do not fix it; clamping is best.
2. **Isolated 1/32 ft roundings at exact ties** (3 px B1, 3 px on one C layer). Our raw
   value is within 0.002/32 ft of a tie and RAS Mapper rounds the other way. Stable
   across repeat exports, so a numeric difference, not RAS Mapper noise. Float32
   variants of the TIN evaluation did not fix them.
3. **Writer-authored 22 km channel on synthetic T2 (20 ft): 1,502 px off**, worst about
   1 ft at the first joint, elsewhere 1/32-0.2 ft along the toe edges. Transition
   Percent is not the cause.
