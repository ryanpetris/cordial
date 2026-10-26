"""Keep Docker source timestamps stable while file contents and modes are unchanged."""
import hashlib
import json
import os
from pathlib import Path
import sys
import time

# These paths are cache mounts, not inputs from the Docker build context.
CACHES = {"target", "rust/target", ".cache"}


def prepare(source, manifest):
    previous = json.loads(manifest.read_text()) if manifest.exists() else {}
    current = {}
    timestamp = time.time_ns()

    def visit(path):
        relative = path.relative_to(source).as_posix()
        if path.is_symlink():
            digest = hashlib.sha256(os.fsencode(os.readlink(path))).hexdigest()
        elif path.is_dir():
            children = [(child.name, visit(child)) for child in sorted(path.iterdir())
                        if child.relative_to(source).as_posix() not in CACHES]
            digest = hashlib.sha256(json.dumps(children).encode()).hexdigest()
        else:
            digest = hashlib.sha256(path.read_bytes()).hexdigest()
        # A mode change can affect scripts and packaging without changing their bytes.
        identity = [digest, path.lstat().st_mode]
        prior = previous.get(relative)
        stamp = prior[1] if prior and prior[0] == identity else timestamp
        os.utime(path, ns=(stamp, stamp), follow_symlinks=False)
        current[relative] = [identity, stamp]
        return identity

    visit(source)
    manifest.parent.mkdir(parents=True, exist_ok=True)
    temporary = manifest.with_suffix(".tmp")
    temporary.write_text(json.dumps(current, sort_keys=True))
    temporary.replace(manifest)


if __name__ == "__main__":
    prepare(Path(sys.argv[1]), Path(sys.argv[2]))
