#!/usr/bin/env python3

import os
import subprocess
import sys
import unittest
from pathlib import Path

import run_bazel_with_buildbuddy


class RunBazelWithBuildBuddyTest(unittest.TestCase):
    def test_resource_profiles_are_forwarded_only_for_authenticated_rbe(self) -> None:
        for profile in (None, "auto", "large"):
            for keyed in (False, True):
                for remote_execution in (False, True):
                    with self.subTest(
                        profile=profile, keyed=keyed, remote_execution=remote_execution
                    ):
                        env = {}
                        if profile is not None:
                            env["CODEX_BUILDBUDDY_RESOURCE_PROFILE"] = profile
                        if keyed:
                            env["BUILDBUDDY_API_KEY"] = "test-key"
                        args = ["--batch", "run"]
                        if remote_execution:
                            args.append("--config=ci-linux")
                        args.extend(["//:binary", "--", "--config=ci-linux"])
                        expected = ["--batch", "run"]
                        if keyed:
                            config = "buildbuddy-generic"
                            if remote_execution:
                                config += "-rbe"
                            expected.extend(
                                [
                                    f"--config={config}",
                                    "--remote_header=x-buildbuddy-api-key=test-key",
                                ]
                            )
                            if remote_execution:
                                expected.append(
                                    f"--repo_env=CODEX_RBE_RESOURCE_PROFILE={profile or 'auto'}"
                                )
                                expected.append("--config=ci-linux")
                        expected.extend(["//:binary", "--", "--config=ci-linux"])
                        self.assertEqual(
                            run_bazel_with_buildbuddy.bazel_args_with_remote_config(
                                args, env
                            ),
                            expected,
                        )

    def test_invalid_profile_is_rejected_even_without_credentials(self) -> None:
        for profile in ("", "Large", "unknown"):
            for keyed in (False, True):
                with self.subTest(profile=profile, keyed=keyed):
                    env = {"CODEX_BUILDBUDDY_RESOURCE_PROFILE": profile}
                    if keyed:
                        env["BUILDBUDDY_API_KEY"] = "test-key"
                    with self.assertRaisesRegex(
                        ValueError,
                        "CODEX_BUILDBUDDY_RESOURCE_PROFILE must be auto or large",
                    ):
                        run_bazel_with_buildbuddy.bazel_command(
                            "build", "--config=ci-linux", "//...", env=env
                        )

    def test_main_logs_profile_without_credentials(self) -> None:
        for keyed in (False, True):
            with self.subTest(keyed=keyed):
                env = {
                    **os.environ,
                    "CODEX_BAZEL_BIN": sys.executable,
                    "CODEX_BUILDBUDDY_RESOURCE_PROFILE": "large",
                }
                env.pop("BUILDBUDDY_API_KEY", None)
                if keyed:
                    env["BUILDBUDDY_API_KEY"] = "secret-test-key"
                # Intercept exec so the child receives no Bazel-only flags.
                child_code = (
                    "import os, run_bazel_with_buildbuddy as wrapper, sys, types; "
                    "os.execvp = lambda *args: None; "
                    "wrapper.subprocess.run = lambda *a, **k: types.SimpleNamespace(returncode=0); "
                    "sys.argv = ['wrapper', 'build', '--config=ci-linux']; "
                    "wrapper.main()"
                )
                result = subprocess.run(
                    [sys.executable, "-c", child_code],
                    cwd=Path(run_bazel_with_buildbuddy.__file__).parent,
                    env=env,
                    capture_output=True,
                    text=True,
                    check=False,
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertNotIn("secret-test-key", result.stderr + result.stdout)
                self.assertEqual("resource profile: large" in result.stderr, keyed)

    def test_keyless_invocation_drops_remote_ci_configuration(self) -> None:
        self.assertIsNone(
            run_bazel_with_buildbuddy.remote_config(
                ["build", "--config=ci-linux", "//codex-rs/cli:codex"],
                {},
            )
        )
        self.assertEqual(
            run_bazel_with_buildbuddy.bazel_args_with_remote_config(
                ["build", "--config=ci-linux", "--", "//codex-rs/cli:codex"],
                {},
            ),
            ["build", "--", "//codex-rs/cli:codex"],
        )

    def test_program_arguments_after_separator_do_not_select_or_lose_rbe(self) -> None:
        args = ["run", "//codex-rs/cli:codex", "--", "--config=remote"]

        self.assertEqual(
            run_bazel_with_buildbuddy.bazel_args_with_remote_config(args, {}),
            args,
        )
        self.assertEqual(
            run_bazel_with_buildbuddy.remote_config(
                args, {"BUILDBUDDY_API_KEY": "fork-token"}
            ),
            "buildbuddy-generic",
        )

    def test_windows_cross_ci_configuration_follows_remote_configuration(self) -> None:
        env = {"BUILDBUDDY_API_KEY": "fork-token"}

        self.assertEqual(
            run_bazel_with_buildbuddy.bazel_args_with_remote_config(
                ["build", "--config=ci-windows-cross", "//codex-rs/cli:codex"],
                env,
            ),
            [
                "build",
                "--config=buildbuddy-generic-rbe",
                "--remote_header=x-buildbuddy-api-key=fork-token",
                "--repo_env=CODEX_RBE_RESOURCE_PROFILE=auto",
                "--config=ci-windows-cross",
                "//codex-rs/cli:codex",
            ],
        )

    def test_query_remote_configuration_is_inserted_before_expression(self) -> None:
        expression = 'kind("rust_library rule", //codex-rs/...)'
        env = {"BUILDBUDDY_API_KEY": "fork-token"}

        for command in ("query", "cquery", "aquery"):
            with self.subTest(command=command):
                self.assertEqual(
                    run_bazel_with_buildbuddy.bazel_args_with_remote_config(
                        [
                            command,
                            "--config=ci-windows-cross",
                            "--output=label",
                            expression,
                        ],
                        env,
                    ),
                    [
                        command,
                        "--config=buildbuddy-generic-rbe",
                        "--remote_header=x-buildbuddy-api-key=fork-token",
                        "--repo_env=CODEX_RBE_RESOURCE_PROFILE=auto",
                        "--config=ci-windows-cross",
                        "--output=label",
                        expression,
                    ],
                )

    def test_bazel_command_uses_configured_binary_locally(self) -> None:
        self.assertEqual(
            run_bazel_with_buildbuddy.bazel_command(
                "info",
                "execution_root",
                env={"CODEX_BAZEL_BIN": "fake-bazel"},
            ),
            ["fake-bazel", "info", "execution_root"],
        )

    def test_bazel_command_normalizes_github_actions_startup_options(self) -> None:
        env = {
            "BAZEL_OUTPUT_USER_ROOT": "/tmp/bazel-output",
            "GITHUB_ACTIONS": "true",
        }

        self.assertEqual(
            run_bazel_with_buildbuddy.bazel_command("build", "//codex-rs/...", env=env),
            [
                "bazel",
                "--output_user_root=/tmp/bazel-output",
                "--noexperimental_remote_repo_contents_cache",
                "build",
                "//codex-rs/...",
            ],
        )
        self.assertEqual(
            run_bazel_with_buildbuddy.bazel_command(
                "--experimental_remote_repo_contents_cache",
                "build",
                "//codex-rs/...",
                env=env,
            ),
            [
                "bazel",
                "--output_user_root=/tmp/bazel-output",
                "--experimental_remote_repo_contents_cache",
                "build",
                "//codex-rs/...",
            ],
        )

    def test_bazel_command_uses_configured_local_caches(self) -> None:
        env = {
            "BAZEL_REPO_CONTENTS_CACHE": "/tmp/bazel-repo-contents",
            "BAZEL_REPOSITORY_CACHE": "/tmp/bazel-repository",
        }

        self.assertEqual(
            run_bazel_with_buildbuddy.bazel_command(
                "build",
                "--config=local",
                "//codex-rs/...",
                env=env,
            ),
            [
                "bazel",
                "build",
                "--config=local",
                "//codex-rs/...",
                "--repo_contents_cache=/tmp/bazel-repo-contents",
                "--repository_cache=/tmp/bazel-repository",
            ],
        )

    def test_bazel_command_adds_local_caches_before_separator(self) -> None:
        self.assertEqual(
            run_bazel_with_buildbuddy.bazel_command(
                "build",
                "//codex-rs/...",
                "--",
                "--program-arg",
                env={"BAZEL_REPOSITORY_CACHE": "/tmp/bazel-repository"},
            ),
            [
                "bazel",
                "build",
                "//codex-rs/...",
                "--repository_cache=/tmp/bazel-repository",
                "--",
                "--program-arg",
            ],
        )

    def test_main_preserves_spaced_argument_and_child_exit_status(self) -> None:
        spaced_arg = (
            r"--test_env=PATH=C:\Program Files\PowerShell\7;C:\Program Files\Git\bin"
        )
        child_code = (
            f"import sys; sys.exit(37 if sys.argv[1] == {spaced_arg!r} else 91)"
        )
        env = os.environ.copy()
        env["CODEX_BAZEL_BIN"] = sys.executable
        env.pop("BAZEL_OUTPUT_USER_ROOT", None)
        env.pop("BUILDBUDDY_API_KEY", None)
        env.pop("GITHUB_ACTIONS", None)

        result = subprocess.run(
            [
                sys.executable,
                str(Path(run_bazel_with_buildbuddy.__file__)),
                "-c",
                child_code,
                spaced_arg,
            ],
            env=env,
            check=False,
            capture_output=True,
            text=True,
        )

        self.assertEqual(result.returncode, 37, result.stderr)


if __name__ == "__main__":
    unittest.main()
