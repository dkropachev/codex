import argparse
import json
from pathlib import Path
import re
import subprocess


def inventory(root):
    return {
        path.relative_to(root).as_posix(): (
            path.is_symlink(),
            path.readlink().as_posix() if path.is_symlink() else None,
            path.stat().st_mode & 0o777,
        )
        for path in root.rglob("*")
    }


def minimum_os(binary):
    output = subprocess.check_output(["otool", "-l", str(binary)], text=True)
    versions = []
    for block in output.split("Load command "):
        if re.search(r"cmd LC_(BUILD_VERSION|VERSION_MIN_MACOSX)\b", block):
            match = re.search(r"^\s+(?:minos|version)\s+([0-9.]+)\s*$", block, re.M)
            assert match, binary
            values = [int(value) for value in match[1].split(".")]
            versions.append(tuple((values + [0, 0, 0])[:3]))
    assert versions, binary
    return max(versions)


parser = argparse.ArgumentParser()
parser.add_argument("--baseline", type=Path, required=True)
parser.add_argument("--candidate", type=Path, required=True)
parser.add_argument("--output", type=Path, required=True)
args = parser.parse_args()
assert inventory(args.baseline) == inventory(args.candidate), "Package layout changed"
assert json.loads((args.baseline / "codex-package.json").read_text()) == json.loads(
    (args.candidate / "codex-package.json").read_text()
), "Package metadata changed"
checked = {}
for baseline in sorted(args.baseline.rglob("*")):
    if not baseline.is_file() or baseline.is_symlink():
        continue
    relative = baseline.relative_to(args.baseline)
    candidate = args.candidate / relative
    description = subprocess.check_output(["file", "-b", str(baseline)], text=True)
    if "Mach-O" not in description:
        if relative.as_posix() not in ("codex-package.json",):
            assert baseline.read_bytes() == candidate.read_bytes(), relative
        continue
    architectures = {}
    for label, binary in (("baseline", baseline), ("candidate", candidate)):
        architectures[label] = subprocess.check_output(
            ["lipo", "-archs", str(binary)], text=True
        ).split()
    assert architectures["baseline"] == architectures["candidate"], (
        relative,
        architectures,
    )
    assert "x86_64" in architectures["candidate"], (relative, architectures)
    if relative.as_posix() in ("bin/codex", "bin/codex-code-mode-host"):
        assert architectures["candidate"] == ["x86_64"], (relative, architectures)
    old_minimum = minimum_os(baseline)
    new_minimum = minimum_os(candidate)
    assert new_minimum <= old_minimum, (relative, old_minimum, new_minimum)
    checked[relative.as_posix()] = {
        "architectures": architectures,
        "baseline_minimum_os": old_minimum,
        "candidate_minimum_os": new_minimum,
    }
assert {"bin/codex", "bin/codex-code-mode-host"} <= checked.keys(), checked
args.output.write_text(
    json.dumps({"compatible": True, "mach_o_files": checked}, indent=2)
)
print(
    f"Verified layout, metadata, architecture and minimum OS for {len(checked)} binaries"
)
