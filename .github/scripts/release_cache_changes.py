#!/usr/bin/env python3
"""Skip release-cache warming for workspace version-only changes."""

import argparse
import subprocess
import tomllib
from fnmatch import fnmatchcase
from pathlib import Path

from v8_canary_changes import ROOT
from v8_canary_changes import git_output

BUILD_INPUTS = (
    "codex-rs/rust-toolchain.toml",
    ".github/actions/build-fork-release/*",
    ".github/actions/setup-rusty-v8/*",
    ".github/scripts/install-musl-*.sh",
    ".github/scripts/rusty_v8_bazel.py",
    ".github/scripts/release_cache_changes.py",
    ".github/workflows/warm-unix-release-cache.yml",
    "third_party/v8/*_release_manifests.sha256",
)


def normalized_cargo_input(path: str, content: bytes) -> object:
    data = tomllib.loads(content.decode())
    if path.endswith("Cargo.lock"):
        return [
            package
            for package in data["package"]
            if "source" in package or "checksum" in package
        ]
    for container in (data, data.get("workspace", {})):
        container.get("package", {}).pop("version", None)
        for section in ("dependencies", "build-dependencies", "dev-dependencies"):
            for dependency in container.get(section, {}).values():
                if isinstance(dependency, dict) and "path" in dependency:
                    dependency.pop("version", None)
    return data


def warming_required(base: str, head: str, *, root: Path = ROOT) -> bool:
    if not base.strip("0"):
        return True
    changed = git_output(
        "diff", "--name-only", "--no-renames", "-z", base, head, root=root
    )
    for path in changed.decode().split("\0"):
        if any(fnmatchcase(path, pattern) for pattern in BUILD_INPUTS):
            return True
        if path.startswith("codex-rs/") and (
            path.endswith("/Cargo.toml") or path == "codex-rs/Cargo.lock"
        ):
            try:
                previous, current = [
                    normalized_cargo_input(
                        path,
                        subprocess.check_output(
                            ["git", "show", f"{revision}:{path}"],
                            cwd=root,
                            stderr=subprocess.DEVNULL,
                        ),
                    )
                    for revision in (base, head)
                ]
                if previous != current:
                    return True
            except (subprocess.CalledProcessError, tomllib.TOMLDecodeError, KeyError):
                # Missing/new manifests and unknown input formats need a cold warm.
                return True
    return False


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", required=True)
    parser.add_argument("--head", default="HEAD")
    args = parser.parse_args()
    print(str(warming_required(args.base, args.head)).lower())
