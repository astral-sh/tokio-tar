#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Update the crate version and lockfile for a release."""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

PACKAGE_NAME = "astral-tokio-tar"
ROOT = Path(__file__).resolve().parent.parent


def run(*args: str, capture_output: bool = False) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        args,
        cwd=ROOT,
        check=True,
        capture_output=capture_output,
        text=True,
    )


def update_manifest(version: str) -> None:
    if not re.fullmatch(r"[0-9A-Za-z.+-]+", version) or not version[0].isdigit():
        raise SystemExit(f"invalid Cargo version: {version!r}")

    manifest = ROOT / "Cargo.toml"
    contents = manifest.read_text()
    package_start = contents.index("[package]")
    package_end = contents.find("\n[", package_start + len("[package]"))
    if package_end == -1:
        package_end = len(contents)

    package = contents[package_start:package_end]
    match = re.search(r'(?m)^version\s*=\s*"([^"]+)"$', package)
    if match is None:
        raise SystemExit("Cargo.toml [package] table has no version")
    if match.group(1) == version:
        raise SystemExit(f"Cargo.toml is already at version {version}")

    package = package[: match.start()] + f'version = "{version}"' + package[match.end() :]
    manifest.write_text(contents[:package_start] + package + contents[package_end:])


def main() -> None:
    if len(sys.argv) != 2:
        raise SystemExit(f"usage: {Path(sys.argv[0]).name} <version>")

    version = sys.argv[1]
    update_manifest(version)

    # Let Cargo validate the version and refresh only the root package entry.
    run("cargo", "update", "-p", PACKAGE_NAME)
    run(
        "cargo",
        "metadata",
        "--locked",
        "--no-deps",
        "--format-version",
        "1",
        capture_output=True,
    )
    run("git", "diff", "--check")
    changed = set(run("git", "diff", "--name-only", capture_output=True).stdout.splitlines())
    expected = {"Cargo.toml", "Cargo.lock"}
    if changed != expected:
        raise SystemExit(f"release preparation changed {sorted(changed)}, expected {sorted(expected)}")


if __name__ == "__main__":
    main()
