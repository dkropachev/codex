#!/usr/bin/env python3

import hashlib
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import textwrap
import unittest


INSTALL_SCRIPT = Path(__file__).with_name("install.sh")
VERSION = "0.150.0"
NEXT_VERSION = "0.151.0"
OLD_VERSION = "0.149.1"
REPOSITORY = "dkropachev/codex"
TARGET = "aarch64-apple-darwin"
PACKAGE_ASSET = f"codex-package-{TARGET}.tar.gz"
CHECKSUM_ASSET = "codex-package_SHA256SUMS"


class InstallShTest(unittest.TestCase):
    def test_metadata_fetch_failure_is_not_reported_as_missing_assets(self) -> None:
        result, requests = run_installer(VERSION, metadata_failure=True)

        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(requests, [tag_metadata_url(VERSION)])
        self.assertIn(
            f"Could not fetch GitHub release metadata for Codex {VERSION}",
            result.stderr,
        )
        self.assertNotIn("Could not find Codex package", result.stderr)

    def test_exact_release_uses_fork_metadata_and_assets(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            archive_path, checksum_path, metadata_json = create_package_release(root)

            result, requests = run_installer_in(
                root,
                VERSION,
                metadata_json=metadata_json,
                archive_path=archive_path,
                checksum_path=checksum_path,
                force_macos=True,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(
                requests,
                [
                    tag_metadata_url(VERSION),
                    release_asset_url(VERSION, CHECKSUM_ASSET),
                    release_asset_url(VERSION, PACKAGE_ASSET),
                ],
            )
            self.assertIn(f"Resolved version: {VERSION}", result.stdout)

    def test_latest_release_uses_fork_latest_metadata_and_resolved_assets(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            archive_path, checksum_path, metadata_json = create_package_release(root)

            result, requests = run_installer_in(
                root,
                "latest",
                metadata_json=metadata_json,
                archive_path=archive_path,
                checksum_path=checksum_path,
                force_macos=True,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(
                requests,
                [
                    f"https://api.github.com/repos/{REPOSITORY}/releases/latest",
                    release_asset_url(VERSION, CHECKSUM_ASSET),
                    release_asset_url(VERSION, PACKAGE_ASSET),
                ],
            )
            self.assertIn(f"Resolved version: {VERSION}", result.stdout)

    def test_compact_metadata_is_independent_of_field_order(self) -> None:
        result, requests = run_installer(
            VERSION,
            metadata_json=release_metadata(compact=True, reorder=True),
        )

        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(
            requests,
            [
                tag_metadata_url(VERSION),
                release_asset_url(VERSION, CHECKSUM_ASSET),
            ],
        )
        self.assertIn(f"Resolved version: {VERSION}", result.stdout)

    def test_prerelease_is_rejected_before_metadata_fetch(self) -> None:
        result, requests = run_installer("0.150.1-alpha.1")

        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(requests, [])
        self.assertIn("Expected latest or x.y.z", result.stderr)

    def test_historical_exact_releases_are_manual_download_only(self) -> None:
        for release in (OLD_VERSION, f"v{OLD_VERSION}", f"rust-v{OLD_VERSION}"):
            with self.subTest(release=release):
                result, requests = run_installer(release)

                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(requests, [])
                self.assertIn(f"minimum managed fork release {VERSION}", result.stderr)
                self.assertIn("manual-download history only", result.stderr)
                self.assertIn(
                    f"https://github.com/{REPOSITORY}/releases/tag/rust-v{OLD_VERSION}",
                    result.stderr,
                )

    def test_latest_release_below_minimum_is_rejected(self) -> None:
        result, requests = run_installer(
            "latest",
            metadata_json=release_metadata(version=OLD_VERSION),
        )

        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(
            requests,
            [f"https://api.github.com/repos/{REPOSITORY}/releases/latest"],
        )
        self.assertIn("manual-download history only", result.stderr)

    def test_exact_release_rejects_mismatched_metadata_tag(self) -> None:
        result, requests = run_installer(
            VERSION,
            metadata_json=release_metadata(version=NEXT_VERSION),
        )

        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(requests, [tag_metadata_url(VERSION)])
        self.assertIn(
            f"Release metadata version did not match requested Codex version {VERSION}",
            result.stderr,
        )

    def test_json_like_strings_and_nested_fields_do_not_define_assets(self) -> None:
        result, requests = run_installer(
            VERSION,
            metadata_json=release_metadata_with_decoys(),
        )

        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(requests, [tag_metadata_url(VERSION)])
        self.assertIn("Could not find Codex package release assets", result.stderr)

    def test_install_uses_namespaced_leaf_and_atomic_links(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            archive_path, checksum_path, metadata_json = create_package_release(root)

            result, _requests = run_installer_in(
                root,
                VERSION,
                metadata_json=metadata_json,
                archive_path=archive_path,
                checksum_path=checksum_path,
                force_macos=True,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            standalone_root = root / "codex-home" / "packages" / "standalone"
            release_dir = (
                standalone_root / "releases" / f"dkropachev-{VERSION}-{TARGET}"
            )
            current = standalone_root / "current"
            install_bin = root / "install-bin"
            self.assertTrue((release_dir / "codex-package.json").is_file())
            self.assertEqual(os.readlink(current), str(release_dir))
            self.assertEqual(
                os.readlink(install_bin / "codex"),
                str(current / "bin" / "codex"),
            )
            self.assertEqual(
                os.readlink(install_bin / "codex-code-mode-host"),
                str(current / "bin" / "codex-code-mode-host"),
            )
            self.assertTrue(os.access(install_bin / "codex", os.X_OK))
            self.assertTrue(os.access(install_bin / "codex-code-mode-host", os.X_OK))

    def test_idempotent_reinstall_does_not_redownload_assets(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            archive_path, checksum_path, metadata_json = create_package_release(root)
            first_result, _requests = run_installer_in(
                root,
                VERSION,
                metadata_json=metadata_json,
                archive_path=archive_path,
                checksum_path=checksum_path,
                force_macos=True,
            )
            self.assertEqual(first_result.returncode, 0, first_result.stderr)

            (root / "requests.log").unlink()
            second_result, second_requests = run_installer_in(
                root,
                VERSION,
                metadata_json=metadata_json,
                force_macos=True,
            )

            self.assertEqual(second_result.returncode, 0, second_result.stderr)
            self.assertEqual(second_requests, [tag_metadata_url(VERSION)])
            self.assertNotIn("Downloading Codex CLI", second_result.stdout)

    def test_reinstall_repairs_missing_bundled_zsh(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            archive_path, checksum_path, metadata_json = create_package_release(root)
            first_result, _requests = run_installer_in(
                root,
                VERSION,
                metadata_json=metadata_json,
                archive_path=archive_path,
                checksum_path=checksum_path,
                force_macos=True,
            )
            self.assertEqual(first_result.returncode, 0, first_result.stderr)

            release_dir = (
                root
                / "codex-home"
                / "packages"
                / "standalone"
                / "releases"
                / f"dkropachev-{VERSION}-{TARGET}"
            )
            zsh_path = release_dir / "codex-resources" / "zsh" / "bin" / "zsh"
            zsh_path.unlink()
            (root / "requests.log").unlink()

            second_result, second_requests = run_installer_in(
                root,
                VERSION,
                metadata_json=metadata_json,
                archive_path=archive_path,
                checksum_path=checksum_path,
                force_macos=True,
            )

            self.assertEqual(second_result.returncode, 0, second_result.stderr)
            self.assertEqual(
                second_requests,
                [
                    tag_metadata_url(VERSION),
                    release_asset_url(VERSION, CHECKSUM_ASSET),
                    release_asset_url(VERSION, PACKAGE_ASSET),
                ],
            )
            self.assertTrue(os.access(zsh_path, os.X_OK))

    def test_checksum_asset_must_match_github_digest(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            archive_path, checksum_path, metadata_json = create_package_release(root)
            metadata = json.loads(metadata_json)
            set_asset_digest(metadata, CHECKSUM_ASSET, "0" * 64)

            result, requests = run_installer_in(
                root,
                VERSION,
                metadata_json=json.dumps(metadata),
                archive_path=archive_path,
                checksum_path=checksum_path,
                force_macos=True,
            )

            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(
                requests,
                [
                    tag_metadata_url(VERSION),
                    release_asset_url(VERSION, CHECKSUM_ASSET),
                ],
            )
            self.assertIn("checksum did not match expected digest", result.stderr)

    def test_manifest_digest_must_match_github_package_digest(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            archive_path, checksum_path, metadata_json = create_package_release(root)
            checksum_path.write_text(
                f"{'0' * 64}  {PACKAGE_ASSET}\n",
                encoding="utf-8",
            )
            metadata = json.loads(metadata_json)
            set_asset_digest(metadata, CHECKSUM_ASSET, file_sha256(checksum_path))

            result, requests = run_installer_in(
                root,
                VERSION,
                metadata_json=json.dumps(metadata),
                archive_path=archive_path,
                checksum_path=checksum_path,
                force_macos=True,
            )

            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(
                requests,
                [
                    tag_metadata_url(VERSION),
                    release_asset_url(VERSION, CHECKSUM_ASSET),
                ],
            )
            self.assertIn("GitHub and codex-package_SHA256SUMS disagree", result.stderr)

    def test_checksum_manifest_must_contain_selected_package(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            archive_path, checksum_path, metadata_json = create_package_release(root)
            checksum_path.write_text(
                f"{'0' * 64}  codex-package-other-target.tar.gz\n",
                encoding="utf-8",
            )
            metadata = json.loads(metadata_json)
            set_asset_digest(metadata, CHECKSUM_ASSET, file_sha256(checksum_path))

            result, requests = run_installer_in(
                root,
                VERSION,
                metadata_json=json.dumps(metadata),
                archive_path=archive_path,
                checksum_path=checksum_path,
                force_macos=True,
            )

            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(
                requests,
                [
                    tag_metadata_url(VERSION),
                    release_asset_url(VERSION, CHECKSUM_ASSET),
                ],
            )
            self.assertIn(
                f"Could not find SHA-256 digest for {PACKAGE_ASSET}", result.stderr
            )

    def test_downloaded_package_must_match_agreed_digest(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            archive_path, checksum_path, metadata_json = create_package_release(root)
            expected_digest = "0" * 64
            checksum_path.write_text(
                f"{expected_digest}  {PACKAGE_ASSET}\n",
                encoding="utf-8",
            )
            metadata = json.loads(metadata_json)
            set_asset_digest(metadata, PACKAGE_ASSET, expected_digest)
            set_asset_digest(metadata, CHECKSUM_ASSET, file_sha256(checksum_path))

            result, requests = run_installer_in(
                root,
                VERSION,
                metadata_json=json.dumps(metadata),
                archive_path=archive_path,
                checksum_path=checksum_path,
                force_macos=True,
            )

            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(
                requests,
                [
                    tag_metadata_url(VERSION),
                    release_asset_url(VERSION, CHECKSUM_ASSET),
                    release_asset_url(VERSION, PACKAGE_ASSET),
                ],
            )
            self.assertIn("checksum did not match expected digest", result.stderr)

    def test_wrong_binary_version_is_not_activated(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            archive_path, checksum_path, metadata_json = create_package_release(
                root,
                binary_version=NEXT_VERSION,
            )

            result, _requests = run_installer_in(
                root,
                VERSION,
                metadata_json=metadata_json,
                archive_path=archive_path,
                checksum_path=checksum_path,
                force_macos=True,
            )

            self.assertNotEqual(result.returncode, 0)
            self.assertIn(f"did not report expected version {VERSION}", result.stderr)
            self.assertFalse(
                (root / "codex-home" / "packages" / "standalone" / "current").exists()
            )
            self.assertNotIn("installed successfully", result.stdout)

    def test_platform_npm_asset_is_not_an_installer_fallback(self) -> None:
        npm_asset = f"codex-npm-darwin-arm64-{VERSION}.tgz"
        metadata_json = json.dumps(
            {
                "tag_name": f"rust-v{VERSION}",
                "assets": [{"name": npm_asset, "digest": f"sha256:{'a' * 64}"}],
            }
        )

        result, requests = run_installer(VERSION, metadata_json=metadata_json)

        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(requests, [tag_metadata_url(VERSION)])
        self.assertIn("Could not find Codex package release assets", result.stderr)

    def test_installer_has_no_openai_release_fallback(self) -> None:
        installer = INSTALL_SCRIPT.read_text(encoding="utf-8")

        self.assertNotIn("releases.openai.com", installer)
        self.assertNotIn("https://github.com/openai/codex", installer)
        self.assertNotIn("CODEX_INSTALLER_USE_RELEASES_OPENAI_COM", installer)


def run_installer(
    release: str,
    *,
    metadata_failure: bool = False,
    metadata_json: str | None = None,
) -> tuple[subprocess.CompletedProcess[str], list[str]]:
    with tempfile.TemporaryDirectory() as temp_dir:
        return run_installer_in(
            Path(temp_dir),
            release,
            metadata_failure=metadata_failure,
            metadata_json=metadata_json,
        )


def run_installer_in(
    root: Path,
    release: str,
    *,
    metadata_failure: bool = False,
    metadata_json: str | None = None,
    archive_path: Path | None = None,
    checksum_path: Path | None = None,
    force_macos: bool = False,
) -> tuple[subprocess.CompletedProcess[str], list[str]]:
    bin_dir = root / "bin"
    bin_dir.mkdir(exist_ok=True)
    request_log = root / "requests.log"
    fake_curl = bin_dir / "curl"
    fake_curl.write_text(
        textwrap.dedent(
            f"""\
            #!/bin/sh
            url=""
            output=""
            previous=""
            for arg in "$@"; do
              case "$arg" in
                https://*) url="$arg" ;;
              esac
              if [ "$previous" = "-o" ]; then
                output="$arg"
              fi
              previous="$arg"
            done
            printf '%s\n' "$url" >>"$CODEX_TEST_REQUEST_LOG"

            case "$url" in
              https://api.github.com/repos/{REPOSITORY}/releases/*)
                if [ "$CODEX_TEST_METADATA_FAILURE" = "1" ]; then
                  echo "curl: (22) The requested URL returned error: 403" >&2
                  exit 22
                fi
                printf '%s\n' "$CODEX_TEST_METADATA_JSON"
                ;;
              https://github.com/{REPOSITORY}/releases/download/*/{CHECKSUM_ASSET})
                if [ -n "$CODEX_TEST_CHECKSUM_PATH" ]; then
                  cp "$CODEX_TEST_CHECKSUM_PATH" "$output"
                else
                  exit 22
                fi
                ;;
              https://github.com/{REPOSITORY}/releases/download/*/codex-package-*.tar.gz)
                if [ -n "$CODEX_TEST_ARCHIVE_PATH" ]; then
                  cp "$CODEX_TEST_ARCHIVE_PATH" "$output"
                else
                  exit 22
                fi
                ;;
              *)
                exit 22
                ;;
            esac
            """
        ),
        encoding="utf-8",
    )
    fake_curl.chmod(0o755)
    if force_macos:
        fake_uname = bin_dir / "uname"
        fake_uname.write_text(
            "#!/bin/sh\n"
            'case "$1" in\n'
            "  -s) printf 'Darwin\\n' ;;\n"
            "  -m) printf 'arm64\\n' ;;\n"
            "esac\n",
            encoding="utf-8",
        )
        fake_uname.chmod(0o755)

    home = root / "home"
    home.mkdir(exist_ok=True)
    env = os.environ.copy()
    env.update(
        {
            "CODEX_HOME": str(root / "codex-home"),
            "CODEX_INSTALL_DIR": str(root / "install-bin"),
            "CODEX_NON_INTERACTIVE": "1",
            "CODEX_RELEASE": release,
            "CODEX_TEST_ARCHIVE_PATH": str(archive_path or ""),
            "CODEX_TEST_CHECKSUM_PATH": str(checksum_path or ""),
            "CODEX_TEST_METADATA_FAILURE": "1" if metadata_failure else "0",
            "CODEX_TEST_METADATA_JSON": (
                metadata_json if metadata_json is not None else release_metadata()
            ),
            "CODEX_TEST_REQUEST_LOG": str(request_log),
            "HOME": str(home),
            "PATH": f"{bin_dir}:/usr/bin:/bin",
            "SHELL": "/bin/sh",
        }
    )
    result = subprocess.run(
        ["/bin/sh", str(INSTALL_SCRIPT)],
        capture_output=True,
        check=False,
        env=env,
        text=True,
    )
    requests = (
        request_log.read_text(encoding="utf-8").splitlines()
        if request_log.exists()
        else []
    )
    return result, requests


def create_package_release(
    root: Path,
    *,
    binary_version: str = VERSION,
    metadata_version: str = VERSION,
) -> tuple[Path, Path, str]:
    package_dir = root / "package"
    (package_dir / "bin").mkdir(parents=True)
    (package_dir / "codex-path").mkdir()
    (package_dir / "codex-resources" / "zsh" / "bin").mkdir(parents=True)
    (package_dir / "codex-package.json").write_text(
        json.dumps(
            {
                "layoutVersion": 1,
                "version": metadata_version,
                "target": TARGET,
                "variant": "codex",
                "entrypoint": "bin/codex",
                "resourcesDir": "codex-resources",
                "pathDir": "codex-path",
            }
        )
        + "\n",
        encoding="utf-8",
    )
    write_executable(
        package_dir / "bin" / "codex",
        f"#!/bin/sh\nprintf 'codex-cli {binary_version}\\n'\n",
    )
    write_executable(
        package_dir / "bin" / "codex-code-mode-host",
        "#!/bin/sh\nexit 0\n",
    )
    write_executable(package_dir / "codex-path" / "rg", "#!/bin/sh\nexit 0\n")
    write_executable(
        package_dir / "codex-resources" / "zsh" / "bin" / "zsh",
        "#!/bin/sh\nexit 0\n",
    )

    archive_path = root / PACKAGE_ASSET
    with tarfile.open(archive_path, "w:gz") as archive:
        for path in package_dir.iterdir():
            archive.add(path, arcname=path.name)

    archive_digest = file_sha256(archive_path)
    checksum_path = root / CHECKSUM_ASSET
    checksum_path.write_text(
        f"{archive_digest}  {PACKAGE_ASSET}\n",
        encoding="utf-8",
    )
    metadata_json = json.dumps(
        {
            "assets": [
                {"name": PACKAGE_ASSET, "digest": f"sha256:{archive_digest}"},
                {
                    "name": CHECKSUM_ASSET,
                    "digest": f"sha256:{file_sha256(checksum_path)}",
                },
            ],
            "tag_name": f"rust-v{metadata_version}",
        },
        indent=2,
    )
    return archive_path, checksum_path, metadata_json


def write_executable(path: Path, contents: str) -> None:
    path.write_text(contents, encoding="utf-8")
    path.chmod(0o755)


def file_sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def set_asset_digest(metadata: dict[str, object], asset_name: str, digest: str) -> None:
    assets = metadata["assets"]
    assert isinstance(assets, list)
    for asset in assets:
        assert isinstance(asset, dict)
        if asset["name"] == asset_name:
            asset["digest"] = f"sha256:{digest}"
            return
    raise AssertionError(f"missing asset metadata for {asset_name}")


def release_metadata(
    *,
    version: str = VERSION,
    compact: bool = False,
    reorder: bool = False,
) -> str:
    assets = [
        asset_metadata(
            f"codex-package-{target}.tar.gz",
            f"sha256:{'a' * 64}",
            reorder=reorder,
        )
        for target in (
            "aarch64-apple-darwin",
            "x86_64-apple-darwin",
            "aarch64-unknown-linux-musl",
            "x86_64-unknown-linux-musl",
        )
    ]
    assets.append(
        asset_metadata(
            CHECKSUM_ASSET,
            f"sha256:{'b' * 64}",
            reorder=reorder,
        )
    )
    separators = (",", ":") if compact else None
    return json.dumps(
        {"assets": assets, "body": "braces: { } [ ]", "tag_name": f"rust-v{version}"},
        indent=None if compact else 2,
        separators=separators,
    )


def asset_metadata(name: str, digest: str, *, reorder: bool) -> dict[str, str]:
    if reorder:
        return {"digest": digest, "name": name}
    return {"name": name, "digest": digest}


def release_metadata_with_decoys() -> str:
    fake_digest = f"sha256:{'0' * 64}"
    return json.dumps(
        {
            "body": (f'fake: {{"name":"{CHECKSUM_ASSET}","digest":"{fake_digest}"}}'),
            "assets": [
                {
                    "metadata": {
                        "name": PACKAGE_ASSET,
                        "digest": fake_digest,
                    },
                    "digest": f"sha256:{'c' * 64}",
                    "name": f"codex-npm-darwin-arm64-{VERSION}.tgz",
                }
            ],
            "tag_name": f"rust-v{VERSION}",
        },
        separators=(",", ":"),
    )


def tag_metadata_url(version: str) -> str:
    return f"https://api.github.com/repos/{REPOSITORY}/releases/tags/rust-v{version}"


def release_asset_url(version: str, asset: str) -> str:
    return f"https://github.com/{REPOSITORY}/releases/download/rust-v{version}/{asset}"


if __name__ == "__main__":
    unittest.main()
