import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location(
    "time_package_linker", Path(__file__).with_name("time-package-linker.py")
)
LINKER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(LINKER)


class PackageLinkerTests(unittest.TestCase):
    def run_driver(self, mode, returncode):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            log = root / "linker.jsonl"
            arguments = [
                "-arch",
                "x86_64",
                "-Wl,-syslibroot,/SDK with spaces",
                "-o",
                "binary with spaces",
            ]
            env = {
                "CODEX_PACKAGE_LINKER": mode,
                "CODEX_PACKAGE_CLANG": str(root / "clang"),
                "CODEX_PACKAGE_LLD": str(root / "ld64.lld"),
                "CODEX_PACKAGE_LINKER_LOG": str(log),
            }
            with patch.object(
                LINKER.subprocess,
                "run",
                return_value=subprocess.CompletedProcess([], returncode),
            ) as run:
                self.assertEqual(LINKER.main(arguments, env), returncode)
            expected = [env["CODEX_PACKAGE_CLANG"]]
            if mode == "rust-lld":
                expected.extend(
                    ["-fuse-ld=lld", f"--ld-path={env['CODEX_PACKAGE_LLD']}"]
                )
            run.assert_called_once_with([*expected, *arguments], check=False)
            records = [json.loads(line) for line in log.read_text().splitlines()]
            self.assertEqual(len(records), 1)
            record = records[0]
            self.assertGreaterEqual(record.pop("seconds"), 0)
            self.assertEqual(record, {"mode": mode, "returncode": returncode})

    def test_system_forwards_arguments_and_records_duration(self):
        self.run_driver("system", 0)

    def test_lld_uses_explicit_driver_selection(self):
        self.run_driver("rust-lld", 0)

    def test_failed_lld_preserves_failure_without_system_fallback(self):
        self.run_driver("rust-lld", 42)


if __name__ == "__main__":
    unittest.main()
