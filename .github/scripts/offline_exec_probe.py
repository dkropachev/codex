import http.server
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import time

EVENTS = [
    {"type": "response.created", "response": {"id": "resp-benchmark"}},
    {
        "type": "response.output_item.done",
        "item": {
            "type": "message",
            "role": "assistant",
            "id": "msg-benchmark",
            "content": [{"type": "output_text", "text": "done"}],
        },
    },
    {
        "type": "response.completed",
        "response": {
            "id": "resp-benchmark",
            "usage": {
                "input_tokens": 0,
                "input_tokens_details": None,
                "output_tokens": 0,
                "output_tokens_details": None,
                "total_tokens": 0,
            },
        },
    },
]
PAYLOAD = "".join(
    f"event: {event['type']}\ndata: {json.dumps(event)}\n\n" for event in EVENTS
).encode()


class Handler(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        self.rfile.read(int(self.headers.get("Content-Length", "0")))
        if not self.path.endswith("/responses"):
            self.send_error(404)
            return
        self.server.response_count += 1
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(PAYLOAD)))
        self.end_headers()
        self.wfile.write(PAYLOAD)

    def do_GET(self):
        data = b'{"models": [], "data": []}'
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *_args):
        pass


if __name__ == "__main__":
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        home = root / "home"
        work = root / "work"
        home.mkdir()
        work.mkdir()
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        server.response_count = 0
        threading.Thread(target=server.serve_forever, daemon=True).start()
        (home / "config.toml").write_text(
            'model = "gpt-5.5"\nmodel_provider = "benchmark"\n'
            '[model_providers.benchmark]\nname = "Benchmark"\n'
            f'base_url = "http://127.0.0.1:{server.server_port}/v1"\n'
            'wire_api = "responses"\nrequires_openai_auth = false\n'
        )
        binary = "/extra/dkropachev/ci-speed-evidence/packages-baseline-linux/unpacked/bin/codex"
        started = time.perf_counter()
        result = subprocess.run(
            [
                binary,
                "--no-daemon",
                "exec",
                "--skip-git-repo-check",
                "--sandbox",
                "read-only",
                "--json",
                "Reply done",
            ],
            cwd=work,
            env={
                **os.environ,
                "CODEX_HOME": str(home),
                "NO_PROXY": "localhost,127.0.0.1",
                "no_proxy": "localhost,127.0.0.1",
            },
            stdin=subprocess.DEVNULL,
            capture_output=True,
            text=True,
            timeout=60,
        )
        elapsed = time.perf_counter() - started
        server.shutdown()
        server.server_close()
        events = [json.loads(line) for line in result.stdout.splitlines()]
        assert result.returncode == 0, result.stderr[:2000]
        assert any(event.get("type") == "turn.completed" for event in events), events
        assert any(event.get("item", {}).get("text") == "done" for event in events), (
            events
        )
        assert server.response_count == 1, server.response_count
        print(
            json.dumps(
                {
                    "seconds": elapsed,
                    "local_response_requests": server.response_count,
                    "events": len(events),
                }
            )
        )
