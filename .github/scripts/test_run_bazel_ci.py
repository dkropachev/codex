import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


@unittest.skipIf(os.name == "nt", "Exercises the Unix CI runner defaults")
class RunBazelCiTest(unittest.TestCase):
    def test_local_test_concurrency_preserves_caller_options(self) -> None:
        script = Path(__file__).with_name("run-bazel-ci.sh")
        for runner in ("Linux", "macOS"):
            for options in ([], ["--local_test_jobs=8"], ["--local_test_jobs", "8"]):
                with self.subTest(runner=runner, options=options):
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
                        subprocess.run(
                            [
                                "bash",
                                str(script),
                                "--",
                                "test",
                                *options,
                                "--",
                                "//:test",
                            ],
                            env=env,
                            check=True,
                            capture_output=True,
                            text=True,
                        )
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
                                "test",
                                *options,
                                *defaults,
                                *([] if options else ["--local_test_jobs=2"]),
                                "--",
                                "//:test",
                            ],
                        )


if __name__ == "__main__":
    unittest.main()
