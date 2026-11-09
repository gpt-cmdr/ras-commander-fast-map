"""Synthetic wheel/graph invariants for the pre-publication SBOM repair."""
from __future__ import annotations

import io
import json
from pathlib import Path
import struct
import tempfile
import unittest
import zipfile

import sanitize_wheel_sbom as repair

DIST = "ras_commander_fast_map-0.14.0.dist-info"
SBOM = DIST + "/sboms/ras-commander-fast-map.cyclonedx.json"
RECORD = DIST + "/RECORD"
OLD = "path+file:///workspace/synthetic-checkout#ras-commander-fast-map@0.14.0"
BASE = "pkg:cargo/ras-commander-fast-map@0.14.0"


def document():
    return {
        "bomFormat": "CycloneDX", "specVersion": "1.5", "version": 1,
        "serialNumber": "urn:uuid:00000000-0000-0000-0000-000000000001",
        "metadata": {"timestamp": "2026-01-01T00:00:00Z", "component": {
            "type": "application", "name": repair.PROJECT, "version": "0.14.0", "bom-ref": OLD,
            "purl": BASE + "?download_url=file://.", "licenses": [{"expression": "MIT AND BSL-1.0"}],
            "components": [
                {"type": "library", "name": "ras_commander_fast_map", "version": "0.14.0",
                 "bom-ref": OLD + " bin-target-0", "purl": BASE + "?download_url=file://.#src/lib.rs"},
                {"type": "application", "name": "ras2fim-batch", "version": "0.14.0",
                 "bom-ref": OLD + " bin-target-1", "purl": BASE + "?download_url=file://.#src/main.rs"},
            ]}},
        "components": [{"type": "library", "name": "dependency", "version": "1.0.0", "bom-ref": "registry:dep",
                        "purl": "pkg:cargo/dependency@1.0.0", "hashes": [{"alg": "SHA-256", "content": "ab" * 32}]}],
        "dependencies": [{"ref": OLD, "dependsOn": [OLD + " bin-target-0", "registry:dep"]},
                         {"ref": OLD + " bin-target-0", "dependsOn": ["registry:dep"]},
                         {"ref": OLD + " bin-target-1", "dependsOn": [OLD + " bin-target-0"]},
                         {"ref": "registry:dep", "dependsOn": []}],
    }


class Unseekable(io.BytesIO):
    def seek(self, *args):
        raise io.UnsupportedOperation("streaming fixture")


def wheel(folder, sbom=None, *, record_failure=False, extra=None, streaming=False):
    contents = {
        "ras_commander_fast_map-0.14.0.data/scripts/ras2fim-batch.exe": b"synthetic executable\x00\xff",
        "licenses/upstream.txt": b"Complete upstream notice.\r\n",
        "licenses/unicode-ß.txt": b"Complete second notice.\n",
        DIST + "/METADATA": b"Metadata-Version: 2.4\nName: ras-commander-fast-map\nVersion: 0.14.0\n\n",
        DIST + "/WHEEL": b"Wheel-Version: 1.0\nTag: py3-none-win_amd64\n",
        SBOM: json.dumps(document() if sbom is None else sbom, indent=2).encode(),
    }
    contents.update(extra or {})
    contents[RECORD] = repair.record_bytes(contents, [*contents, RECORD], RECORD)
    if record_failure:
        contents["licenses/upstream.txt"] += b"tampered"
    buffer = Unseekable() if streaming else io.BytesIO()
    with zipfile.ZipFile(buffer, "w") as archive:
        archive.comment = b"archive comment retained"
        for index, (name, data) in enumerate(contents.items()):
            info = zipfile.ZipInfo(name, (2020, 1, 2, 3, 4, index * 2))
            info.compress_type = zipfile.ZIP_DEFLATED
            info.create_system = 3
            info.external_attr = 0o100640 << 16
            info.internal_attr = 1
            info.extra = b"\xfe\xca\x03\x00abc"
            info.comment = b"entry comment"
            archive.writestr(info, data)
    path = folder / "ras_commander_fast_map-0.14.0-py3-none-win_amd64.whl"
    path.write_bytes(buffer.getvalue())
    return path


def compressed(data, info):
    start = info.header_offset
    filename, extra = struct.unpack_from("<HH", data, start + 26)
    offset = start + 30 + filename + extra
    return data[offset:offset + info.compress_size]


class SanitizeWheelTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.addCleanup(self.temp.cleanup)

    def test_full_graph_only_known_identities_change_and_input_is_immutable(self):
        source = wheel(self.root)
        before = source.read_bytes()
        result = repair.sanitize_wheel(source, self.root / "fixed")
        self.assertEqual(source.read_bytes(), before)
        self.assertEqual(set(result["changed_entries"]), {SBOM, RECORD})
        self.assertEqual(result["graph"]["components"], 4)
        self.assertEqual(result["graph"]["dependency_nodes"], 4)
        self.assertEqual(result["graph"]["dependency_edges"], 4)
        with zipfile.ZipFile(result["output"]) as archive:
            after = json.loads(archive.read(SBOM))
            original = document()
            self.assertEqual(after["components"], original["components"])
            self.assertEqual(after["metadata"]["timestamp"], original["metadata"]["timestamp"])
            self.assertFalse(any(repair.local_value(value) for value in repair.strings(after)))
            self.assertEqual(after["metadata"]["component"]["bom-ref"], BASE)
            self.assertEqual(after["dependencies"][0]["ref"], BASE)
            self.assertTrue(after["dependencies"][0]["dependsOn"][0].startswith(BASE + "#target/"))
            repair.validate_record({info.filename: archive.read(info) for info in archive.infolist()}, RECORD)

    def test_zip_attributes_and_unchanged_compressed_streams_are_preserved(self):
        source = wheel(self.root)
        result = repair.sanitize_wheel(source, self.root / "fixed")
        old_bytes, new_bytes = source.read_bytes(), Path(result["output"]).read_bytes()
        with zipfile.ZipFile(source) as old, zipfile.ZipFile(result["output"]) as new:
            self.assertEqual(old.comment, new.comment)
            self.assertEqual([repair.attributes(i) for i in old.infolist()], [repair.attributes(i) for i in new.infolist()])
            for first, second in zip(old.infolist(), new.infolist()):
                if first.filename not in {SBOM, RECORD}:
                    self.assertEqual(old.read(first), new.read(second))
                    self.assertEqual(compressed(old_bytes, first), compressed(new_bytes, second))

    def test_streaming_descriptors_are_preserved_and_updated(self):
        source = wheel(self.root, streaming=True)
        result = repair.sanitize_wheel(source, self.root / "fixed")
        with zipfile.ZipFile(source) as old, zipfile.ZipFile(result["output"]) as new:
            self.assertTrue(all(info.flag_bits & 8 for info in old.infolist()))
            self.assertEqual([repair.attributes(i) for i in old.infolist()], [repair.attributes(i) for i in new.infolist()])
            self.assertIsNone(new.testzip())

    def test_deterministic_and_byte_idempotent(self):
        source = wheel(self.root)
        first = repair.sanitize_wheel(source, self.root / "first")
        second = repair.sanitize_wheel(source, self.root / "second")
        third = repair.sanitize_wheel(first["output"], self.root / "third")
        self.assertEqual(first["output_sha256"], second["output_sha256"])
        self.assertEqual(Path(first["output"]).read_bytes(), Path(third["output"]).read_bytes())
        self.assertEqual(third["changed_entries"], [])

    def assert_rejected(self, value, message):
        source = wheel(self.root, value)
        before = source.read_bytes()
        with self.assertRaisesRegex(repair.SanitizeError, message):
            repair.sanitize_wheel(source, self.root / "fixed")
        self.assertEqual(source.read_bytes(), before)
        self.assertFalse((self.root / "fixed").exists())

    def test_dangling_dependency_ref_is_rejected(self):
        value = document()
        value["dependencies"][0]["dependsOn"].append("not-a-component")
        self.assert_rejected(value, "Dangling")

    def test_canonical_component_collision_is_rejected(self):
        value = document()
        value["components"][0]["bom-ref"] = BASE
        value["dependencies"][3]["ref"] = BASE
        for node in value["dependencies"]:
            node["dependsOn"] = [BASE if item == "registry:dep" else item for item in node["dependsOn"]]
        self.assert_rejected(value, "collision")

    def test_unknown_local_uri_is_rejected_without_dropping_component(self):
        value = document()
        value["components"][0]["purl"] = "file:///workspace/unreviewed-dependency"
        self.assert_rejected(value, "Unknown local")

    def test_unknown_local_target_identity_is_rejected(self):
        value = document()
        value["metadata"]["component"]["components"][0]["bom-ref"] = "file:///other-target"
        value["dependencies"][0]["dependsOn"][0] = "file:///other-target"
        value["dependencies"][1]["ref"] = "file:///other-target"
        value["dependencies"][2]["dependsOn"] = ["file:///other-target"]
        self.assert_rejected(value, "Unknown local target")

    def test_posix_absolute_property_is_rejected(self):
        value = document()
        value["metadata"]["component"]["properties"] = [
            {"name": "build-directory", "value": "/workspace/private-build"}]
        self.assert_rejected(value, "local")

    def test_https_and_relative_fragments_are_not_local_paths(self):
        self.assertFalse(repair.local_value("https://example.org/workspace"))
        self.assertFalse(repair.local_value("src/lib.rs"))
        self.assertFalse(repair.local_value("#src/lib.rs"))
        self.assertTrue(repair.local_value("/workspace/private-build"))

    def test_duplicate_dependency_nodes_are_rejected(self):
        value = document()
        value["dependencies"].append(dict(value["dependencies"][0]))
        self.assert_rejected(value, "Duplicate dependency")

    def test_unknown_spec_version_is_rejected(self):
        value = document()
        value["specVersion"] = "future-version"
        self.assert_rejected(value, "specVersion")

    def test_record_failure_refuses_before_output(self):
        source = wheel(self.root, record_failure=True)
        with self.assertRaisesRegex(repair.SanitizeError, "RECORD hash"):
            repair.sanitize_wheel(source, self.root / "fixed")
        self.assertFalse((self.root / "fixed").exists())

    def test_archive_path_traversal_is_rejected(self):
        source = wheel(self.root, extra={"../escape.txt": b"never extracted"})
        with self.assertRaisesRegex(repair.SanitizeError, "traversal"):
            repair.sanitize_wheel(source, self.root / "fixed")

    def test_input_and_existing_output_are_never_overwritten(self):
        source = wheel(self.root)
        with self.assertRaisesRegex(repair.SanitizeError, "overwrite"):
            repair.sanitize_wheel(source, self.root)
        repair.sanitize_wheel(source, self.root / "fixed")
        with self.assertRaisesRegex(repair.SanitizeError, "overwrite"):
            repair.sanitize_wheel(source, self.root / "fixed")

    def test_duplicate_json_key_and_nonfinite_are_rejected(self):
        with self.assertRaisesRegex(repair.SanitizeError, "Duplicate JSON"):
            repair.json_document(b'{"components":[],"components":[]}')
        with self.assertRaisesRegex(repair.SanitizeError, "Nonfinite"):
            repair.json_document(b'{"value":NaN}')


if __name__ == "__main__":
    unittest.main()
