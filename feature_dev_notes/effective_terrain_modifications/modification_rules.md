# Modification rules

References: `RasTerrain.export_rasmapper_terrain(downsample_factor=1,
rasterize_modifications=True)`, HEC-RAS 7.0.1, on project copies. Ground lines
and ellipses now follow RasMapperLib 7.0.1's export arithmetic as described by
an interoperability study (prose findings only; no vendor code used or copied).
Polygons keep the fitted T14 rule.

Notation: Hi = SetIfHigher, Lo = SetIfLower, Set = SetValue. A ground line is
`Type=Levee` with `Subtype=Levee|Channel`; RAS Mapper ignores any other Type (a
layer written as `Type=Channel` leaves the export equal to the base export).

## General behaviour

- **The `.rasmap` `Checked` state is ignored by the export.** Every layer in the
  terrain HDF is applied (corpus A with layers unchecked equals all checked), and
  an HDF layer the `.rasmap` does not list is applied too.
- **Order:** layers stable-sorted by ascending Priority and applied in reverse,
  so Priority 0 is applied last and wins (synthetic W4: a Lo channel at
  Priority 0 cuts a 700 ft Set pad at Priority 1). Features in stored (ID) order;
  each feature sees the result of the previous one.
- **Combine, binary32:** a nodata terrain pixel takes the modification value;
  Set replaces; Hi / Lo replace when strictly higher / lower. Add would be
  terrain + modification on the unrounded terrain (not reproduced).
- **Rounding:** binary32, then 1/32 ties to even. RAS Mapper rounds once when it
  writes; for Set / Hi / Lo rounding after every feature gives the same result.
- **Per output tile:** the export rasterizes modifications per 256-pixel output
  tile, after the export TIN, with that tile's recomputed pixel geometry.

## Per type

| Type | Rule | Qualification | Status |
|---|---|---|---|
| Ground line, no System Name, Hi / Lo / Set | Ribbon TIN per feature. Profile: binary32 stations shifted to start at 0, clamped to the length, first value per station; every segment longer than 100 ft (100/3.281 m) adds stations every 100 from a running start that only advances over such long segments; the length is appended when the last station is more than 0.01 short; binary32 query clamped at both ends. Offset lines: toes at Max Reach/2, ridges at Top Width/2, M = centerline station; joints meet at the offset-segment intersection (M averaged) or get the previous end, an arc (split while chord-mid/r < 0.95) and the next start; square ends; self-intersections trimmed; profile stations injected by M fraction. Centre/ridge z = profile, toe z = profile -/+ (MaxReach/2 - TopWidth/2)/slope (H:V, 0 read as 0.001). Five ribbons zipped by the shorter diagonal (not Delaunay); export rasterizer per tile, last triangle wins, clockwise triangles draw nothing. | Real A, B1-B4: no difference inside any footprint (B2 443 -> 0, B1 3 -> 0). Synthetic (blank System Name): channel 22.5 km, levee 21.2 km, combined case: 0 modified px; sharp bends/short segments/crossings/hairpin: 2 of 8,935 at 1/32 ft; short-segment levees: 0 of 2,384; Set: 0 of 5,282 | supported |
| Ground line, Max Reach <= Top Width | Same rule: the toes fold inside the ridges and their strips draw nothing; the crest remains. | Synthetic, 3 features (one with Max Reach = Top Width, one Lo): 0 of 1,325 | supported |
| Ground line, Top Width or Max Reach <= 0 | No change (RAS Mapper builds no TIN). | traced, not measured | supported |
| Ground line with any System Name | RAS Mapper's system path (also for a single feature); not reproduced. | The 22.5 km channel named `chan` differs from its blank-name export on 1,506 px | refused |
| Ground line, Add | terrain + value on the unrounded terrain. | not measured | refused |
| Ground line with control points | Profile from control points (not reproduced). | — | refused (caller) |
| Polygon, constant (Generate Boundary Elevations = 0) | Pixel centre inside (even-odd), constant `Elevation Value`; a stored profile is **ignored**. | B: 108 px and 722 px; C: 10,759 / 17,828 / 6,626 px; synthetic W1/M1 46,8xx px: all 0 diff | supported (Set/Hi/Lo) |
| Polygon with boundary elevations (gen = 1), control points, shape Z | RAS Mapper uses a constrained Delaunay TIN of the perimeter profile; not reproduced. | — | refused |
| Ellipse / circle, any rotation | Output-tile pixel centre: circle (equal radii) distance <= radius; ellipse `(c dx + s dy)^2/rx^2 + (s dx - c dy)^2/ry^2 <= 1`, c, s = cos, sin(-rotation in degrees). **Inclusive.** | A: 86 px, 0 diff; synthetic five ellipses (exact circle boundary, rotations 0, 30, -63.5, 90): 0 of 1,005 | supported |

## Real terrains (whole terrain vs export a, pixels where two exports differ excluded)

| Terrain | Before (fitted rule): mask / value / max | After: mask / value / max | Inside footprints after |
|---|---|---|---|
| A | 0 / 322 / 0.53 ft | 0 / 322 / 0.53 ft | 0 (the 39 "modified" px lie outside every footprint, where the base and modified exports' gap fills differ) |
| B1 | 2 / 3 / 1/32 ft | 2 / 0 / 0 | 0 |
| B2 | 2 / 443 / 1.47 ft (439 on modified px) | 2 / 0 / 0 | 0 |
| B3 | 2 / 0 / 0 | 2 / 0 / 0 | 0 |
| B4 | 0 / 0 / 0 | 0 / 0 / 0 | 0 |

The remaining mask pixels are not near any feature (base export-TIN pixels).

### How the ground-line rule was found

- T07-T14 fitted a triangulated template (bevel/miter joints, Delaunay quad
  diagonals, max/min overlap). It matched most corpora but left B2's
  channel/levee crossing (433-443 px) and the writer channel (1,502 px).
- The study's prose (offset lines with M, arc joints, trims, M-fraction
  injection, ribbon zipper, last-wins) was implemented from the description.
  Offset lines, M injection and the ribbon TIN were then checked against
  RasMapperLib 7.0.1 called as a black box through pythonnet on the same
  centerline: identical points and triangles; RasMapperLib's own rasterizer on
  that TIN gives the engine's pixels exactly.
- The writer-authored synthetic cases (W3, W3n, W4, M2) still differed, because
  ras-commander's writer stores `System Name = <layer name>` and RAS Mapper
  sends every named ground line through its system path. With the name blanked
  and the projects re-exported, they match.

## Open discrepancies (measured, not explained)

1. **Named ground lines** (any non-empty System Name) use RAS Mapper's system
   path (shared profile, Transition Percent, ribbon modes). Not reproduced;
   refused. Writer-authored ground lines are affected until the writer leaves
   System Name empty.
2. **Isolated 1/32 ft roundings at exact ties** (2 px of 8,935 on the sharp-bend
   synthetic case). RAS Mapper rounds the other way; a numeric difference in the
   last bits.
3. **Ground-line Add** needs the unrounded terrain value (RAS Mapper rounds only
   when it writes); the engine holds rounded values. Not measured.
