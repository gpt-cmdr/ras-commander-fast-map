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
| Ground line, NaN template or negative slope | A NaN Max Reach draws nothing (also with every template value NaN); a NaN slope is read as 3; a negative slope is used as is, so that toe lies on the other side of the crest. A NaN Top Width with a finite Max Reach makes the RAS Mapper export fail. | Synthetic: all-NaN feature skipped and the next feature in its layer drawn; NaN slopes 0 of 625; NaN Max Reach 0 px drawn; negative-slope channel 0 of 910 px | supported; NaN Top Width with finite Max Reach refused |
| Ground line with control points (unnamed) | The layer's points within the feature's Elev Pt Tolerance of the centerline set the profile at the station of their nearest centerline point (full search, first segment at the smallest distance), after the stored stations so they win; with no stored profile the ends take the elevation of the first and last such point (by station). Densification as usual. | Synthetic: stored profile + points, points only, beyond-tolerance point: 1 of 2,499 px at 1/32 ft; real E: a 14-feature layer with 2,862 points, 3 px at 1/32 ft | supported |
| Ground line with a System Name (also a lone feature), Hi / Lo / Set | RAS Mapper's system path. **Joining:** in feature-ID order an unassigned named feature starts a system; same-name features with equal elevation type and Elev Pt Tolerance whose end is within 60 ft of the system's first point are prepended, those whose start is within 60 ft of its last point appended (nearest first). Centerline = members' vertices concatenated. **Profile:** member profiles concatenated with stations offset by the running last station; each member's first point becomes a control cross section (template of that member; Max Reach <= 0 read as 2 x Top Width) entered before the stored stations, so at a junction the next feature's start elevation wins and the previous feature's end elevation is dropped; densification every 100 ft (30.48 m SI); a closing cross section at the system end. **Sections:** cross-section lines perpendicular to the chord half a reach up and down the centerline carry the trapezoid (Top Width = Max Reach narrowed by 0.01); a changing template with Transition Percent > 0 ends its section Top Width x Transition Percent early and that station enters the centre profile (NaN: nothing); toe and ridge stream lines grown from the centerline to the cross-section points in 4-16 lateral steps (resample, smooth cubic blend, Douglas-Peucker, crossing trims); template values interpolated by station for the toe z. Same ribbon TIN and rasterizer; drawn once at the highest member feature ID. | Writer cases W3, W3n, W4, M2: 0 modified px differ (were 1,505-1,507 and 159 px, max 1,032 and 77 ft, under the unnamed rule). Synthetic systems: 3 joined channels with a 20 ft gap, Transition Percent 0 / 0.5 and a lone NaN-transition channel: 1 of 7,118 at 1/32 ft; levee systems split by elevation type, Transition Percent 1, a profile starting at station 10, SetValue members: 0 of 2,494; SetValue channel system in scrambled feature order: 0 of 4,567. Real C: a 3-feature system and two lone named features in two layers, 0 differences | supported |
| Ground line, Add | Terrain plus the template surface in binary32 on the unrounded terrain (the mosaic and export TIN now hold binary32 values; rounding to 1/32 happens once after every modification, as the TIFF writer does). | Synthetic Add levee (in G1): exact; real C: the Add levee layer, no difference | supported |
| Named ground line with control points or a NaN template | System path with control points not traced. | — | refused |
| Polygon, constant (Generate Boundary Elevations = 0) | Pixel centre inside (even-odd), constant `Elevation Value`; a stored profile is **ignored**. | B: 108 px and 722 px; C: 10,759 / 17,828 / 6,626 px; synthetic W1/M1 46,8xx px: all 0 diff | supported (Set/Hi/Lo) |
| Polygon with a control point inside, or with boundary elevations (gen = 1) from perimeter control points or a stored profile, Set / Hi / Lo / Add | Perimeter TIN. Profile: with gen = 1, the layer's points within Elev Pt Tolerance of the perimeter (at their nearest perimeter station, first per station); without such points the stored perimeter profile; with gen = 0 the constant Elevation Value. Douglas-Peucker filter at 0.01 ft (binary32). Perimeter points: the ring with the profile's stations injected and z from the profile; points before the first or after the last station interpolate across the ring start. TIN: the export TIN's Delaunay factory at 0.1 ft (binary32 widened): extent of perimeter and mass points (at least 10 x tolerance, 1:8) widened to three times the perimeter's extent, perimeter points, then the points inside the polygon as mass points, perimeter enforced (an edge that fails stops the enforcement, as in RAS Mapper), triangles touching the super polygon deleted and the deletion flooded across non-perimeter edges. Rasterized per output tile, combined over the cells holding the polygon's extent. | Synthetic: interior points 0 of 2,500; stored profile 2 of 4,080 at 1/32 ft; three perimeter points 0 of 1,225; one perimeter point 0 of 2,500; Add 0 of 2,500; real C: 7 boundary-elevation polygons, no difference; real D: 202 boundary-elevation polygon layers with perimeter control points, 3 px at 1/32 ft | supported |
| Polygon with boundary elevations sampled from the terrain (gen = 1, no stored profile, no perimeter control point) | RAS Mapper samples each source raster and the terrain HDF's stored level-0 stitch TIN along the perimeter, without modifications, filtered at 0.01; not reproduced. | — | refused |
| Polygon with shape Z, multi-part polygon needing a TIN | Vertex z storage not measured (a third point column makes RAS Mapper skip the layer); holes need RAS Mapper's multi-part deletion. | — | refused |
| Ellipse / circle, any rotation | Output-tile pixel centre: circle (equal radii) distance <= radius; ellipse `(c dx + s dy)^2/rx^2 + (s dx - c dy)^2/ry^2 <= 1`, c, s = cos, sin(-rotation in degrees). **Inclusive.** | A: 86 px, 0 diff; synthetic five ellipses (exact circle boundary, rotations 0, 30, -63.5, 90): 0 of 1,005 | supported |

## HEC-RAS 6.6 (`hecras_version: "6.6"`)

Exports with HEC-RAS 6.6 of the synthetic cases, against 7.0.1: identical base
terrain; identical modifications for unnamed ground lines (bends, short
segments, SetValue, narrow templates, control points, Add, NaN and negative
templates), constant polygons and ellipses. Different: ground lines whose Max
Reach is not wider than one cell are not drawn (15 and 20 ft on 20 ft cells:
skipped; 25 and 40 ft: drawn as in 7.0.1); named systems (486-990 px apart);
a boundary-elevation polygon with a stored profile (6.6 samples the terrain,
4,080 px apart; perimeter control points give the same profile as 7.0.1). The
engine follows 6.6 for the narrow skip (per output tile, Max Reach / cell <= 1)
and refuses the two other forms. Under 6.6 every accepted synthetic case
matches (2 of 8,935 and 1 of 2,499 px at 1/32 ft, the rest exact).

## Real terrains (whole terrain vs export a, pixels where two exports differ excluded)

| Terrain | Before (fitted rule): mask / value / max | After: mask / value / max | Inside footprints after |
|---|---|---|---|
| A | 0 / 322 / 0.53 ft | 0 / 322 / 0.53 ft | 0 (the 39 "modified" px lie outside every footprint, where the base and modified exports' gap fills differ) |
| B1 | 2 / 3 / 1/32 ft | 2 / 0 / 0 | 0 |
| B2 | 2 / 443 / 1.47 ft (439 on modified px) | 2 / 0 / 0 | 0 |
| B3 | 2 / 0 / 0 | 2 / 0 / 0 | 0 |
| B4 | 0 / 0 / 0 | 0 / 0 / 0 | 0 |

Real terrain C (2 ft, 4 sources).
Scope: an HDF copy reduced to the 10 accepted layers (3 constant polygon
layers; 7 ground-line layers with 44 features, among them the named systems
one 3-feature system and named features that stay alone after joining, and a
SetValue channel layer; many slopes of 0 and Max Reach =
Top Width). Removed: an Add levee, polygons with boundary elevations, and a
levee layer with a NaN template. Two base and two modified exports of the
copy, which agree with each other. Before this change the named layers were
refused.

| Scope | Pixels | Mask | Value | Max |
|---|---|---|---|---|
| Whole terrain, source extent | 379.0M | 0 | 4 | 1/32 ft |
| Whole terrain, export grid (674.6M px; 73 modified pixels lie beyond the sources) | 674.6M | 0 | 4 | 1/32 ft |
| Each layer's footprint (engine run with the layer alone) | 0.4M | 0 | 0, except 1 in the SetValue channel layer | 1/32 ft |

The engine's base equals base export a on C.

With control points, boundary elevations, Add and NaN templates supported,
terrain C needs no reduction: all 13 layers, two modified exports (which
agree) against the earlier two base exports: 0 mask and 4 value differences
at 1/32 ft in 379.0M px (three lie outside every feature's extent).

Real terrain E (3 ft, 1.14G px; one source). Scope: an HDF copy without its
polygon layer whose boundary elevations are sampled from the terrain; kept:
a 14-feature levee layer with 2,862 control points (Hi and Set), SetIfHigher
levee layers, a channel and a constant polygon layer. Two base and two
modified exports, which agree with each other: 0 mask and 3 value differences, all at 1/32 ft and all inside the control-point levee layer; no pixel differs outside the modifications.

Real terrain D (one source, 924.6M px). Scope: an HDF copy without the one polygon layer whose
boundary elevations are sampled from the terrain; kept: 228 layers (202
boundary-elevation SetValue polygon layers with perimeter control points, 20
channel layers with control points, levees, channels, an ellipse layer). Two
base and two modified exports, which agree with each other: 0 mask and 3 value differences, all at 1/32 ft and all inside boundary-elevation polygons; no pixel differs outside the modifications.

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
- The system path was then implemented from the study's description and
  checked the same way: RasMapperLib's system ribbons for W3 and for terrain
  C's systems, obtained through pythonnet, equal the engine's point for point
  (except the 4 points below), and RasMapperLib's rasterization of its own
  system TIN reproduces the W3 export exactly.

## Open discrepancies (measured, not explained)

1. **Binary64 powers.** RasMapperLib computes distances and cubic blends with
   .NET Framework's `Math.Pow` (for example `Pow(x, 0.5)`), which is not
   correctly rounded: it differs from the correctly rounded square root or
   product on about 0.3% of inputs. The engine uses correctly rounded
   arithmetic. On terrain C's 3-feature system this moves 4 stream-line points
   by up to 6e-5 ft; no pixel changed in the measured cases.
2. **Isolated 1/32 ft roundings at exact ties** (2 px of 8,935 on the sharp-bend
   synthetic case). RAS Mapper rounds the other way; a numeric difference in the
   last bits.
3. **Mass-point order.** RAS Mapper inserts interior control points in its
   quadtree query order; the engine uses stored order. Only cocircular ties
   depend on it. Not observed.
4. **Joining candidates.** RAS Mapper looks for the next feature through a
   spatial index bin around the end point; the engine tests every feature
   within 60 ft. A candidate in a different bin could join in the engine and
   not in RAS Mapper. Not observed.
