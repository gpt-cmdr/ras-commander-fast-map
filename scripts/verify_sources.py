"""Verify the distribution source and extraction's immutable numerical files."""
from pathlib import Path
import argparse
import hashlib
import json

root = Path(__file__).resolve().parents[1]


def digest(path):
    return hashlib.sha256(path.read_bytes().replace(b"\r\n", b"\n")).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--write", action="store_true")
    args = parser.parse_args()
    paths = [root / name for name in ("Cargo.toml", "Cargo.lock", "pyproject.toml", "build.rs", "README.md", "LICENSE", "CHANGELOG.md", "MIGRATION.json", "THIRD_PARTY_NOTICES.md")]
    for folder in ("src", "examples", "tests", "licenses"):
        paths.extend(path for path in (root / folder).rglob("*") if path.is_file())
    files = {path.relative_to(root).as_posix(): digest(path) for path in sorted(paths)}
    target = root / "SOURCE_MANIFEST.json"
    if args.write:
        target.write_text(json.dumps({"schema": "ras-commander-fast-map-source/1", "files": files}, indent=2) + "\n", encoding="utf-8")
    else:
        assert json.loads(target.read_text())["files"] == files, "Source manifest is stale"
    migration = json.loads((root / "MIGRATION.json").read_text())
    for name, expected in migration["unchanged_source_sha256"].items():
        assert digest(root / name) == expected, f"Extraction changed numerical source: {name}"
    print(f"Verified {len(files)} distribution sources and {len(migration['unchanged_source_sha256'])} unchanged numerical modules")


if __name__ == "__main__":
    main()
