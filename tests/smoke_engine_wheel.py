"""Exercise an installed platform wheel with a small real GeoTIFF and Step05 request.

Synthetic packaging smoke test; not real-model numerical qualification.
Run explicitly: python tests/smoke_engine_wheel.py.
"""
import json
from pathlib import Path
import shutil
import subprocess
import tempfile

import numpy as np
import rasterio
from affine import Affine

from scipy.spatial import Delaunay


def sloping_smoke(engine, root, terrain):
    """Packaging smoke: typed geometry, two profiles, native ownership and AOI."""
    import io
    import struct
    folder=root/"sloping-inputs";folder.mkdir()
    tables={
        "centers":([[2,2],[-1,2],[2,5],[5,2],[2,-1]],"<f8"),
        "cell_info":([[0,4],[4,1],[5,1],[6,1],[7,1]],"<i4"),
        "cell_faces":([[0,1],[1,1],[2,1],[3,1],[0,-1],[1,-1],[2,-1],[3,-1]],"<i4"),
        "cell_minimum":([100,0,0,0,0],"<f4"),
        "face_vertices":([[0,1],[1,2],[2,3],[3,0]],"<i4"),
        "face_cells":([[0,1],[0,2],[0,3],[0,4]],"<i4"),
        "face_info":(np.zeros((4,2)),"<i4"),
        "face_points":(np.empty((0,2)),"<f8"),
        "face_minimum":([100]*4,"<f4"),
        "vertices":([[0,0],[0,4],[4,4],[4,0]],"<f8"),
        "vertex_info":([[0,2],[2,2],[4,2],[6,2]],"<i4"),
        "vertex_faces":([[3,1],[0,1],[0,1],[1,1],[1,1],[2,1],[2,1],[3,1]],"<i4"),
    }
    descriptors={}
    for key,(data,dtype) in tables.items():
        array=np.asarray(data,dtype=dtype);filename=f"{key}.bin";array.tofile(folder/filename)
        descriptors[key]={"path":filename,"rows":len(array)}
    (folder/"mesh.json").write_text(json.dumps({"schema":"ras-commander-sloping-mesh/1","arrays":descriptors}))
    def polygon(filename,points):
        wkb=struct.pack("<BIII",1,3,1,len(points))+np.asarray(points,dtype="<f8").tobytes()
        (folder/filename).write_bytes(struct.pack("<I",len(wkb))+wkb)
    polygon("cell.wkb",[[0,0],[0,4],[4,4],[4,0],[0,0]])
    polygon("aoi.wkb",[[0,0],[0,4],[2,4],[2,0],[0,0]])
    for name,value in [("a",101),("b",102)]:np.array([value,0,0,0,0],dtype="<f4").tofile(folder/f"{name}.bin")
    request=dict(schema="ras-commander-sloping-request/1",terrain=str(terrain),epsg=3857,
        crs_identity={"kind":"epsg"},terrain_transform=[0.,1.,0.,8.,0.,-1.],window=[0,4,4,4],
        geometry_file="cell.wkb",mesh_files=["mesh.json"],owner_cells=[[0,0]],
        profiles=[dict(id=name,values_files=[f"{name}.bin"]) for name in ["a","b"]],
        threads=4,max_working_bytes=256<<20)
    path=folder/"request.json";path.write_text(json.dumps(request))
    subprocess.run([engine,"sloping",str(path),str(folder/"files")],check=True,capture_output=True)
    for name,value in [("a",1),("b",2)]:
        actual=np.fromfile(folder/"files"/name/"depth.bin",dtype="<f4")
        np.testing.assert_array_equal(actual,np.full(16,value,dtype="<f4"))
    # Elevations are optional for plain mode; enhanced depth weighting requires them.
    np.full(4, 100, dtype="<f4").tofile(folder/"vertex_elevations.bin")
    descriptors["vertex_elevations"] = dict(path="vertex_elevations.bin", rows=4)
    (folder/"mesh.json").write_text(json.dumps({"schema":"ras-commander-sloping-mesh/1","arrays":descriptors}))
    for shallow in (False, True):
        for weighted in (False, True):
            request["max_inflight_profiles"] = 2
            request["options"] = dict(render_mode="slopingPretty", reduce_shallow_to_horizontal=shallow, use_depth_weighted_faces=weighted)
            path.write_text(json.dumps(request))
            destination=folder/f"enhanced-{int(shallow)}-{int(weighted)}"
            subprocess.run([engine,"sloping",str(path),str(destination)],check=True,capture_output=True)
            report=json.loads((destination/"report.json").read_text())
            assert report["method"]["method_id"]=="rasmapper-sloping-with-faces/1"
            assert report["effective_inflight_profiles"] == 2
            for name,value in [("a",1),("b",2)]:
                np.testing.assert_array_equal(np.fromfile(destination/name/"depth.bin",dtype="<f4"),np.full(16,value,dtype="<f4"))
    # An upper bound may be reduced by memory admission before allocation.
    request["max_working_bytes"] = report["planned_bytes"] - report["additional_profile_planned_bytes"]
    path.write_text(json.dumps(request))
    subprocess.run([engine,"sloping",str(path),str(folder/"memory-capped")],check=True,capture_output=True)
    assert json.loads((folder/"memory-capped/report.json").read_text())["effective_inflight_profiles"] == 1
    request["max_working_bytes"] = 256 << 20
    request["options"] = dict(render_mode="slopingPretty", reduce_shallow_to_horizontal=True, use_depth_weighted_faces=True)
    request.update(stream_outputs=True,aoi_file="aoi.wkb",threads=1)
    path.write_text(json.dumps(request))
    output=subprocess.run([engine,"sloping",str(path),str(folder/"stream")],check=True,capture_output=True).stdout
    stream=io.BytesIO(output)
    for name,value in [("a",1),("b",2)]:
        assert json.loads(stream.readline())==dict(id=name,status="complete",pixels=16,storage_render_seconds=0.0)
        for band,level in [("depth",value),("wse",100+value)]:
            actual=np.frombuffer(stream.read(64),dtype="<f4").reshape(4,4)
            expected=np.full((4,4),level,dtype="<f4");expected[:,2:]=np.nan
            np.testing.assert_array_equal(actual,expected,err_msg=band)
    assert not stream.read()
    # Native output controls preserve decoded values and explicitly set compression.
    request.update(geotiff_outputs=True, products=["depth"])
    for compression in ("NONE", "LZW"):
        request["compression"] = compression
        path.write_text(json.dumps(request))
        destination = folder / f"native-{compression}"
        result = subprocess.run([engine, "sloping", str(path), str(destination)],
                                check=True, capture_output=True)
        headers = [json.loads(line) for line in result.stdout.splitlines()]
        assert all(h["selected_products"] == ["depth"] for h in headers)
        for name, level in [("a", 1), ("b", 2)]:
            assert not (destination / name / "wse.tif").exists()
            with rasterio.open(destination / name / "depth.tif") as ds:
                expected = np.full((4, 4), level, dtype="<f4")
                expected[:, 2:] = np.nan
                np.testing.assert_array_equal(ds.read(1).view("<u4"), expected.view("<u4"))
                assert (ds.compression is None) == (compression == "NONE")
    model = dict(request)
    for key in ("products", "compression", "stream_outputs", "geotiff_outputs", "aoi_file"):
        model.pop(key, None)
    model.update(options={}, max_inflight_profiles=1)
    grouped = dict(schema="ras-commander-depth-groups-request/1", model=model,
        source_indices=[0, 1], depth_units="feet", depth_cutoff_ft=1.5,
        max_accumulator_bytes=0, compression="NONE",
        scratch=dict(path=str(root), node_local=True, max_bytes=1024),
        jobs=[dict(job_id="left", reach_id="left", window=[0, 4, 4, 4], aoi_file="aoi.wkb",
                   groups=[dict(group_id="both", timesteps=[1, 0]),
                           dict(group_id="below-cutoff", timesteps=[0])])])
    path.write_text(json.dumps(grouped))
    grouped_out = folder / "groups"
    subprocess.run([engine, "depth-groups", str(path), str(grouped_out)], check=True, capture_output=True)
    report = json.loads((grouped_out / "report.json").read_text())
    assert report["status"] == "complete" and report["hydraulic_preparations"] == 2
    assert report["geometry_preparations"] == 1 and report["scratch_peak_bytes"] == 128
    assert report["jobs"][0]["groups"][0]["source_indices"] == [1, 0]
    with rasterio.open(grouped_out / "left/both.tif") as ds:
        expected = np.full((4, 4), 2, dtype="<f4"); expected[:, 2:] = np.nan
        np.testing.assert_array_equal(ds.read(1).view("<u4"), expected.view("<u4"))
        assert ds.crs.to_epsg() == 3857 and ds.transform == Affine(1, 0, 0, 0, -1, 4)
    with rasterio.open(grouped_out / "left/below-cutoff.tif") as ds:
        assert np.isnan(ds.read(1)).all() and ds.shape == (4, 4)
    assert sorted(p.name for p in grouped_out.iterdir()) == ["left", "report.json"]
    # Exact byte lengths keep newlines inside Float32 payloads out of JSON framing.
    grouped["sink"] = "grouped-buffer"
    path.write_text(json.dumps(grouped))
    result = subprocess.run([engine, "depth-groups", str(path), str(folder / "group-buffers")],
                            check=True, capture_output=True)
    stream = io.BytesIO(result.stdout)
    for name in ("both", "below-cutoff"):
        header = json.loads(stream.readline())["group"]
        assert header["group_id"] == name and header["byte_length"] == 64
        data = np.frombuffer(stream.read(header["byte_length"]), dtype="<f4")
        assert len(data) == 16
    assert json.loads(stream.readline()) == {"command_status": "complete"}
    assert not stream.read()
    print("Installed wheel: native selection/compression, grouped maxima/spill/buffers PASS")
    print("Installed wheel: plain/enhanced sloping options, binary geometry, profiles, file/stream AOI PASS")


def any_crs_smoke(engine, root):
    """A CRS without an EPSG code (ESRI:102630, US survey feet): GDAL reads the
    engine's products in exactly the terrain's CRS (verbatim GeoKeys)."""
    folder = root / "sloping-inputs"
    request = dict(schema="ras-commander-sloping-request/1", terrain_transform=[0., 1., 0., 8., 0., -1.],
        window=[0, 4, 4, 4], geometry_file="cell.wkb", mesh_files=["mesh.json"], owner_cells=[[0, 0]],
        profiles=[dict(id=name, values_files=[f"{name}.bin"]) for name in ["a", "b"]],
        threads=2, max_working_bytes=256 << 20)
    results = {}
    for name, crs in [("epsg", "EPSG:3857"), ("esri", "ESRI:102630")]:
        terrain = root / f"terrain-{name}.tif"
        with rasterio.open(terrain, "w", driver="GTiff", width=8, height=8, count=1,
                           dtype="float32", crs=crs, transform=Affine(1, 0, 0, 0, -1, 8),
                           nodata=-9999) as ds:
            ds.write(np.full((8, 8), 100, dtype=np.float32), 1)
        info = json.loads(subprocess.check_output([engine, "terrain-info", str(terrain)], text=True))
        model = dict(request)
        model.update(terrain=str(terrain), aoi_file="aoi.wkb", stream_outputs=True,
                     geotiff_outputs=True, products=["depth", "wse"], compression="DEFLATE",
                     final_format={"nodata": -9999.0, "block_size": 16})
        if name == "epsg":
            model.update(epsg=3857, crs_identity={"kind": "epsg"})
        else:
            assert info["epsg"] is None and info["geokeys"]["directory"][:2] == [1, 1]
            model.update(crs_geokeys=info["geokeys"], crs_identity={"kind": "geokeys"})
        for final in (True, False):
            if not final:
                model.pop("final_format"); model["compression"] = "LZW"
            path = folder / f"crs-{name}-{final}.json"
            path.write_text(json.dumps(model))
            out = folder / f"crs-{name}-{final}"
            subprocess.run([engine, "sloping", str(path), str(out)], check=True, capture_output=True)
            for profile in ("a", "b"):
                for product in ("depth", "wse"):
                    with rasterio.open(terrain) as src, rasterio.open(out / profile / f"{product}.tif") as ds:
                        assert ds.crs == src.crs and ds.crs.to_wkt() == src.crs.to_wkt(), (name, ds.crs)
                        results[(name, final, profile, product)] = ds.read(1)
    for key, values in results.items():
        if key[0] == "esri":
            np.testing.assert_array_equal(values.view("<u4"), results[("epsg",) + key[1:]].view("<u4"))
    print("Installed wheel: ESRI:102630 terrain, verbatim GeoKeys in strip and final products PASS")


def native_one_d_smoke(engine, root):
    """Installed executable includes ranked scanlines and global crop transport."""
    folder=root/"native-one-d";folder.mkdir()
    payloads={
        "points.bin":( [[0,0,1,0],[0,4,1,0],[4,0,0,0],[4,4,0,0]],"<f8"),
        "triangles.bin":([[0,2,1],[1,2,3]],"<i4"),
        "info.bin":([[0,4,0,2]],"<i4"),"ids.bin":([[0,1]],"<i4"),
        "ranks.bin":([0],"<u4"),"levels.bin":([12,8],"<f4"),
        "terrain.bin":([0]*16,"<f4"),"crop.bin":([0]*4,"<f4"),
    }
    for name,(values,dtype) in payloads.items():np.asarray(values,dtype=dtype).tofile(folder/name)
    request=dict(schema="fast-map-one-d-diagnostic/2",scope="xs-surface-only",
        points_file="points.bin",point_count=4,triangles_file="triangles.bin",triangle_count=2,
        tin_info_file="info.bin",xs_ids_file="ids.bin",segment_count=1,cross_section_count=2,
        levels_file="levels.bin",terrain_file="terrain.bin",width=4,height=4,
        geo_transform=[0.,1.,0.,4.,0.,-1.],render_order_file="ranks.bin",tile_size=2,
        max_working_bytes=1<<20)
    for name in ["full","crop"]:
        if name=="crop":request.update(window=dict(col=1,row=1,width=2,height=2),terrain_file="crop.bin")
        path=folder/(name+".json");path.write_text(json.dumps(request))
        subprocess.run([engine,"one-d-diagnostic",str(path),str(folder/name)],check=True,capture_output=True)
        report=json.loads((folder/name/"report.json").read_text())
        assert report["method"]["method_id"]=="one-d-xs-surface-diagnostic/4"
    for product in ["depth","wse"]:
        full=np.fromfile(folder/"full"/(product+".bin"),dtype="<f4").reshape(4,4)
        crop=np.fromfile(folder/"crop"/(product+".bin"),dtype="<f4").reshape(2,2)
        np.testing.assert_array_equal(full,np.tile([11.5,10.5,9.5,8.5],(4,1)))
        np.testing.assert_array_equal(crop.view("u4"),full[1:3,1:3].view("u4"))
    print("Installed wheel: native-ranked 1D scanlines and global AOI PASS")
    np.asarray([-2.,2.,-2.,2.],dtype="<f8").tofile(folder/"normalized-m.bin")
    clips=np.full((2,8),np.nan,dtype="<f4");clips[:,2]=0.;clips[:,3]=100.;clips.tofile(folder/"clips.bin")
    request.pop("window")
    request.update(schema="fast-map-one-d-diagnostic/3",terrain_file="terrain.bin",
        normalized_m_file="normalized-m.bin",xs_clip_file="clips.bin",clipping_scope="levees-and-xs-end-obstructions-only")
    path=folder/"clipped.json";path.write_text(json.dumps(request))
    subprocess.run([engine,"one-d-diagnostic",str(path),str(folder/"clipped")],check=True,capture_output=True)
    report=json.loads((folder/"clipped/report.json").read_text());assert report["method"]["method_id"]=="one-d-xs-surface-diagnostic/5"
    for product in ["depth","wse"]:
        clipped=np.fromfile(folder/"clipped"/(product+".bin"),dtype="<f4").reshape(4,4)
        full=np.fromfile(folder/"full"/(product+".bin"),dtype="<f4").reshape(4,4)
        assert np.isnan(clipped[:2]).all()
        np.testing.assert_array_equal(clipped[2:].view("u4"),full[2:].view("u4"))
    print("Installed wheel: explicit normalized-M levee clipping PASS")


def main():
    engine = shutil.which("ras2fim-batch")
    assert engine, "wheel did not install ras2fim-batch on PATH"
    method = json.loads(subprocess.check_output([engine, "method"], text=True))
    assert "step05_r2" in method
    assert method["horizontal"]["method_id"] == "horizontal-cell-constant/2"
    assert method["sloping"]["method_id"] == "rasmapper-plain-sloping/1"
    assert method["sloping"]["native_output_compression"] == ["NONE", "LZW"]
    assert method["depth_groups"]["request_schema"] == "ras-commander-depth-groups-request/1"
    assert method["sloping"]["crs_any"] and method["effective_terrain"]["crs_any"]
    with tempfile.TemporaryDirectory(prefix="ras2fim-wheel-") as tmp:
        root = Path(tmp)
        terrain = root / "terrain.tif"
        with rasterio.open(terrain, "w", driver="GTiff", width=8, height=8,
                           count=1, dtype="float32", crs="EPSG:3857",
                           transform=Affine(1, 0, 0, 0, -1, 8), nodata=-9999) as ds:
            ds.write(np.full((8, 8), 100, dtype=np.float32), 1)
        sloping_smoke(engine,root,terrain)
        any_crs_smoke(engine,root)
        native_one_d_smoke(engine,root)
        ownership=np.tile(np.array([1,1,1,2,2,0],dtype="<u4"),(4,1))
        ownership.tofile(root/"owner.bin")
        np.array([100.5,102.],dtype="<f4").tofile(root/"a.bin")
        np.array([103.,np.nan],dtype="<f4").tofile(root/"b.bin")
        request={"schema":"ras2fim-horizontal-request/1","terrain":str(terrain),"epsg":3857,
                 "crs_identity":{"kind":"epsg"},"terrain_transform":[0.,1.,0.,8.,0.,-1.],
                 "window":[1,2,6,4],"owner_file":"owner.bin","cell_count":2,
                 "profiles":[{"id":"a","values_file":"a.bin"},{"id":"b","values_file":"b.bin"}],
                 "min_depth":0.5,"threads":2,"max_working_bytes":256<<20}
        (root/"horizontal.json").write_text(json.dumps(request))
        subprocess.run([engine,"horizontal",str(root/"horizontal.json"),str(root/"horizontal")],check=True)
        report=json.loads((root/"horizontal/report.json").read_text())
        assert report["status"]=="complete" and len(report["profiles"])==2
        for name,values in [("a",np.array([np.nan,100.5,102.],dtype="float32")),
                            ("b",np.array([np.nan,103.,np.nan],dtype="float32"))]:
            expected=values[ownership]-np.float32(100.)
            expected[~(expected>0.5)]=np.nan
            actual=np.fromfile(root/"horizontal"/name/"depth.bin",dtype="<f4").reshape(4,6)
            np.testing.assert_array_equal(actual,expected)
        request["stream_outputs"]=True
        (root/"stream.json").write_text(json.dumps(request))
        streamed=subprocess.run([engine,"horizontal",str(root/"stream.json"),str(root/"stream")],check=True,capture_output=True).stdout
        import io
        pipe=io.BytesIO(streamed)
        for name in ("a","b"):
            header=json.loads(pipe.readline())
            assert header=={"id":name,"status":"complete","pixels":24}
            for product in ("depth","wse"):
                data=np.frombuffer(pipe.read(24*4),dtype="<f4")
                expected=np.fromfile(root/"horizontal"/name/f"{product}.bin",dtype="<f4")
                np.testing.assert_array_equal(data.view("<u4"),expected.view("<u4"))
        assert not pipe.read()
        assert not list((root/"stream").rglob("*.bin"))
        inputs = root / "inputs"
        inputs.mkdir()
        samples = np.array([[0., 0., 105.], [7., 0., 105.],
                            [0., 7., 105.], [7., 7., 105.]])
        tri = Delaunay(samples[:, :2])
        arrays = {"samples": (samples, "<f8"), "simplices": (tri.simplices, "<u4"),
                  "transform": (tri.transform, "<f8"), "neighbors": (tri.neighbors, "<i4"),
                  "equations": (tri.equations, "<f8"),
                  "x_axis": (np.arange(1., 4.), "<f8"),
                  "y_axis": (np.arange(1., 4.), "<f8"),
                  "wet_mask": (np.ones((3, 3)), "u1")}
        files = {}
        for name, (values, dtype) in arrays.items():
            arr = np.ascontiguousarray(values, dtype=dtype)
            filename = name + ".bin"
            arr.tofile(inputs / filename)
            files[name] = {"file": filename, "dtype": arr.dtype.str, "shape": list(arr.shape)}
        meta = {"schema": "ras2fim-step05-inputs/1", "rows": 3, "cols": 3,
                "raster_affine": {"x0": 1., "y0": 1., "res_x": 1., "res_y": 1.},
                "delaunay": {"paraboloid_scale": tri.paraboloid_scale,
                             "paraboloid_shift": tri.paraboloid_shift,
                             "min_bound": tri.min_bound.tolist(),
                             "max_bound": tri.max_bound.tolist(), "nsimplex": tri.nsimplex},
                "files": files}
        (inputs / "inputs.json").write_text(json.dumps(meta))
        mask = root / "mask.u8"
        mask.write_bytes(bytes([1, 0, 1, 1]))
        aois = [{"id": "full"}, {"id": "bbox", "window": [1, 1, 2, 2]},
                {"id": "polygon", "window": [1, 1, 2, 2], "mask_u8": str(mask)}]
        request = {"schema": "ras2fim-step05-request/1",
                   "terrain": {"path": str(terrain), "epsg": 3857,
                               "crs_identity": {"kind": "epsg"}},
                   "source_crs": {"kind": "terrain"},
                   "budgets": {"threads": 2, "max_cache_bytes": 4 << 20,
                               "max_profile_bytes": 128 << 20, "max_inflight_profiles": 2},
                   "interpolation": {"selection": "auto"},
                   "nudge": {"dst_epsg": 3857, "error_threshold": 0.125,
                             "memory_limit_bytes": 67108864., "method": "lookup"},
                   "profiles": [{"id": name, "inputs_dir": str(inputs),
                                 "dest_grid": {"geotransform": [1., 1., 0., 4., 0., -1.],
                                               "width": 3, "height": 3}, "aois": aois}
                                for name in ["wsel_a", "wsel_b"]]}
        rp = root / "request.json"
        rp.write_text(json.dumps(request))
        out = root / "out"
        result = subprocess.run([engine, "step05", str(rp), str(out)])
        if result.returncode:
            raise AssertionError((out / "report.json").read_text())
        report = json.loads((out / "report.json").read_text())
        assert report["status"] == "complete", report
        for name in ["wsel_a", "wsel_b"]:
            for product, expected in [("depth.f32", 5.), ("wse.f32", 105.)]:
                base = out / "units" / name
                full = np.fromfile(base / "full" / product, dtype="<f4").reshape(3, 3)
                np.testing.assert_array_equal(full, np.full((3, 3), expected))
                for aoi in ["bbox", "polygon"]:
                    got = np.fromfile(base / aoi / product, dtype="<f4").reshape(2, 2)
                    want = full[1:3, 1:3].copy()
                    if aoi == "polygon":
                        want[0, 1] = np.nan
                    np.testing.assert_array_equal(got, want)
        print("Installed wheel: Step05 shared-terrain batch, bbox and polygon mask PASS")


if __name__ == "__main__":
    main()
