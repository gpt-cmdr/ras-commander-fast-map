"""Synthetic tests for the external parity checker; no native parity claim."""
import tempfile
import unittest
from pathlib import Path

import numpy as np
import rasterio
from rasterio.transform import from_origin

from qualify_storage_areas import compare


class ParityTests(unittest.TestCase):
    def test_bits_masks_and_metadata(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)

            def write(name, values, transform=from_origin(0, 2, 1, 1)):
                path = root / name
                with rasterio.open(path, "w", driver="GTiff", width=2, height=2,
                                   count=1, dtype="float32", nodata=np.nan,
                                   crs="EPSG:26915", transform=transform) as dst:
                    dst.write(np.asarray(values, dtype=np.float32).reshape(2, 2), 1)
                return path

            ref = write("ref.tif", [0, 1, 2, np.nan])
            same = write("same.tif", [0, 1, 2, np.nan])
            self.assertTrue(compare(ref, same)["passed"])
            bits = write("bits.tif", [-0.0, 1, 2, np.nan])
            self.assertEqual(compare(ref, bits)["value_bit_differences"], 1)
            mask = write("mask.tif", [0, 1, np.nan, 3])
            self.assertEqual(compare(ref, mask)["mask_differences"], 2)
            shifted = write("shift.tif", [0, 1, 2, np.nan], from_origin(1, 2, 1, 1))
            with self.assertRaisesRegex(ValueError, "transform"):
                compare(ref, shifted)


if __name__ == "__main__":
    unittest.main()
