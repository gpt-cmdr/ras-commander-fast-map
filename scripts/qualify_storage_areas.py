"""Compare externally supplied native and Fast-Map Float32 GeoTIFF products.

Usage: python scripts/qualify_storage_areas.py external-manifest.json report.json
Requires numpy and rasterio in the qualification environment, not the Rust engine.
This checks parity only; timing evidence must come from supervised benchmark trials.
"""
import argparse
import hashlib
import json
from pathlib import Path

import numpy as np
import rasterio


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def compare(reference, candidate):
    with rasterio.open(reference) as ref, rasterio.open(candidate) as got:
        if (ref.count != 1 or got.count != 1
                or ref.dtypes != ("float32",) or got.dtypes != ("float32",)):
            raise ValueError("Products must be single-band Float32 GeoTIFFs")
        if (ref.shape != got.shape or ref.transform != got.transform
                or ref.crs is None or ref.crs != got.crs):
            raise ValueError("Shape, transform or CRS mismatch")
        mask_differences = value_differences = valid_pixels = 0
        for _, window in ref.block_windows(1):
            a = ref.read(1, window=window)
            b = got.read(1, window=window)
            am = ref.read_masks(1, window=window) != 0
            bm = got.read_masks(1, window=window) != 0
            if np.any(am & ~np.isfinite(a)) or np.any(bm & ~np.isfinite(b)):
                raise ValueError("Nonfinite value marked valid")
            mask_differences += int(np.count_nonzero(am != bm))
            valid_pixels += int(np.count_nonzero(am))
            value_differences += int(np.count_nonzero(
                (am & bm) & (a.view(np.uint32) != b.view(np.uint32))))
        return dict(valid_pixels=valid_pixels, mask_differences=mask_differences,
                    value_bit_differences=value_differences,
                    passed=mask_differences == 0 and value_differences == 0)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("manifest", type=Path)
    parser.add_argument("report", type=Path)
    args = parser.parse_args()
    manifest = json.loads(args.manifest.read_text(encoding="utf-8"))
    if manifest.get("schema") != "fast-map-storage-qualification/1":
        parser.error("Unsupported manifest schema")
    required = ("fixture", "engine_revision", "native_version", "options", "products")
    if any(not manifest.get(key) for key in required):
        parser.error("Manifest requires nonempty " + ", ".join(required))
    results = []
    for product in manifest["products"]:
        record = {"id": product.get("id", "unknown")}
        try:
            reference = args.manifest.parent / product["reference"]
            candidate = args.manifest.parent / product["candidate"]
            record.update(reference_sha256=sha256(reference),
                          candidate_sha256=sha256(candidate))
            record.update(compare(reference, candidate))
        except Exception as error:
            record.update(passed=False, error=str(error))
        results.append(record)
    report = dict(schema="fast-map-storage-qualification-report/1",
                  provenance=manifest, products=results,
                  passed=bool(results) and all(row["passed"] for row in results))
    with args.report.open("x", encoding="utf-8") as stream:
        json.dump(report, stream, indent=2)
        stream.write("\n")
    print(f"{len(results)} products; passed={report['passed']}")
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
