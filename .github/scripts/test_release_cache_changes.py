import subprocess
import tempfile
import unittest
from pathlib import Path

from release_cache_changes import warming_required


class ReleaseCacheChangesTest(unittest.TestCase):
    def test_warming_tracks_dependency_and_build_changes(self) -> None:
        manifest = (
            '[workspace.package]\nversion = "1.0.0"\n[profile.release]\nopt-level = 3\n'
        )
        lock = (
            'version = 4\n[[package]]\nname = "local"\nversion = "1.0.0"\n'
            '[[package]]\nname = "dependency"\nversion = "2.0.0"\n'
            'source = "registry+https://example.invalid"\n'
        )
        scenarios = (
            (
                "workspace version",
                {
                    "codex-rs/Cargo.toml": manifest.replace("1.0.0", "1.0.1"),
                    "codex-rs/Cargo.lock": lock.replace("1.0.0", "1.0.1"),
                },
                False,
            ),
            (
                "dependency version",
                {"codex-rs/Cargo.lock": lock.replace("2.0.0", "2.0.1")},
                True,
            ),
            (
                "release profile",
                {
                    "codex-rs/Cargo.toml": manifest.replace(
                        "opt-level = 3", "opt-level = 2"
                    )
                },
                True,
            ),
            (
                "native setup",
                {".github/scripts/install-musl-openssl.sh": "new setup\n"},
                True,
            ),
            (
                "workspace source",
                {"codex-rs/example/src/lib.rs": "new source\n"},
                False,
            ),
            (
                "new workspace manifest",
                {
                    "codex-rs/new/Cargo.toml": '[package]\nname = "new"\nversion = "1.0.0"\n'
                },
                True,
            ),
        )
        for name, updates, expected in scenarios:
            with self.subTest(name=name), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                subprocess.run(
                    ["git", "init", "--quiet", str(root)],
                    check=True,
                    capture_output=True,
                )
                for files in (
                    {"codex-rs/Cargo.toml": manifest, "codex-rs/Cargo.lock": lock},
                    updates,
                ):
                    for path, content in files.items():
                        destination = root / path
                        destination.parent.mkdir(parents=True, exist_ok=True)
                        destination.write_text(content)
                    subprocess.run(["git", "add", "."], cwd=root, check=True)
                    subprocess.run(
                        [
                            "git",
                            "-c",
                            "user.name=Cache Test",
                            "-c",
                            "user.email=cache@example.invalid",
                            "commit",
                            "--quiet",
                            "-m",
                            "fixture",
                        ],
                        cwd=root,
                        check=True,
                        capture_output=True,
                    )
                self.assertEqual(
                    warming_required("HEAD~1", "HEAD", root=root),
                    expected,
                )
