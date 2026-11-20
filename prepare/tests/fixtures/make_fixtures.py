"""Regenerate the synthetic fixtures and their ras-commander 0.124.0 expectations.

The meshes are those of ras-commander 0.124.0 `tests/test_fim_invalid_cells.py`
(MIT, Copyright (c) 2024 William M. Katzenmeyer): a bowtie cell (a west face
whose interior perimeter point crosses back over the south face) and minimal
meshes whose square cells overlap it (split, consumed, cascade, hole, tie).
Each case is loaded with ras-commander's `_invalid_cells.load_cells` +
`require_cells` under every policy; `expected-cells.json` records the ownership
rows (mesh, cell id, little-endian 2-D WKB) and the `fim-invalid-cells/1` record,
or the refusal. `expected-lattice.json` records `_aoi_lattice.index_range`,
`_aoi.center_index_range`, rasterio window transforms and CPython 3.12
`sys.getsizeof` values for the unit tests.

Run with the oracle pins (ras-commander 0.124.0, ras-commander-fast-hdf 0.5.0,
shapely 2.1.2 / GEOS 3.13.1, rasterio 1.5.2, Python 3.12):
    python make_fixtures.py
"""
import hashlib
import json
import sys
from pathlib import Path

import h5py
import numpy as np
from pyproj import CRS

HERE = Path(__file__).resolve().parent
MESH = "Bowtie Mesh"
BOWTIE_RING = [(0, 0), (10, 10), (0, 10), (10, 0)]


def _attrs(hdf):
    attributes = np.zeros(1, dtype=[("Name", "S64")])
    attributes["Name"] = MESH.encode()
    hdf.attrs["Projection"] = np.bytes_(CRS.from_epsg(26915).to_wkt(version="WKT1_ESRI"))
    hdf["Geometry/2D Flow Areas/Attributes"] = attributes


def write_bowtie_mesh(path):
    points = np.array([[0, 0], [10, 0], [10, 10], [0, 10], [-10, 0], [-10, 10], [10, -10], [0, -10]], dtype=float)
    faces = np.array([[0, 1], [1, 2], [2, 3], [3, 0], [0, 4], [4, 5], [5, 3], [1, 6], [6, 7], [7, 0]], dtype=np.int32)
    perimeter_info = np.zeros((len(faces), 2), dtype=np.int32)
    perimeter_info[3] = [0, 1]
    perimeter_values = np.array([[2.0, -1.0]])
    info, values = [], []
    for ids in [[0, 1, 2, 3], [3, 4, 5, 6], [0, 7, 8, 9]]:
        info.append([len(values), len(ids)])
        values.extend([face, 1] for face in ids)
    with h5py.File(path, "w") as hdf:
        _attrs(hdf)
        base = f"Geometry/2D Flow Areas/{MESH}"
        hdf[f"{base}/FacePoints Coordinate"] = points
        hdf[f"{base}/Faces FacePoint Indexes"] = faces
        hdf[f"{base}/Faces Perimeter Info"] = perimeter_info
        hdf[f"{base}/Faces Perimeter Values"] = perimeter_values
        hdf[f"{base}/Cells Face and Orientation Info"] = np.asarray(info, dtype=np.int32)
        hdf[f"{base}/Cells Face and Orientation Values"] = np.asarray(values, dtype=np.int32)


def write_cells(path, shapes):
    points, faces, info, values = [], [], [], []
    for ring in [BOWTIE_RING] + [list(shape.exterior.coords)[:-1] for shape in shapes]:
        first_point, first_face = len(points), len(faces)
        points.extend(ring)
        faces.extend((first_point + i, first_point + (i + 1) % len(ring)) for i in range(len(ring)))
        info.append((len(values), len(ring)))
        values.extend((first_face + i, 1) for i in range(len(ring)))
    with h5py.File(path, "w") as hdf:
        _attrs(hdf)
        base = f"Geometry/2D Flow Areas/{MESH}"
        hdf[f"{base}/FacePoints Coordinate"] = np.asarray(points, dtype=float)
        hdf[f"{base}/Faces FacePoint Indexes"] = np.asarray(faces, dtype=np.int32)
        hdf[f"{base}/Faces Perimeter Info"] = np.zeros((len(faces), 2), dtype=np.int32)
        hdf[f"{base}/Faces Perimeter Values"] = np.empty((0, 2), dtype=float)
        hdf[f"{base}/Cells Face and Orientation Info"] = np.asarray(info, dtype=np.int32)
        hdf[f"{base}/Cells Face and Orientation Values"] = np.asarray(values, dtype=np.int32)


def main():
    import math
    import shapely
    from shapely.geometry import box
    from affine import Affine
    import rasterio.windows
    from ras_commander.fim import _aoi, _aoi_lattice, _invalid_cells

    cases = {
        "bowtie": None,
        "split": [box(4, -1, 6, 11), box(0, 0, 10, 10)],
        "consumed": [box(0, 0, 10, 10), box(1, 1, 9, 9)],
        "cascade": [box(9, 0, 20, 10), box(15, 0, 30, 10), box(25, 0, 40, 10)],
        "hole": [box(12, 2, 14, 4), box(10, 0, 20, 10)],
        "tie": [],
    }
    expected = {}
    files = {}
    for name, shapes in cases.items():
        path = HERE / f"cells-{name}.hdf"
        if shapes is None:
            write_bowtie_mesh(path)
        else:
            write_cells(path, shapes)
        files[path.name] = hashlib.sha256(path.read_bytes()).hexdigest()
        for policy in ("repair", "hull", "omit", "raise"):
            try:
                cells, records = _invalid_cells.load_cells(path, policy=policy, threads=1)
                _invalid_cells.require_cells(cells, records, policy)
                result = {
                    "rows": [
                        {"mesh": m, "cell_id": int(c), "wkb": shapely.to_wkb(g, byte_order=1, output_dimension=2).hex()}
                        for m, c, g in zip(cells.mesh_name, cells.cell_id, cells.geometry)
                    ],
                    "invalid_cells": json.loads(json.dumps(_invalid_cells.describe(records, policy, None))),
                }
            except ValueError as exc:
                result = {"refused": str(exc)}
            expected[f"{name}/{policy}"] = result
    (HERE / "expected-cells.json").write_text(json.dumps(expected, indent=1, sort_keys=True) + "\n", encoding="utf-8")

    t = Affine(6.561666666666606, 0.0, 1738955.4338581087, 0.0, -6.561666666666606, 14017250.5176182)
    lattice = {"index_range": [], "center_index_range": [], "window_transform": [], "getsizeof": {}}
    probes = [
        (1851757.0455247744, 1863968.1, 1738955.4338581087, 6.561666666666606, 25655),
        (13924448.0, 13937263.9, 14017250.5176182, -6.561666666666606, 23187),
        (1738955.4338581087, 1738958.7, 1738955.4338581087, 6.561666666666606, 25655),
        (1738900.0, 1738955.4338581087, 1738955.4338581087, 6.561666666666606, 25655),
        (14017240.0, 14017250.5176182, 14017250.5176182, -6.561666666666606, 23187),
        (1907290.0, 1907400.0, 1738955.4338581087, 6.561666666666606, 25655),
    ]
    for lo, hi, origin, step, count in probes:
        lattice["index_range"].append([lo, hi, origin, step, count, list(_aoi_lattice.index_range(lo, hi, origin, step, count))])
        lattice["center_index_range"].append([lo, hi, origin, step, count, list(_aoi.center_index_range(lo, hi, origin, step, count))])
    for col, row in [(0, 0), (17191, 12190), (25654, 23186), (1, 23186)]:
        w = rasterio.windows.Window(col, row, 10, 10)
        wt = rasterio.windows.transform(w, t)
        lattice["window_transform"].append([col, row, list(wt.to_gdal()), list(rasterio.windows.bounds(w, t))])
    for s in ("UPGU1_2DArea", "UPGU3_2DArea", "Perimeter 1", "Bowtie Mesh", "café", "Ārea", "x\U0001f30a"):
        lattice["getsizeof"][s] = sys.getsizeof(s)
    lattice["getsizeof_int"] = {str(v): sys.getsizeof(v) for v in (0, 1, 413150, 2**30 - 1, 2**30, 2**60)}
    lattice["python"] = sys.version.split()[0]
    (HERE / "expected-lattice.json").write_text(json.dumps(lattice, indent=1) + "\n", encoding="utf-8")
    (HERE / "SHA256SUMS").write_text("".join(f"{v}  {k}\n" for k, v in sorted(files.items())), encoding="utf-8")
    print(json.dumps({"cases": len(expected), "files": files}, indent=1))


if __name__ == "__main__":
    main()
