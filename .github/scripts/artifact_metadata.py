"""Record artifact identities without modifying qualified run output."""
from pathlib import Path
import hashlib
import json
import os
import platform
import subprocess

folder = Path("dist")
artifacts = {p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in sorted(folder.glob("*")) if p.suffix in (".whl", ".gz", ".crate")}
result = dict(schema="ras-commander-fast-map-artifacts/1", source_commit=os.environ["FAST_MAP_BUILD_COMMIT"], artifact_sha256=artifacts)
engine = subprocess.run(["ras2fim-batch", "method"], check=True, text=True, capture_output=True)
result["engine"] = json.loads(engine.stdout)["engine"]
assert result["engine"]["source_commit"] == result["source_commit"]
(folder / f"BUILD-{platform.system().lower()}.json").write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
