# Max shear stress

Method `rasmapper-face-shear-max/1`, advertised on the `sloping` method as
`shear_stress`, with `shear_stress_depth: ["rendered", "horizontal"]`. It
renders RAS Mapper's **Shear Stress (Max)** stored map for physical 2D cells.
Stored output times, storage areas, 1D elements and the slopingPretty face
options are not part of it.

## Definition

RAS Mapper 7.0.1 does not take the Max of a per-pixel shear time series, and it
does not use the time of maximum depth or velocity. It reads a **face**
quantity, the PostProcessing `Maximum Shear` dataset, which its post-processor
fills by computing every face's shear vector at every stored output time and
keeping the largest magnitude. The stored map then interpolates those face
scalars. Shear stress is not stored in the plan HDF; it is derived.

At each stored output time, per mesh:

1. **Face water surface.** Each face is classified from its two cells' water
   surfaces with RAS Mapper's adjusted face rule (`ComputeFaceWSsNew`, the same
   classification the sloping WSE renderer uses). A face is *connected* unless
   it is `None` (a dry cell, a perimeter face, or both sides below the face
   minimum) or `Levee`. Its water surface is `ValueA`.
2. **Hydraulic radius** (connected faces only; other faces 0). From the face's
   `Faces Area Elevation` table (columns elevation, area, wetted perimeter,
   Manning's n):
   - area: at the first row `i >= 1` whose elevation is `>= ws`,
     `top width = (A[i] - A[i-1]) / (z[i] - z[i-1])` and
     `area = A[i-1] + top width * (ws - z[i-1])`. Above the table the top width
     is the face polyline length: `area = A[last] + length * (ws - z[last])`.
   - wetted perimeter: at the same row, `t = (ws - z[i-1]) / (z[i] - z[i-1])`
     and `P = P[i-1] * t + P[i] * (1 - t)`. The weights are reversed with respect
     to linear interpolation; this is RAS Mapper's rule and is reproduced as is.
     Above the table, `P = P[last]`.
   - `R = area / P`, all in binary32.
3. **Facepoint velocity.** For each facepoint and each hydraulically connected
   arc of its incident faces, a weighted least-squares fit of the faces'
   normal velocities, with weights `1 / polyline length`: accumulate
   `n n^T w` and `n u w` over the arc's faces, plus the arc's end face when it
   is not connected, then solve the 2 x 2 system by its scaled inverse (binary32;
   a singular system uses `1 / count` on the diagonal). A facepoint whose
   incident faces are all dry on both sides gets zero.
   (`ComputeSlopingFacepointVelocities_LeastSquares`.) This is not the C-stencil
   reconstruction the velocity map uses.
4. **Face velocity.** A connected face takes the mean of the arc vectors of its
   two facepoints at that face (`ComputeFaceTrueVelocities`); other faces get
   zero.
5. **Face shear** (`ComputeFaceTrueShear`):
   `tau = k * |V| * V * n * n / float32(pow(R, 0.33333333333333337))`, evaluated
   left to right in binary32 with the root from a binary64 `pow`, where
   `k = 28.258362` for US Customary (62.4 / 1.486^2) and `9810` for SI, and `n` is
   the face's **first** `Faces Area Elevation` Manning's n. `tau = 0` when
   `V = 0` or `R = 0`. Its magnitude is `float32(sqrt(x*x + y*y))`.
   This is the Manning friction slope form, `tau = gamma R S_f` with
   `S_f = n^2 V^2 / (k_m^2 R^(4/3))`.
6. **Max.** Per face, the largest magnitude over every stored output time
   (start 0, replace only on a strictly larger value).

Rendering (`ShearRenderer`, Max branch):

7. Each facepoint takes the binary32 mean of its incident faces' Max values,
   in ring order (`AverageFaceScalarsToFacepointValuesSA`), including dry and
   perimeter faces, whose Max is 0.
8. Pixels are painted from the cell's facepoint values with the **plain
   sloping facepoint weights** (`FacepointWeightedPaint` with `FPPrevWeights`),
   the weights plain sloping WSE uses, on the native source-tile lattice for
   square cells. A cell is skipped when its Max WSE is `<= minimum + 0.001`.
9. A pixel is drawn where the clipping depth is `> 0`. The values do **not**
   depend on the WSE render mode; RAS Mapper clips them with the active mode's
   Max depth. `shear_stress.depth = "rendered"` uses the request's sloping
   depth, `"horizontal"` the cell-constant Max depth (RAS Mapper's horizontal
   mode).

Sources: RAS Mapper 7.0.1 behaviour, read from `RasMapperLib.dll`
(`Render.ShearRenderer.Compute`, `PostProcessor.ComputeMaxMinShear2D` and
`ComputePostProcessHydraulicFaceProperties`, `MeshFV2D.ComputeFaceTrueShear`,
`ComputeFaceTrueVelocities`, `ComputeFacePointVelocity_FacePerpLeastSquares_Weighted`,
`ComputeFaceAreaAndTopWidth`, `ComputeFacePerimeter`, `ComputeFaceWSsNew`,
`RASD2FlowArea.ReadFaceMannValues`, `Render.Renderer.ComputeFromFaceScalarValues`),
and checked against native stored maps and the PostProcessing `Maximum Shear`
datasets RAS Mapper wrote for them (below). No decompiled code is in this
repository. The rule is the open-channel boundary shear `tau = gamma R S_f`
with a Manning friction slope. The HEC-RAS user documentation does not state
the face, hydraulic-radius and Max details above, so they were read from the
implementation and qualified against its output.

## Request

`sloping` requests take an optional `shear_stress` block:

```json
"shear_stress": {
  "units": "US Customary",
  "depth": "rendered",
  "meshes": [{
    "normals_file": "shear-normals-0.bin",
    "tables_info_file": "shear-tables-info-0.bin",
    "tables_values_file": "shear-tables-values-0.bin",
    "table_rows": 260000,
    "times": 433,
    "water_surface_file": "shear-water-surface-0.bin",
    "face_velocity_file": "shear-face-velocity-0.bin"
  }]
}
```

Per mesh, in HDF order: float32 `(x, y, polyline length)` face normals (as
`velocity_normals_files`), int32 `Faces Area Elevation Info`, float32
`Faces Area Elevation Values`, and every stored output time's float32 cell
`Water Surface` (ghost cells included) and `Face Velocity` as contiguous
time-major arrays. The request must have exactly one profile, the Max profile
(`Maximum Water Surface`), and plain sloping options; reduction, native GeoTIFF
outputs, the stored tile grid, row streaming, storage areas, consumers,
boundary and depth-group requests refuse it. A face without a
`Faces Area Elevation` table is refused, as RAS Mapper cannot read its
Manning's n either.

The face pass streams the time series one output time at a time (one cell row
and one face row in memory). Outputs: `shear_stress.bin` per profile (or a
float32 frame after the derived products in the stream, with
`"shear_stress": true` in the header), and `shear_face_max-<mesh>.bin`, the
per-face Max (RAS Mapper's PostProcessing `Maximum Shear`), in the output
folder. `report.json` records the method, units, factor, clipping depth and
the face pass's time.

ras-commander exposes this as
`RasFim.map_sloping_profiles(..., shear_stress=True, shear_depth=...)` and
`RasFim.supports_shear_stress()`.

## Qualification

Native HEC-RAS 7.0.1 `RasProcess.store_maps` on fresh project copies (no
cached PostProcessing), compared over complete masks and every valid binary32
bit, and the engine's face Max against RAS Mapper's PostProcessing
`Maximum Shear` of the same run:

| Model, plan | Clipping depth | Valid pixels | Mask / bit differences | Face Max (faces, bit differences) |
|---|---|---|---|---|
| BaldEagleCrkMulti2D bridges p05, 433 times | sloping | 973,902 | 0 / 0 | 54,636, 0 |
| BaldEagleCrkMulti2D bridges p05 | horizontal | 973,532 | 0 / 0 | 54,636, 0 |
| Davis p02, 217 times, 2 meshes | sloping | 22,041 | 0 / 0 | 5,555 + 35, 0 |
| Davis p02 | horizontal | 20,560 | 0 / 0 | 5,555 + 35, 0 |

Bridges ran on its RAS Mapper terrain export, Davis on its CRS-canonical
source copy, the grids of the native references. Storage areas, slopingPretty,
stored output times and SI-unit models are not qualified (SI uses the same code
path with `k = 9810`; no SI 2D reference was run).
