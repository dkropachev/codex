import argparse
import json
from pathlib import Path
import select
import struct
import subprocess
import tempfile
import time


parser = argparse.ArgumentParser()
parser.add_argument("--host", type=Path, required=True)
parser.add_argument("--output", type=Path, required=True)
args = parser.parse_args()
started = time.monotonic()
deadline = started + 30
expected = {"sum": 6, "bigint": "18446744073709551616", "async": "ok"}

with tempfile.TemporaryFile() as errors:
    process = subprocess.Popen(
        [str(args.host.resolve()), "--listen", "stdio"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=errors,
        bufsize=0,
    )

    def send(message):
        payload = json.dumps(message).encode()
        process.stdin.write(struct.pack("<I", len(payload)) + payload)
        process.stdin.flush()

    def read_exact(length):
        output = bytearray()
        while len(output) < length:
            remaining = deadline - time.monotonic()
            if (
                remaining <= 0
                or not select.select([process.stdout], [], [], remaining)[0]
            ):
                raise TimeoutError("code-mode host response deadline")
            part = process.stdout.read(length - len(output))
            if not part:
                errors.seek(0)
                raise RuntimeError(errors.read(4000).decode(errors="replace"))
            output.extend(part)
        return bytes(output)

    def receive():
        length = struct.unpack("<I", read_exact(4))[0]
        assert 0 < length <= 1024 * 1024, length
        return json.loads(read_exact(length))

    def response(kind, request_id):
        for _ in range(30):
            message = receive()
            if message["type"] == kind and message.get("id") == request_id:
                assert message["result"]["status"] == "ok", message
                return message["result"]["value"]
            assert message["type"] == "cell/closed", message
        raise RuntimeError("too many unexpected host messages")

    try:
        send(
            {
                "type": "connection/hello",
                "supportedVersions": [1],
                "requiredCapabilities": [],
                "optionalCapabilities": [],
            }
        )
        assert receive() == {
            "type": "connection/ready",
            "selectedVersion": 1,
            "capabilities": [],
        }
        send(
            {
                "type": "operation/request",
                "id": 1,
                "request": {"method": "session/open", "sessionId": "benchmark"},
            }
        )
        assert response("operation/response", 1) == {
            "type": "session/ready",
            "sessionId": "benchmark",
        }
        source = """const values = new Uint32Array([1, 2, 3]);
text(JSON.stringify({sum: values.reduce((a, b) => a + b, 0),
bigint: (2n ** 64n).toString(), async: await Promise.resolve("ok")}));"""
        send(
            {
                "type": "operation/request",
                "id": 2,
                "request": {
                    "method": "session/execute",
                    "sessionId": "benchmark",
                    "request": {
                        "tool_call_id": "benchmark",
                        "enabled_tools": [],
                        "source": source,
                        "yield_time_ms": 10000,
                        "max_output_tokens": 1000,
                    },
                },
            }
        )
        execution = response("operation/response", 2)
        assert execution["type"] == "execution/started", execution
        result = response("execute/initialResponse", 2)
        assert set(result) == {"Result"}, result
        value = result["Result"]
        assert value["error_text"] is None, value
        assert len(value["content_items"]) == 1, value
        assert value["content_items"][0]["type"] == "input_text", value
        assert json.loads(value["content_items"][0]["text"]) == expected, value
        send(
            {
                "type": "operation/request",
                "id": 3,
                "request": {"method": "session/shutdown", "sessionId": "benchmark"},
            }
        )
        assert response("operation/response", 3) == {
            "type": "session/closed",
            "sessionId": "benchmark",
        }
        process.stdin.close()
        process.wait(timeout=5)
        assert process.returncode == 0, process.returncode
    finally:
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()

args.output.write_text(
    json.dumps(
        {
            "v8_execution": "passed",
            "result": expected,
            "total_ms": (time.monotonic() - started) * 1000,
        },
        indent=2,
    )
)
print("Code-mode V8 typed arrays, BigInt and asynchronous JavaScript passed")
