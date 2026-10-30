"""Qualify `ras2fim-batch effective-terrain` against a RAS Mapper terrain export.

Usage: python scripts/qualify_effective_terrain.py <ras2fim-batch> <terrain.hdf> <export.tif> <new-work-dir> [threads]

Stages the terrain HDF (priority from `Priority` attributes, else the terrain VRT order;
level-0 masks; stitch TIN) the way ras-commander would, runs the engine, and reports valid-mask
and float32 value differences against the export from
`RasTerrain.export_rasmapper_terrain(downsample_factor=1, rasterize_modifications=False)`.
Needs h5py, numpy, rasterio and psutil."""
import json, subprocess, sys, time, shutil, xml.etree.ElementTree as ET
from pathlib import Path
import h5py, numpy as np, rasterio, psutil

def priority_order(hdf: Path):
    with h5py.File(hdf, 'r') as f:
        g = f['Terrain']
        recs = {k: g[k] for k in g if isinstance(g[k], h5py.Group) and 'File' in g[k].attrs}
        files = {k: recs[k].attrs['File'].decode() if isinstance(recs[k].attrs['File'], bytes) else str(recs[k].attrs['File']) for k in recs}
        pri = {k: recs[k].attrs.get('Priority') for k in recs}
        mods = list(f['Modifications'].keys()) if 'Modifications' in f else []
    if all(v is not None for v in pri.values()):
        order = sorted(recs, key=lambda k: int(pri[k]))
    else:
        vrt = [e.text for e in ET.parse(hdf.with_suffix('.vrt')).getroot().iter('SourceFilename')]
        order = sorted(recs, key=lambda k: vrt.index(files[k]))
    return [(k, files[k]) for k in order], mods

def stage(hdf: Path, out: Path, engine: str, threads=4):
    out.mkdir(parents=True, exist_ok=True)
    order, mods = priority_order(hdf)
    sources = []
    with h5py.File(hdf, 'r') as f:
        for i, (k, fn) in enumerate(order):
            m = f[f'Terrain/{k}/0/Mask'][:]
            (out / f'mask{i}.u1').write_bytes(np.ascontiguousarray(m, np.uint8).tobytes())
            sources.append({'path': str((hdf.parent / fn).resolve()), 'mask': {'path': f'mask{i}.u1', 'dtype': '|u1', 'shape': list(m.shape)}})
        P = f['Terrain/Stitch TIN Points'][:]; T = f['Terrain/Stitch TIN Triangles'][:]
    tin = None
    if P.size and T.size:
        (out / 'tin_points.f8').write_bytes(np.ascontiguousarray(P, '<f8').tobytes())
        (out / 'tin_triangles.i4').write_bytes(np.ascontiguousarray(T, '<i4').tobytes())
        tin = {'points': {'path': 'tin_points.f8', 'dtype': '<f8', 'shape': list(P.shape)},
               'triangles': {'path': 'tin_triangles.i4', 'dtype': '<i4', 'shape': list(T.shape)}}
    info = json.loads(subprocess.run([engine, 'terrain-info', sources[0]['path']], capture_output=True, text=True, check=True).stdout)
    with rasterio.open(sources[0]['path']) as d:
        epsg = d.crs.to_epsg() or 0
    ident = {'kind': 'epsg'} if info.get('epsg') else {'kind': 'caller_verified_wkt', 'fnv1a64': info['crs_wkt_fnv1a64']}
    req = {'schema': 'ras-commander-effective-terrain-request/1', 'sources': sources, 'stitch_tin': tin,
           'epsg': int(info.get('epsg') or epsg), 'crs_identity': ident, 'modifications': len(mods),
           'threads': threads, 'max_working_bytes': 4 << 30}
    (out / 'request.json').write_text(json.dumps(req, indent=1))
    return out / 'request.json'

def run(engine, req, outdir):
    if outdir.exists(): shutil.rmtree(outdir)
    t = time.time()
    p = psutil.Popen([engine, 'effective-terrain', str(req), str(outdir)], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    peak = 0
    while p.poll() is None:
        try: peak = max(peak, p.memory_info().peak_wset if hasattr(p.memory_info(), 'peak_wset') else p.memory_info().rss)
        except psutil.Error: pass
        time.sleep(0.05)
    o, e = p.communicate()
    if p.returncode: raise SystemExit(f'engine failed: {o} {e}')
    return time.time() - t, peak, o

def compare(ours, ref):
    a = rasterio.open(ours); b = rasterio.open(ref)
    assert (a.width, a.height) == (b.width, b.height), ((a.width, a.height), (b.width, b.height))
    ta, tb = a.transform, b.transform
    dt = max(abs(x - y) for x, y in zip(ta[:6], tb[:6]))
    res = dict(size=(a.width, a.height), transform_max_abs_diff=dt, nodata=(a.nodata, b.nodata))
    tot = dict(valid_ref=0, valid_ours=0, mask_diff=0, value_diff=0, max_abs=0.0, le_1_32=0)
    for _, w in a.block_windows(1):
        x = a.read(1, window=w); y = b.read(1, window=w)
        vx = x != a.nodata; vy = y != b.nodata
        tot['valid_ref'] += int(vy.sum()); tot['valid_ours'] += int(vx.sum())
        tot['mask_diff'] += int((vx != vy).sum())
        both = vx & vy; d = np.abs(x[both].astype(np.float64) - y[both])
        nz = d > 0; tot['value_diff'] += int(nz.sum()); tot['le_1_32'] += int((nz & (d <= 1/32)).sum())
        if nz.any(): tot['max_abs'] = max(tot['max_abs'], float(d.max()))
    res.update(tot); return res

if __name__ == '__main__':
    engine, hdf, ref, work = sys.argv[1], Path(sys.argv[2]), sys.argv[3], Path(sys.argv[4])
    threads = int(sys.argv[5]) if len(sys.argv) > 5 else 4
    req = stage(hdf.resolve(), work / 'stage', engine, threads)
    secs, peak, o = run(engine, req, work / 'out')
    r = compare(work / 'out' / 'effective_terrain.tif', ref)
    r.update(seconds=round(secs, 2), peak_working_set_mib=round(peak / 2**20, 1), engine_stdout=o.strip())
    print(json.dumps(r))
