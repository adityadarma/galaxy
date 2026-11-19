#!/usr/bin/env python3
"""Set the Rust workspace version from a release tag before Cargo builds."""

import json
import re
import subprocess
import sys
from pathlib import Path


def main() -> None:
    if len(sys.argv) != 2 or not re.fullmatch(r"v?[0-9]+\.[0-9]+\.[0-9]+", sys.argv[1]):
        raise SystemExit("Usage: scripts/set-rust-release-version.py vMAJOR.MINOR.PATCH")
    version = sys.argv[1].removeprefix("v")
    manifest = Path("Cargo.toml")
    contents = manifest.read_text()
    section = re.search(r"(?ms)^\[workspace\.package\]\n(?P<package>.*?)(?=^\[|\Z)", contents)
    if section is None:
        raise SystemExit("Missing [workspace.package] in Cargo.toml")
    updated, count = re.subn(
        r'(?m)^version = "[0-9]+\.[0-9]+\.[0-9]+"$',
        f'version = "{version}"',
        section.group("package"),
    )
    if count != 1:
        raise SystemExit("Expected one workspace package version in Cargo.toml")
    manifest.write_text(contents[: section.start("package")] + updated + contents[section.end("package") :])

    # Path dependencies and workspace package versions must match Cargo.lock
    # before build-core.sh runs `cargo build --locked`.
    subprocess.run(["cargo", "update", "--workspace"], check=True)
    metadata = json.loads(
        subprocess.run(
            ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout
    )
    members = set(metadata["workspace_members"])
    for package in metadata["packages"]:
        if package["id"] in members and package["version"] != version:
            raise SystemExit(f"Rust version mismatch: {package['name']}")


if __name__ == "__main__":
    main()
