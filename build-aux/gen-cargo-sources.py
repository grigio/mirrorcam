#!/usr/bin/env python3
"""Generate cargo-sources.json (Flatpak vendoring) from Cargo.lock.

Every crates.io package in Cargo.lock becomes two flatpak-builder sources:
the .crate archive (sha256 from the lockfile) and the empty
.cargo-checksum.json that cargo's vendored layout requires. A final
`shell` source writes .cargo/config.toml redirecting crates.io to the
vendored directory inside the sandbox.

Run from the repo root, before a local `flatpak-builder` run and in the CI
`flatpak` job:

    python3 build-aux/gen-cargo-sources.py

Output is gitignored (like Carosello's): regenerate after any Cargo.lock
change, never commit it.
"""
import json
import sys
import tomllib
from pathlib import Path

CRATES_IO = "https://static.crates.io/crates"
VENDOR_DIR = "cargo/vendored"
# flatpak-builder builds each module in /run/build/<module name>; the
# module is called "mirrorcam" (see io.github.grigio.mirrorcam.yml).
SANDBOX_VENDOR = "/run/build/mirrorcam/" + VENDOR_DIR

CARGO_CONFIG = f"""[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "{SANDBOX_VENDOR}"
"""


def main() -> int:
    lock_path = Path("Cargo.lock")
    if not lock_path.is_file():
        print("error: run me from the repo root (Cargo.lock not found)", file=sys.stderr)
        return 1

    with open(lock_path, "rb") as f:
        lock = tomllib.load(f)

    sources = []
    for pkg in lock.get("package", []):
        name, version = pkg["name"], pkg["version"]
        if not pkg.get("source", "").startswith("registry+"):
            continue  # path/workspace members are part of `type: dir`
        checksum = pkg.get("checksum", "")
        if not checksum:
            print(f"error: {name} {version} has no checksum", file=sys.stderr)
            return 1
        crate_dir = f"{name}-{version}"
        sources.append(
            {
                "type": "archive",
                "archive-type": "tar-gzip",
                "url": f"{CRATES_IO}/{name}/{crate_dir}.crate",
                "sha256": checksum,
                "dest": f"{VENDOR_DIR}/{crate_dir}",
            }
        )
        sources.append(
            {
                "type": "inline",
                "contents": json.dumps({"package": checksum, "files": {}}),
                "dest": f"{VENDOR_DIR}/{crate_dir}",
                "dest-filename": ".cargo-checksum.json",
            }
        )

    sources.append(
        {
            "type": "shell",
            "commands": [
                "mkdir -p .cargo",
                "cat > .cargo/config.toml <<'EOF'\n" + CARGO_CONFIG + "EOF",
            ],
        }
    )

    with open("cargo-sources.json", "w") as f:
        json.dump(sources, f, indent=2)
        f.write("\n")
    print(f"cargo-sources.json: {len(sources)} entries")
    return 0


if __name__ == "__main__":
    sys.exit(main())
