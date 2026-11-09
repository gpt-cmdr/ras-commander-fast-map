"""Canonicalize wheel SBOM workspace identities before installation/publication.

Only the Rust workspace component/target identities and their graph references
may change. Executable, notices and every other payload remain byte-identical.
Unchanged ZIP records retain even their original compressed bytes/attributes.
This deliberately supports bounded, unsigned ZIP32 wheels; it fails closed on
unknown local identities rather than dropping provenance.
"""
from __future__ import annotations

import argparse
import base64
import copy
import csv
from email.parser import BytesParser
import hashlib
import io
import json
from pathlib import Path, PurePosixPath
import re
import stat
import struct
from urllib.parse import quote, urlsplit
import zipfile
import zlib

MAX_WHEEL_BYTES = 256 * 1024**2
MAX_UNCOMPRESSED_BYTES = 512 * 1024**2
MAX_SBOM_BYTES = 16 * 1024**2
PROJECT = "ras-commander-fast-map"


class SanitizeError(ValueError):
    """An artifact failed a required integrity/privacy invariant."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise SanitizeError(message)


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def local_value(value: str) -> bool:
    return bool(re.search(r"(?i)(?:path\+)?file://|(?<![a-z])[a-z]:[/\\]|^\\\\|^/", value))


def strings(value):
    if isinstance(value, str):
        yield value
    elif isinstance(value, dict):
        for key, child in value.items():
            yield key
            yield from strings(child)
    elif isinstance(value, list):
        for child in value:
            yield from strings(child)


def json_document(data):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            require(key not in result, "Duplicate JSON key")
            result[key] = value
        return result
    def constant(_):
        raise SanitizeError("Nonfinite JSON constant")
    return json.loads(data, object_pairs_hook=unique, parse_constant=constant)


def components(document):
    def descend(component):
        require(isinstance(component, dict), "Invalid component")
        yield component
        for child in component.get("components", []):
            yield from descend(child)
    root = document.get("metadata", {}).get("component")
    if root is not None:
        yield from descend(root)
    for component in document.get("components", []):
        yield from descend(component)


def validate_graph(document):
    refs = [component["bom-ref"] for component in components(document) if "bom-ref" in component]
    require(all(isinstance(ref, str) and ref for ref in refs), "Invalid bom-ref")
    require(len(refs) == len(set(refs)), "Component bom-ref collision")
    known = set(refs)
    edges = []
    nodes = set()
    for node in document.get("dependencies", []):
        require(isinstance(node, dict) and node.get("ref") in known, "Dangling dependency ref")
        require(node["ref"] not in nodes, "Duplicate dependency node ref")
        nodes.add(node["ref"])
        targets = node.get("dependsOn", [])
        require(isinstance(targets, list) and all(target in known for target in targets), "Dangling dependsOn ref")
        edges.append((node["ref"], tuple(targets)))
    return refs, edges


def canonical_purl(value, base):
    require(isinstance(value, str), "Missing workspace purl")
    old = base + "?download_url=file://."
    if value == old or value == base:
        return base
    for prefix in (old + "#", base + "#"):
        if value.startswith(prefix):
            fragment = value[len(prefix):]
            require(fragment and not local_value(fragment) and "\\" not in fragment, "Unknown target purl")
            require(not PurePosixPath(fragment).is_absolute() and ".." not in fragment.split("/"), "Unsafe target purl")
            return base + "#" + fragment
    raise SanitizeError("Unknown workspace purl")


def sanitize_document(document, package, version):
    require(document.get("bomFormat") == "CycloneDX", "Expected CycloneDX Rust SBOM")
    require(document.get("specVersion") == "1.5", "Unsupported CycloneDX specVersion")
    before_refs, before_edges = validate_graph(document)
    result = copy.deepcopy(document)
    root = result.get("metadata", {}).get("component")
    require(isinstance(root, dict) and root.get("name") == package and root.get("version") == version,
            "SBOM workspace does not match wheel name/version")
    base = f"pkg:cargo/{quote(package, safe='.-_')}@{quote(version, safe='.+-')}"
    original_root = root.get("bom-ref")
    require(isinstance(original_root, str), "Missing workspace bom-ref")
    if original_root != base:
        require(original_root.startswith("path+file://"), "Unknown workspace identity")
        uri = urlsplit(original_root[len("path+"):])
        require(uri.scheme == "file" and uri.path and not uri.query and uri.fragment == f"{package}@{version}",
                "Unknown local workspace identity")
    mapping = {original_root: base}
    root["bom-ref"] = base
    root["purl"] = canonical_purl(root.get("purl"), base)
    for child in root.get("components", []):
        name, kind = child.get("name"), child.get("type")
        require(isinstance(name, str) and re.fullmatch(r"[A-Za-z0-9_.-]+", name) and kind in ("library", "application"),
                "Unknown workspace target")
        require(child.get("version") == version and not child.get("components"), "Unknown nested workspace target")
        canonical = f"{base}#target/{kind}/{name}"
        original = child.get("bom-ref")
        require(isinstance(original, str) and
                (original == canonical or re.fullmatch(re.escape(original_root) + r" bin-target-\d+", original)),
                "Unknown local target identity")
        require(original not in mapping, "Workspace identity collision")
        mapping[original] = canonical
        child["bom-ref"] = canonical
        child["purl"] = canonical_purl(child.get("purl"), base)
    require(len(set(mapping.values())) == len(mapping), "Canonical target collision")
    for node in result.get("dependencies", []):
        node["ref"] = mapping.get(node["ref"], node["ref"])
        if "dependsOn" in node:
            node["dependsOn"] = [mapping.get(value, value) for value in node["dependsOn"]]
    after_refs, after_edges = validate_graph(result)
    require(after_refs == [mapping.get(ref, ref) for ref in before_refs], "Component inventory changed")
    require(after_edges == [(mapping.get(ref, ref), tuple(mapping.get(target, target) for target in targets))
                            for ref, targets in before_edges], "Dependency graph changed")
    require(not any(local_value(value) for value in strings(result)), "Unknown local URI/path remains in SBOM")
    return result, {"components": len(list(components(result))), "dependency_nodes": len(after_edges),
                    "dependency_edges": sum(len(targets) for _, targets in after_edges), "identities": mapping}


def validate_names(infos):
    names = [info.filename for info in infos]
    require(len(names) <= 4096 and len(names) == len(set(names)), "Duplicate/excessive ZIP members")
    for info in infos:
        name = info.filename
        require(name and "\\" not in name and not re.match(r"^[A-Za-z]:", name)
                and not PurePosixPath(name).is_absolute() and not any(part in ("", ".", "..") for part in name.rstrip("/").split("/")),
                "Archive path traversal/noncanonical path")
        require(not info.flag_bits & 1 and info.volume == 0, "Encrypted/multidisk wheel unsupported")
        require(info.compress_type in (zipfile.ZIP_STORED, zipfile.ZIP_DEFLATED), "Unsupported wheel compression")
        require(stat.S_IFMT(info.external_attr >> 16) != stat.S_IFLNK, "Wheel symlink unsupported")
    require(sum(info.file_size for info in infos) <= MAX_UNCOMPRESSED_BYTES, "Uncompressed wheel budget exceeded")


def validate_record(payloads, record_name):
    try:
        rows = list(csv.reader(io.StringIO(payloads[record_name].decode("utf-8"))))
        require(all(len(row) == 3 for row in rows), "Malformed RECORD row")
        names = [row[0] for row in rows]
        require(len(names) == len(set(names)), "Duplicate RECORD row")
        required = {name for name in payloads if not name.endswith("/")}
        require(required <= set(names) <= set(payloads), "RECORD member coverage failure")
        for name, value, size in rows:
            if name == record_name:
                require(not value and not size, "RECORD cannot hash itself")
                continue
            algorithm, expected = value.split("=", 1)
            require(algorithm in ("sha256", "sha384", "sha512"), "Unsupported RECORD hash")
            actual = base64.urlsafe_b64encode(hashlib.new(algorithm, payloads[name]).digest()).decode().rstrip("=")
            require(actual == expected and int(size) == len(payloads[name]), "RECORD hash/size failure")
    except (UnicodeError, KeyError, TypeError, ValueError) as error:
        if isinstance(error, SanitizeError):
            raise
        raise SanitizeError("Invalid RECORD") from error


def record_bytes(payloads, names, record_name):
    output = io.StringIO(newline="")
    writer = csv.writer(output, lineterminator="\n")
    for name in names:
        if name.endswith("/"):
            continue
        if name == record_name:
            writer.writerow((name, "", ""))
        else:
            data = payloads[name]
            value = base64.urlsafe_b64encode(hashlib.sha256(data).digest()).decode().rstrip("=")
            writer.writerow((name, "sha256=" + value, len(data)))
    return output.getvalue().encode()


def rewrite_zip32(data, infos, changes):
    # Preserve raw local records and central attributes. Only changed payloads,
    # their CRC/sizes, and mandatory container offsets may differ.
    end = data.rfind(b"PK\x05\x06", max(0, len(data) - 65557))
    require(end >= 0, "Missing ZIP end record")
    _, disk, central_disk, disk_count, count, central_size, central, comment_size = struct.unpack_from("<4s4H2IH", data, end)
    require(disk == central_disk == 0 and disk_count == count == len(infos)
            and count < 65535 and central_size != 0xFFFFFFFF and central != 0xFFFFFFFF
            and central + central_size == end and end + 22 + comment_size == len(data), "ZIP64/multidisk/trailing archive unsupported")
    records = []
    cursor = central
    for info in infos:
        require(data[cursor:cursor+4] == b"PK\x01\x02", "Invalid ZIP central record")
        filename, extra, comment = struct.unpack_from("<HHH", data, cursor + 28)
        length = 46 + filename + extra + comment
        records.append((info, bytearray(data[cursor:cursor+length])))
        cursor += length
    require(cursor == end, "Unknown ZIP central records")
    local_order = sorted(infos, key=lambda info: info.header_offset)
    require(not local_order or local_order[0].header_offset == 0, "Prefixed ZIP unsupported")
    output = bytearray()
    offsets, rewritten = {}, {}
    for index, info in enumerate(local_order):
        start = info.header_offset
        stop = local_order[index+1].header_offset if index+1 < len(local_order) else central
        require(data[start:start+4] == b"PK\x03\x04", "Invalid local ZIP header")
        filename, extra = struct.unpack_from("<HH", data, start + 26)
        require(struct.unpack_from("<H", data, start+4)[0] < 45 and
                all(size != 0xFFFFFFFF for size in struct.unpack_from("<II", data, start+18)), "ZIP64 local record unsupported")
        body = start + 30 + filename + extra
        trailer = data[body + info.compress_size:stop]
        require(body + info.compress_size <= stop, "Overlapping ZIP records")
        offsets[info.filename] = len(output)
        if info.filename not in changes:
            output.extend(data[start:stop])
            continue
        payload = changes[info.filename]
        if info.compress_type == zipfile.ZIP_STORED:
            compressed = payload
        elif info.compress_type == zipfile.ZIP_DEFLATED:
            compressor = zlib.compressobj(9, zlib.DEFLATED, -15)
            compressed = compressor.compress(payload) + compressor.flush()
        else:
            raise SanitizeError("Unsupported modified-entry compression")
        crc = zlib.crc32(payload) & 0xFFFFFFFF
        header = bytearray(data[start:body])
        require(len(payload) < 0xFFFFFFFF and len(compressed) < 0xFFFFFFFF, "ZIP32 size exceeded")
        if info.flag_bits & 8:
            require(len(trailer) in (12, 16), "Unsupported ZIP descriptor")
            if len(trailer) == 16:
                require(trailer[:4] == b"PK\x07\x08", "Invalid ZIP descriptor")
            trailer = (b"PK\x07\x08" if len(trailer) == 16 else b"") + struct.pack("<III", crc, len(compressed), len(payload))
            if any(header[14:26]):
                struct.pack_into("<III", header, 14, crc, len(compressed), len(payload))
        else:
            require(not trailer, "Unknown modified-entry ZIP trailer")
            struct.pack_into("<III", header, 14, crc, len(compressed), len(payload))
        output.extend(header)
        output.extend(compressed)
        output.extend(trailer)
        rewritten[info.filename] = (crc, len(compressed), len(payload))
    new_central = len(output)
    for info, record in records:
        if info.filename in rewritten:
            struct.pack_into("<III", record, 16, *rewritten[info.filename])
        struct.pack_into("<I", record, 42, offsets[info.filename])
        output.extend(record)
    tail = bytearray(data[end:])
    struct.pack_into("<II", tail, 12, len(output)-new_central, new_central)
    output.extend(tail)
    return bytes(output)


def attributes(info):
    return tuple(getattr(info, name) for name in ("filename", "date_time", "compress_type", "comment", "extra",
                 "create_system", "create_version", "extract_version", "reserved", "flag_bits", "volume", "internal_attr", "external_attr"))


def sanitize_wheel(input_path, output_dir):
    source = Path(input_path).resolve(strict=True)
    require(source.stat().st_size <= MAX_WHEEL_BYTES, "Wheel input budget exceeded")
    destination = Path(output_dir).resolve() / source.name
    require(source != destination and not destination.exists(), "Refusing to overwrite an input/existing wheel")
    original = source.read_bytes()
    with zipfile.ZipFile(io.BytesIO(original)) as archive:
        infos = archive.infolist()
        validate_names(infos)
        require(not any(info.filename.endswith(("/RECORD.jws", "/RECORD.p7s")) for info in infos), "Sanitize before wheel signing")
        payloads = {info.filename: archive.read(info) for info in infos}
    record_names = [name for name in payloads if name.endswith(".dist-info/RECORD")]
    require(len(record_names) == 1, "Expected one wheel RECORD")
    record_name = record_names[0]
    prefix = record_name.rsplit("/", 1)[0]
    require(prefix + "/METADATA" in payloads, "Missing wheel metadata")
    metadata = BytesParser().parsebytes(payloads[prefix + "/METADATA"])
    package, version = metadata.get("Name"), metadata.get("Version")
    require(package == PROJECT and isinstance(version, str) and re.fullmatch(r"[A-Za-z0-9.+-]+", version), "Unexpected wheel project/version")
    validate_record(payloads, record_name)
    rust_name = f"{prefix}/sboms/{PROJECT}.cyclonedx.json"
    require(rust_name in payloads and len(payloads[rust_name]) <= MAX_SBOM_BYTES, "Missing/oversized Rust SBOM")
    try:
        document = json_document(payloads[rust_name])
    except (UnicodeError, ValueError) as error:
        raise SanitizeError("Invalid Rust SBOM JSON") from error
    sanitized, graph = sanitize_document(document, package, version)
    updated = dict(payloads)
    updated[rust_name] = (json.dumps(sanitized, ensure_ascii=False, sort_keys=True, separators=(",", ":")) + "\n").encode()
    for name, data in updated.items():
        if name.startswith(prefix + "/sboms/") and name != rust_name:
            require(len(data) <= MAX_SBOM_BYTES, "Oversized additional SBOM")
            values = strings(json_document(data)) if name.endswith(".json") else (data.decode("utf-8"),)
            require(not any(local_value(value) for value in values), "Unknown local URI in additional SBOM")
    updated[record_name] = record_bytes(updated, [info.filename for info in infos], record_name)
    changes = {name: data for name, data in updated.items() if data != payloads[name]}
    require(set(changes) <= {rust_name, record_name}, "Unexpected wheel mutation")
    output = rewrite_zip32(original, infos, changes)
    with zipfile.ZipFile(io.BytesIO(output)) as archive:
        require([attributes(info) for info in archive.infolist()] == [attributes(info) for info in infos], "ZIP attributes changed")
        verified = {info.filename: archive.read(info) for info in archive.infolist()}
    require(verified == updated, "Output payload verification failed")
    validate_record(verified, record_name)
    require(all(verified[name] == data for name, data in payloads.items() if name not in {rust_name, record_name}), "Executable/notices/other bytes changed")
    require(source.read_bytes() == original, "Input wheel changed during sanitization")
    destination.parent.mkdir(parents=True, exist_ok=True)
    created = False
    try:
        with destination.open("xb") as stream:
            created = True
            stream.write(output)
        require(destination.read_bytes() == output, "Written artifact differs")
    except BaseException:
        if created:
            destination.unlink(missing_ok=True)
        raise
    return {"input": str(source), "output": str(destination), "input_sha256": digest(original), "output_sha256": digest(output),
            "changed_entries": sorted(changes), "graph": graph, "record_verified": True, "zip_attributes_preserved": True,
            "unchanged_payload_sha256": {name: digest(data) for name, data in payloads.items() if name not in {rust_name, record_name}}}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input-dir", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    args = parser.parse_args()
    wheels = sorted(args.input_dir.glob("*.whl"))
    require(wheels, "No input wheels")
    for wheel in wheels:
        result = sanitize_wheel(wheel, args.output_dir)
        print(json.dumps({key: result[key] for key in ("input", "output", "input_sha256", "output_sha256", "changed_entries", "graph")}, sort_keys=True))


if __name__ == "__main__":
    main()
