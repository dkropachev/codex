import itertools
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


@unittest.skipIf(os.name == "nt", "Exercises the Unix CI runner defaults")
class RunBazelCiTest(unittest.TestCase):
    def test_ci_defaults_preserve_caller_options(self) -> None:
        script = Path(__file__).with_name("run-bazel-ci.sh")
        explicit_cfg = (
            "--@rules_rust//rust/settings:extra_rustc_flag=-Cdebug-assertions=no"
        )
        cases = itertools.product(
            ("Linux", "macOS"),
            ([], ["--local_test_jobs=8"], ["--local_test_jobs", "8"]),
            (False, True),
            ([], ["--batch"]),
        )
        for runner, options, keyed, startup in cases:
            with self.subTest(
                runner=runner, options=options, keyed=keyed, startup=startup
            ):
                with tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    fake_bazel = root / "bazel"
                    fake_bazel.write_text(
                        "#!/usr/bin/env python3\n"
                        "import json, os, sys\n"
                        "with open(os.environ['CAPTURE_ARGS'], 'w') as output:\n"
                        "    json.dump(sys.argv[1:], output)\n"
                    )
                    fake_bazel.chmod(0o755)
                    capture = root / "args.json"
                    env = {
                        "PATH": os.environ["PATH"],
                        "RUNNER_OS": runner,
                        "CODEX_BAZEL_BIN": str(fake_bazel),
                        "CAPTURE_ARGS": str(capture),
                        "TMPDIR": directory,
                    }
                    if keyed:
                        env["BUILDBUDDY_API_KEY"] = "test-key"
                    subprocess.run(
                        [
                            "bash",
                            str(script),
                            "--",
                            *startup,
                            "test",
                            *options,
                            explicit_cfg,
                            "--",
                            "//:test",
                        ],
                        env=env,
                        check=True,
                        capture_output=True,
                        text=True,
                    )
                    remote = []
                    if keyed:
                        remote = [
                            "--config=buildbuddy-generic-rbe",
                            "--remote_header=x-buildbuddy-api-key=test-key",
                            "--repo_env=CODEX_RBE_RESOURCE_PROFILE=auto",
                            "--config=ci-macos"
                            if runner == "macOS"
                            else "--config=ci-linux",
                        ]
                    defaults = (
                        ["--jobs=40"]
                        if runner == "macOS"
                        else [
                            "--jobs=80",
                            "--strategy=TestRunner=local",
                            "--test_env=RUST_TEST_THREADS=1",
                        ]
                    )
                    self.assertEqual(
                        json.loads(capture.read_text()),
                        [
                            "--noexperimental_remote_repo_contents_cache",
                            *startup,
                            "test",
                            *remote,
                            *options,
                            explicit_cfg,
                            *defaults,
                            *([] if options else ["--local_test_jobs=2"]),
                            "--",
                            "//:test",
                        ],
                    )


if __name__ == "__main__":
    unittest.main()
