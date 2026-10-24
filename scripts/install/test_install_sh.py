#!/usr/bin/env python3

from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
import os
from pathlib import Path
import select
import shlex
import shutil
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
HOST_ASSET = f"codex-code-mode-host-{TARGET}"
CHECKSUM_ASSET = "codex-package_SHA256SUMS"


class InstallShTest(unittest.TestCase):
    def test_cli_latest_refreshes_daemon_but_explicit_and_daemon_installs_do_not(
        self,
    ) -> None:
        for release, daemon_only, syncs in [
            ("latest", False, True),
            (VERSION, False, False),
            ("latest", True, False),
        ]:
            with (
                self.subTest(release=release, daemon_only=daemon_only),
                tempfile.TemporaryDirectory() as temp,
            ):
                root = Path(temp)
                archive, checksum, metadata = create_package_release(root)
                result, _ = run_installer_in(
                    root,
                    release,
                    daemon_only=daemon_only,
                    metadata_json=metadata,
                    archive_path=archive,
                    checksum_path=checksum,
                    force_macos=True,
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                log = root / "codex-home/daemon-sync.log"
                self.assertEqual(
                    log.read_text().splitlines() if log.exists() else [],
                    ["app-server daemon sync-from-cli"] if syncs else [],
                )

    def test_updater_installer_calls_do_not_recursively_synchronize_the_daemon(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            archive, checksum, metadata = create_package_release(root)
            options = dict(
                metadata_json=metadata,
                archive_path=archive,
                checksum_path=checksum,
                force_macos=True,
            )
            installed, _ = run_installer_in(root, "latest", **options)
            self.assertEqual(installed.returncode, 0, installed.stderr)
            log = root / "codex-home/daemon-sync.log"
            log.unlink()
            for manual in [False, True]:
                result, _ = run_installer_in(
                    root,
                    "latest",
                    update_guard_from_release=f"dkropachev-{VERSION}-{TARGET}",
                    manual_update=manual,
                    **options,
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertFalse(log.exists())

    def test_daemon_refresh_failure_does_not_report_successful_upgrade(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            archive, checksum, metadata = create_package_release(root)
            result, _ = run_installer_in(
                root,
                "latest",
                metadata_json=metadata,
                archive_path=archive,
                checksum_path=checksum,
                force_macos=True,
                daemon_sync_status=1,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("background server could not be updated", result.stderr)
            self.assertNotIn("installed successfully", result.stdout)

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
                    release_asset_url(VERSION, HOST_ASSET),
                ],
            )
            self.assertIn(f"Resolved version: {VERSION}", result.stdout)

    def test_current_release_without_bundled_zsh_installs_and_reuses_cache(
        self,
    ) -> None:
        version = "0.163.0"
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            archive_path, checksum_path, metadata_json = create_package_release(
                root,
                binary_version=version,
                metadata_version=version,
                bundled_zsh=False,
            )
            for phase, expected_requests in (
                (
                    "install",
                    [
                        tag_metadata_url(version),
                        release_asset_url(version, CHECKSUM_ASSET),
                        release_asset_url(version, PACKAGE_ASSET),
                        release_asset_url(version, HOST_ASSET),
                    ],
                ),
                ("reuse", [tag_metadata_url(version)]),
            ):
                with self.subTest(phase=phase):
                    (root / "requests.log").unlink(missing_ok=True)
                    result, requests = run_installer_in(
                        root,
                        version,
                        metadata_json=metadata_json,
                        archive_path=archive_path,
                        checksum_path=checksum_path,
                        force_macos=True,
                    )
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(requests, expected_requests)
                    release_dir = root / (
                        "codex-home/packages/standalone/releases/"
                        f"dkropachev-{version}-{TARGET}"
                    )
                    self.assertEqual(
                        (root / "codex-home/packages/standalone/current").resolve(),
                        release_dir,
                    )
                    self.assertFalse(
                        (release_dir / "codex-resources/zsh/bin/zsh").exists()
                    )

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
                    release_asset_url(VERSION, HOST_ASSET),
                ],
            )
            self.assertIn(f"Resolved version: {VERSION}", result.stdout)

    def test_release_without_host_asset_is_not_installable(self) -> None:
        metadata = json.loads(release_metadata())
        metadata["assets"] = [
            asset
            for asset in metadata["assets"]
            if not asset["name"].startswith("codex-code-mode-host-")
        ]

        result, requests = run_installer(VERSION, metadata_json=json.dumps(metadata))

        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(requests, [tag_metadata_url(VERSION)])
        self.assertIn("Could not find Codex package release assets", result.stderr)

    def test_packaged_host_must_match_standalone_host(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            archive_path, checksum_path, metadata_json = create_package_release(root)
            host_path = root / HOST_ASSET
            write_executable(host_path, "#!/bin/sh\nexit 1\n")
            host_digest = file_sha256(host_path)
            checksum_path.write_text(
                f"{file_sha256(archive_path)}  {PACKAGE_ASSET}\n"
                f"{host_digest}  {HOST_ASSET}\n",
                encoding="utf-8",
            )
            metadata = json.loads(metadata_json)
            set_asset_digest(metadata, HOST_ASSET, host_digest)
            set_asset_digest(metadata, CHECKSUM_ASSET, file_sha256(checksum_path))

            result, _requests = run_installer_in(
                root,
                VERSION,
                metadata_json=json.dumps(metadata),
                archive_path=archive_path,
                checksum_path=checksum_path,
                force_macos=True,
            )

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("Packaged code-mode host differs", result.stderr)
            self.assertFalse((root / "codex-home/packages/standalone/current").exists())

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

    def test_existing_release_with_wrong_host_is_reinstalled(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            archive_path, checksum_path, metadata_json = create_package_release(root)
            options = dict(
                metadata_json=metadata_json,
                archive_path=archive_path,
                checksum_path=checksum_path,
                force_macos=True,
            )
            installed, _requests = run_installer_in(root, VERSION, **options)
            self.assertEqual(installed.returncode, 0, installed.stderr)
            host_path = (
                root
                / "codex-home/packages/standalone/releases"
                / f"dkropachev-{VERSION}-{TARGET}/bin/codex-code-mode-host"
            )
            write_executable(host_path, "#!/bin/sh\nexit 1\n")
            (root / "requests.log").unlink()

            repaired, requests = run_installer_in(root, VERSION, **options)

            self.assertEqual(repaired.returncode, 0, repaired.stderr)
            self.assertEqual(file_sha256(host_path), file_sha256(root / HOST_ASSET))
            self.assertEqual(
                requests,
                [
                    tag_metadata_url(VERSION),
                    release_asset_url(VERSION, CHECKSUM_ASSET),
                    release_asset_url(VERSION, PACKAGE_ASSET),
                    release_asset_url(VERSION, HOST_ASSET),
                ],
            )

    def test_daemon_install_preserves_visible_cli_and_profile(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir).resolve()
            archive, checksum, metadata = create_package_release(root)
            options = dict(
                metadata_json=metadata,
                archive_path=archive,
                checksum_path=checksum,
                force_macos=True,
            )
            pinned, _ = run_installer_in(root, VERSION, **options)
            self.assertEqual(pinned.returncode, 0, pinned.stderr)
            visible = root / "install-bin/codex"
            original = visible.resolve()
            pending = visible.with_name(".codex.12345")
            pending.symlink_to(original)
            profile = root / "home/.profile"
            profile.write_text("unchanged profile\n")

            installed, _ = run_installer_in(root, "latest", daemon_only=True, **options)
            self.assertEqual(installed.returncode, 0, installed.stderr)
            daemon = root / "codex-home/packages/app-server-daemon"
            release_name = f"dkropachev-{VERSION}-aarch64-apple-darwin"
            self.assertEqual(
                (daemon / "current").resolve(), daemon / "releases" / release_name
            )
            self.assertEqual((daemon / "auto-update-version").read_text(), release_name)
            history = daemon / "selection-history"
            original_history = history.read_bytes()
            self.assertEqual(history.read_text().splitlines()[:2], [release_name, ""])
            repeated, _ = run_installer_in(root, "latest", daemon_only=True, **options)
            self.assertEqual(repeated.returncode, 0, repeated.stderr)
            self.assertEqual(history.read_bytes(), original_history)
            options["daemon_only"] = True
            local = daemon / "releases/local-development"
            local.mkdir()
            (daemon / "current").unlink()
            (daemon / "current").symlink_to(local)
            marker = daemon / "auto-update-version"
            marker.unlink()

            scheduled, _ = run_installer_in(
                root, "latest", update_guard_from_release=local.name, **options
            )
            self.assertEqual(scheduled.returncode, 0, scheduled.stderr)
            self.assertEqual((daemon / "current").resolve(), local)
            self.assertFalse(marker.exists())

            raced, _ = run_installer_in(
                root,
                "latest",
                update_guard_from_release="a-different-release",
                manual_update=True,
                **options,
            )
            self.assertNotEqual(raced.returncode, 0, raced.stderr)
            self.assertEqual((daemon / "current").resolve(), local)
            self.assertFalse(marker.exists())

            binary = daemon / "releases" / release_name / "bin/codex"
            contents = binary.read_text()
            binary.write_text(
                '#!/bin/sh\n[ "$1" = "--version" ] || exit 2\n'
                f"echo codex-cli {VERSION}\n"
            )
            incompatible, _ = run_installer_in(
                root,
                "latest",
                update_guard_from_release=local.name,
                manual_update=True,
                **options,
            )
            self.assertNotEqual(incompatible.returncode, 0)
            self.assertIn("does not support daemon-owned packages", incompatible.stderr)
            self.assertEqual((daemon / "current").resolve(), local)
            self.assertFalse(marker.exists())
            binary.write_text(contents)

            restored, _ = run_installer_in(
                root,
                "latest",
                update_guard_from_release=local.name,
                manual_update=True,
                **options,
            )
            self.assertEqual(restored.returncode, 0, restored.stderr)
            self.assertEqual(
                (daemon / "current").resolve(), daemon / "releases" / release_name
            )
            self.assertEqual(marker.read_text(), release_name)
            self.assertEqual(visible.resolve(), original)
            self.assertTrue(pending.is_symlink())
            self.assertEqual(profile.read_text(), "unchanged profile\n")
            self.assertFalse(
                (root / "codex-home/packages/standalone/auto-update-version").exists()
            )

    def test_explicit_release_pins_even_the_current_latest_version(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            archive, checksum, metadata = create_package_release(root)
            options = dict(
                metadata_json=metadata,
                archive_path=archive,
                checksum_path=checksum,
                force_macos=True,
            )
            marker = root / "codex-home/packages/standalone/auto-update-version"
            latest, _ = run_installer_in(root, "latest", **options)
            self.assertEqual(latest.returncode, 0, latest.stderr)
            release_name = f"dkropachev-{VERSION}-{TARGET}"
            self.assertEqual(marker.read_text(encoding="utf-8"), release_name)

            pinned, _ = run_installer_in(root, VERSION, **options)
            self.assertEqual(pinned.returncode, 0, pinned.stderr)
            self.assertFalse(marker.exists())

            updater_record = (
                root / "codex-home/app-server-daemon/app-server-updater.pid"
            )
            updater_record.parent.mkdir(parents=True)
            updater_record.write_text(
                json.dumps(
                    {"pid": os.getpid(), "processStartTime": process_start_time()}
                ),
                encoding="utf-8",
            )
            old_updater, _ = run_installer_in(
                root,
                "latest",
                old_updater_parent_pid=os.getpid(),
                **options,
            )
            self.assertEqual(old_updater.returncode, 0, old_updater.stderr)
            self.assertFalse(marker.exists())

            skipped, _ = run_installer_in(
                root,
                "latest",
                update_guard_from_release=release_name,
                **options,
            )
            self.assertEqual(skipped.returncode, 0, skipped.stderr)
            self.assertFalse(marker.exists())

            updater_record.write_text(
                json.dumps({"pid": os.getpid(), "processStartTime": "stale"}),
                encoding="utf-8",
            )
            latest_again, _ = run_installer_in(
                root,
                "latest",
                old_updater_parent_pid=os.getpid(),
                **options,
            )
            self.assertEqual(latest_again.returncode, 0, latest_again.stderr)
            self.assertEqual(marker.read_text(encoding="utf-8"), release_name)

            managed = (
                root
                / f"codex-home/packages/standalone/releases/{release_name}/bin/codex"
            )
            managed.unlink()
            guarded, _ = run_installer_in(
                root,
                "latest",
                update_guard_from_release=release_name,
                **options,
            )
            self.assertEqual(guarded.returncode, 0, guarded.stderr)
            self.assertTrue(managed.exists())

    def test_standalone_retains_previous_and_running_releases_across_installs(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            # Exercise real process inspection, including paths containing spaces.
            root = Path(temp_dir).resolve() / "codex install"
            root.mkdir()
            system = os.uname()
            if system.sysname == "Linux":
                # The race requires pruning to proceed. A live but unreadable
                # Codex/helper (e.g. the agent's sandbox) must prevent that.
                for process in Path("/proc").glob("[0-9]*"):
                    try:
                        (process / "exe").readlink()
                    except OSError:
                        try:
                            name = (process / "comm").read_text().strip()
                            status = (process / "status").read_text()
                        except OSError:
                            continue  # The process may have exited.
                        if (
                            name.startswith("codex") or name in ("rg", "bwrap")
                        ) and not any(
                            line.startswith("State:") and "Z" in line
                            for line in status.splitlines()
                        ):
                            self.skipTest(
                                f"Cannot inspect live {name} process {process.name}"
                            )
            arch = "aarch64" if system.machine in ("arm64", "aarch64") else "x86_64"
            target = f"{arch}-{'apple-darwin' if system.sysname == 'Darwin' else 'unknown-linux-musl'}"
            packages = root / "codex-home/packages/standalone"
            releases = packages / "releases"
            versions = ("0.140.0", "0.141.0", VERSION)
            install_options = {}
            for version in versions:
                source = root / version
                source.mkdir()
                archive, checksum, metadata = create_package_release(
                    source,
                    metadata_version=version,
                    binary_version=version,
                    target=target,
                )
                install_options[version] = dict(
                    metadata_json=metadata, archive_path=archive, checksum_path=checksum
                )

            first, _ = run_installer_in(
                root, versions[0], **install_options[versions[0]]
            )
            self.assertEqual(first.returncode, 0, first.stderr)
            oldest = releases / f"{versions[0]}-{target}"
            helper = oldest / "bin/codex-code-mode-host"
            shutil.copy("/bin/sleep", helper)
            if system.sysname == "Darwin":
                # A copied macOS system binary cannot run with its platform signature.
                subprocess.run(
                    ["/usr/bin/codesign", "--force", "--sign", "-", str(helper)],
                    check=True,
                    capture_output=True,
                )
            # A helper can outlive its parent CLI; retain its whole release.
            with subprocess.Popen([str(helper), "300"]) as running:
                try:
                    second, _ = run_installer_in(
                        root, versions[1], **install_options[versions[1]]
                    )
                    self.assertEqual(second.returncode, 0, second.stderr)

                    unowned = releases / f"0.139.0-{target}"
                    unowned.mkdir()
                    (unowned / "codex-package.json").write_text("{}")
                    retired = releases / ".retired.old-attempt"
                    retired.mkdir()
                    (retired / "leave-alone").write_text("not retried")
                    external = root / "external"
                    external.mkdir()
                    (external / "user-file").write_text("untouched")
                    (releases / f"0.138.0-{target}").symlink_to(external)

                    # Failed installs must not advance history or prune anything.
                    history = packages / "selection-history"
                    before_failure = history.read_bytes()
                    bad_source = root / "wrong-version"
                    bad_source.mkdir()
                    archive, checksum, metadata = create_package_release(
                        bad_source, metadata_version="0.199.0", target=target
                    )
                    failed, _ = run_installer_in(
                        root,
                        "0.199.0",
                        metadata_json=metadata,
                        archive_path=archive,
                        checksum_path=checksum,
                    )
                    self.assertNotEqual(failed.returncode, 0, failed.stderr)
                    self.assertEqual(history.read_bytes(), before_failure)
                    self.assertTrue(oldest.is_dir())

                    latest, _ = run_installer_in(
                        root, "latest", **install_options[VERSION]
                    )
                    self.assertEqual(latest.returncode, 0, latest.stderr)
                    self.assertIsNone(running.poll())
                    self.assertTrue(oldest.is_dir(), latest.stderr)
                    previous = releases / f"{versions[1]}-{target}"
                    current = releases / f"{VERSION}-{target}"
                    self.assertEqual((packages / "current").resolve(), current)
                    self.assertTrue(previous.is_dir())
                    self.assertEqual(
                        history.read_text().splitlines()[:2],
                        [current.name, previous.name],
                    )
                    recorded = history.read_bytes()
                finally:
                    running.terminate()
                    running.wait()

            if system.sysname == "Darwin":
                # lsof sees no running helper now. ps can still see names for
                # a different user, whose open files lsof silently omits.
                fake_ps = root / "bin/ps"
                foreign_uid = 0 if os.geteuid() != 0 else 501
                try:
                    for executable in (oldest / "bin/codex", helper):
                        with self.subTest(foreign_executable=executable.name):
                            fake_ps.write_text(
                                "#!/bin/sh\n"
                                'case "$*" in\n'
                                "  '-axo uid=,comm=')\n"
                                f"    printf '%s\\n' {shlex.quote(f'{foreign_uid} {executable}')}\n"
                                "    ;;\n"
                                '  *) exec /bin/ps "$@" ;;\n'
                                "esac\n"
                            )
                            fake_ps.chmod(0o755)
                            retained, _ = run_installer_in(
                                root, "latest", **install_options[VERSION]
                            )
                            self.assertEqual(retained.returncode, 0, retained.stderr)
                            self.assertTrue(oldest.is_dir(), retained.stderr)
                            self.assertEqual(history.read_bytes(), recorded)
                finally:
                    fake_ps.unlink()

            # Start the helper after the first scan but immediately before the
            # retirement rename. The second scan must restore its release.
            fake_mv = root / "bin/mv"
            ready = root / "retirement-ready"
            resume = root / "retirement-resume"
            os.mkfifo(ready)
            os.mkfifo(resume)
            real_mv = shutil.which("mv")
            fake_mv.write_text(
                "#!/bin/sh\n"
                'case "$2" in\n'
                "  */.retired.*/release)\n"
                f'    printf "ready\\n" > {shlex.quote(str(ready))}\n'
                f"    read -r _ < {shlex.quote(str(resume))}\n"
                "    ;;\n"
                "esac\n"
                f'exec {shlex.quote(real_mv)} "$@"\n'
            )
            fake_mv.chmod(0o755)
            ready_fd = os.open(ready, os.O_RDWR | os.O_NONBLOCK)
            resume_fd = os.open(resume, os.O_RDWR | os.O_NONBLOCK)
            try:
                with ThreadPoolExecutor(max_workers=1) as executor:
                    installing = executor.submit(
                        run_installer_in, root, "latest", **install_options[VERSION]
                    )
                    try:
                        readable, _, _ = select.select([ready_fd], [], [], 30)
                        self.assertTrue(
                            readable, "installer did not attempt retirement"
                        )
                        self.assertEqual(os.read(ready_fd, 128), b"ready\n")
                        with subprocess.Popen([str(helper), "300"]) as late_helper:
                            try:
                                os.write(resume_fd, b"continue\n")
                                raced, _ = installing.result(timeout=30)
                                self.assertEqual(raced.returncode, 0, raced.stderr)
                                self.assertTrue(oldest.is_dir(), raced.stderr)
                                self.assertEqual(history.read_bytes(), recorded)
                            finally:
                                late_helper.terminate()
                                late_helper.wait()
                    finally:
                        # Release the intercepted mv even if an assertion fails.
                        os.write(resume_fd, b"continue\n")
            finally:
                os.close(ready_fd)
                os.close(resume_fd)
                fake_mv.unlink()

            # A no-op latest install (also the codex update path) cleans up the
            # now-unused release without rotating previous to current.
            repeated, _ = run_installer_in(root, "latest", **install_options[VERSION])
            self.assertEqual(repeated.returncode, 0, repeated.stderr)
            self.assertFalse(oldest.exists(), repeated.stderr)
            self.assertTrue(previous.is_dir())
            self.assertEqual(history.read_bytes(), recorded)
            self.assertTrue(unowned.is_dir())
            self.assertEqual((retired / "leave-alone").read_text(), "not retried")
            self.assertEqual((external / "user-file").read_text(), "untouched")

            # Downgrades keep the actual previously selected version.
            pinned, _ = run_installer_in(
                root, versions[1], **install_options[versions[1]]
            )
            self.assertEqual(pinned.returncode, 0, pinned.stderr)
            self.assertEqual((packages / "current").resolve(), previous)
            self.assertEqual(
                history.read_text().splitlines()[:2], [previous.name, current.name]
            )
            self.assertTrue(current.is_dir())

    def test_uninspectable_legacy_updater_does_not_clear_pin(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            archive, checksum, metadata = create_package_release(root)
            options = dict(
                metadata_json=metadata,
                archive_path=archive,
                checksum_path=checksum,
                force_macos=True,
            )
            pinned, _ = run_installer_in(root, VERSION, **options)
            self.assertEqual(pinned.returncode, 0, pinned.stderr)
            updater_record = (
                root / "codex-home/app-server-daemon/app-server-updater.pid"
            )
            updater_record.parent.mkdir(parents=True)
            updater_record.write_text(
                json.dumps(
                    {"pid": os.getpid(), "processStartTime": process_start_time()}
                ),
                encoding="utf-8",
            )

            attempted, _ = run_installer_in(
                root,
                "latest",
                old_updater_parent_pid=os.getpid(),
                fail_ps=True,
                **options,
            )
            self.assertEqual(attempted.returncode, 0, attempted.stderr)
            self.assertFalse(
                (root / "codex-home/packages/standalone/auto-update-version").exists()
            )

    def test_reinstall_repairs_missing_bundled_zsh(self) -> None:
        version = "0.162.1"
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            archive_path, checksum_path, metadata_json = create_package_release(
                root, binary_version=version, metadata_version=version
            )
            first_result, _requests = run_installer_in(
                root,
                version,
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
                / f"dkropachev-{version}-{TARGET}"
            )
            zsh_path = release_dir / "codex-resources" / "zsh" / "bin" / "zsh"
            zsh_path.unlink()
            (root / "requests.log").unlink()

            second_result, second_requests = run_installer_in(
                root,
                version,
                metadata_json=metadata_json,
                archive_path=archive_path,
                checksum_path=checksum_path,
                force_macos=True,
            )

            self.assertEqual(second_result.returncode, 0, second_result.stderr)
            self.assertEqual(
                second_requests,
                [
                    tag_metadata_url(version),
                    release_asset_url(version, CHECKSUM_ASSET),
                    release_asset_url(version, PACKAGE_ASSET),
                    release_asset_url(version, HOST_ASSET),
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
    update_guard_from_release: str | None = None,
    old_updater_parent_pid: int | None = None,
    fail_ps: bool = False,
    daemon_sync_status: int = 0,
    daemon_only: bool = False,
    manual_update: bool = False,
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
              https://github.com/{REPOSITORY}/releases/download/*/codex-code-mode-host-*)
                if [ -n "$CODEX_TEST_HOST_PATH" ]; then
                  cp "$CODEX_TEST_HOST_PATH" "$output"
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
    if old_updater_parent_pid is not None:
        fake_ps = bin_dir / "ps"
        fake_ps.write_text(
            "#!/bin/sh\nexit 1\n"
            if fail_ps
            else "#!/bin/sh\n"
            'case "$*" in\n'
            '  *lstart*) printf "S %s\\n" "$CODEX_TEST_PARENT_START" ;;\n'
            '  *) printf "%s\\n" "$CODEX_TEST_PARENT_PID" ;;\n'
            "esac\n",
            encoding="utf-8",
        )
        fake_ps.chmod(0o755)

    home = root / "home"
    home.mkdir(exist_ok=True)
    env = os.environ.copy()
    env.update(
        {
            "CODEX_HOME": str(root / "codex-home"),
            "CODEX_INSTALL_DIR": str(root / "install-bin"),
            "CODEX_NON_INTERACTIVE": "1",
            "CODEX_RELEASE": release,
            "CODEX_INSTALL_DAEMON_ONLY": "1" if daemon_only else "0",
            "CODEX_TEST_ARCHIVE_PATH": str(archive_path or ""),
            "CODEX_TEST_HOST_PATH": str(
                archive_path.parent / HOST_ASSET if archive_path else ""
            ),
            "CODEX_TEST_CHECKSUM_PATH": str(checksum_path or ""),
            "CODEX_TEST_METADATA_FAILURE": "1" if metadata_failure else "0",
            "CODEX_TEST_METADATA_JSON": (
                metadata_json if metadata_json is not None else release_metadata()
            ),
            "CODEX_TEST_REQUEST_LOG": str(request_log),
            "CODEX_TEST_DAEMON_SYNC_STATUS": str(daemon_sync_status),
            "HOME": str(home),
            "PATH": f"{bin_dir}:/usr/bin:/bin",
            "SHELL": "/bin/sh",
        }
    )
    env["CODEX_INSTALL_IF_CURRENT"] = "1" if manual_update else "0"
    if update_guard_from_release is None:
        env.pop("CODEX_INSTALL_IF_LATEST", None)
        env.pop("CODEX_UPDATE_FROM_RELEASE", None)
    else:
        env["CODEX_INSTALL_IF_LATEST"] = "0" if manual_update else "1"
        env["CODEX_UPDATE_FROM_RELEASE"] = update_guard_from_release
    if old_updater_parent_pid is not None:
        env["CODEX_TEST_PARENT_PID"] = str(old_updater_parent_pid)
        env["CODEX_TEST_PARENT_START"] = process_start_time()
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


def process_start_time() -> str:
    details = subprocess.check_output(
        ["ps", "-p", str(os.getpid()), "-o", "stat=", "-o", "lstart="], text=True
    ).strip()
    return details.split(maxsplit=1)[1]


def create_package_release(
    root: Path,
    *,
    binary_version: str = VERSION,
    metadata_version: str = VERSION,
    bundled_zsh: bool = True,
    target: str = "aarch64-apple-darwin",
) -> tuple[Path, Path, str]:
    package_dir = root / "package"
    (package_dir / "bin").mkdir(parents=True)
    (package_dir / "codex-path").mkdir()
    (package_dir / "codex-resources").mkdir()
    if bundled_zsh:
        (package_dir / "codex-resources" / "zsh" / "bin").mkdir(parents=True)
    (package_dir / "codex-package.json").write_text(
        json.dumps(
            {
                "layoutVersion": 1,
                "version": metadata_version,
                "target": target,
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
        "#!/bin/sh\n"
        'if [ "$*" = "app-server daemon sync-from-cli" ]; then\n'
        '  printf "%s\\n" "$*" >>"$CODEX_HOME/daemon-sync.log"\n'
        '  exit "${CODEX_TEST_DAEMON_SYNC_STATUS:-0}"\n'
        "fi\n"
        f"printf 'codex-cli {binary_version}\\n'\n",
    )
    write_executable(
        package_dir / "bin" / "codex-code-mode-host",
        "#!/bin/sh\nexit 0\n",
    )
    write_executable(package_dir / "codex-path" / "rg", "#!/bin/sh\nexit 0\n")
    if bundled_zsh:
        write_executable(
            package_dir / "codex-resources" / "zsh" / "bin" / "zsh",
            "#!/bin/sh\nexit 0\n",
        )
    if "linux" in target:
        (package_dir / "codex-resources").mkdir(exist_ok=True)
        write_executable(package_dir / "codex-resources/bwrap", "#!/bin/sh\nexit 0\n")

    asset = f"codex-package-{target}.tar.gz"
    archive_path = root / asset
    with tarfile.open(archive_path, "w:gz") as archive:
        for path in package_dir.iterdir():
            archive.add(path, arcname=path.name)

    archive_digest = file_sha256(archive_path)
    host_path = root / HOST_ASSET
    host_path.write_bytes((package_dir / "bin" / "codex-code-mode-host").read_bytes())
    host_digest = file_sha256(host_path)
    checksum_path = root / CHECKSUM_ASSET
    checksum_path.write_text(
        f"{archive_digest}  {PACKAGE_ASSET}\n{host_digest}  {HOST_ASSET}\n",
        encoding="utf-8",
    )
    metadata_json = json.dumps(
        {
            "assets": [
                {"name": PACKAGE_ASSET, "digest": f"sha256:{archive_digest}"},
                {"name": HOST_ASSET, "digest": f"sha256:{host_digest}"},
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
    assets.extend(
        asset_metadata(
            f"codex-code-mode-host-{target}",
            f"sha256:{'c' * 64}",
            reorder=reorder,
        )
        for target in (
            "aarch64-apple-darwin",
            "x86_64-apple-darwin",
            "aarch64-unknown-linux-musl",
            "x86_64-unknown-linux-musl",
        )
    )
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
