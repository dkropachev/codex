#!/usr/bin/env python3
"""Time the Apple Clang linker driver without recording its arguments.

Driver duration bounds actual linker duration from above. Summing concurrent
invocations also overestimates their contribution to package wall time, so a
small diagnostic total can rule out the linker experiment conservatively.
"""

import json
import os
import subprocess
import sys
import time


def main():
    mode = os.environ["CODEX_PACKAGE_LINKER"]
    command = [os.environ["CODEX_PACKAGE_CLANG"]]
    if mode == "rust-lld":
        command.extend(["-fuse-ld=lld", f"--ld-path={os.environ['CODEX_PACKAGE_LLD']}"])
    elif mode != "system":
        raise ValueError(f"Unsupported package linker: {mode}")
    started = time.monotonic()
    result = subprocess.run([*command, *sys.argv[1:]], check=False)
    record = (
        json.dumps(
            {
                "mode": mode,
                "seconds": time.monotonic() - started,
                "returncode": result.returncode,
            }
        )
        + "\n"
    )
    descriptor = os.open(
        os.environ["CODEX_PACKAGE_LINKER_LOG"],
        os.O_WRONLY | os.O_CREAT | os.O_APPEND,
        0o600,
    )
    try:
        os.write(descriptor, record.encode())
    finally:
        os.close(descriptor)
    return result.returncode


if __name__ == "__main__":
    sys.exit(main())
