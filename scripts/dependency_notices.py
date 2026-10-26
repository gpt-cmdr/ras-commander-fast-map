"""Collect source-distributed dependency licenses without embedding local paths."""
from pathlib import Path
import argparse
import json
import shutil
import subprocess

root = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--locked", "--format-version", "1"], cwd=root, text=True
    ))
    folder = root / "licenses/dependencies"
    rows = ["# Resolved Rust dependency notices", "", "| Package | Version | SPDX expression |", "|---|---|---|"]
    for package in sorted(metadata["packages"], key=lambda p: p["name"]):
        if package.get("source") is None:
            continue
        source = Path(package["manifest_path"]).parent.resolve()
        paths = {p for p in source.iterdir() if p.is_file() and p.name.lower().startswith(("license", "licence", "copying", "notice"))}
        if package.get("license_file"):
            path = (source / package["license_file"]).resolve()
            assert path.is_relative_to(source)
            paths.add(path)
        if not paths:
            paths = {p for p in (root / "licenses/upstream" / f"{package['name']}-{package['version']}").glob("*") if p.is_file()}
        assert paths, f"Review missing license text for {package['name']}"
        destination = folder / f"{package['name']}-{package['version']}"
        rows.append(f"| {package['name']} | {package['version']} | {package.get('license') or 'see notice'} |")
        for path in sorted(paths):
            target = destination / path.name
            if args.check:
                assert target.read_bytes().replace(b"\r\n", b"\n") == path.read_bytes().replace(b"\r\n", b"\n"), str(target)
            else:
                destination.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(path, target)
    content = "\n".join(rows) + "\n"
    target = folder / "INDEX.md"
    if args.check:
        assert target.read_text(encoding="utf-8") == content
    else:
        target.write_text(content, encoding="utf-8", newline="\n")
    print(f"Dependency notices {'verified' if args.check else 'collected'}")


if __name__ == "__main__":
    main()
