#!/usr/bin/env python3
"""Exercise the installed binary's full connector helper through real MCP proposals."""
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import threading


def main():
    root = Path(__file__).resolve().parents[2]
    work = Path(tempfile.mkdtemp(prefix="cad-mechanical-"))
    model = work / "mechanical.ldr"
    lines = ["0 Mechanical smoke: pins, axles, fingers"]
    for n, name in enumerate(["pin_in_brick_hole", "axle_in_cross_block", "lock_44301_44302"]):
        parts = [line.split() for line in (root / "bench-next/tests/connections" / f"{name}.ldr").read_text().splitlines() if line.startswith("1 ")]
        assert len(parts) == 2
        for i, tokens in enumerate(parts):
            tokens[2] = str(float(tokens[2]) + n * 500 + i * 140)
            lines.append(" ".join(tokens))
    original = ("\n".join(lines) + "\n").encode()
    model.write_bytes(original)
    env = dict(os.environ, CAD_SOCKET=str(work / "mcp.sock"))
    process = subprocess.Popen([str(Path.home() / ".local/bin/cad"), str(model), "--bench", "600"], cwd=work, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    ready = threading.Event()
    def logs():
        for line in process.stdout:
            print(line, end="", flush=True)
            if "viewer first scene:" in line:
                ready.set()
    threading.Thread(target=logs, daemon=True).start()
    try:
        assert ready.wait(40), "installed desktop did not become ready"
        connection = socket.socket(socket.AF_UNIX)
        connection.settimeout(30)
        connection.connect(env["CAD_SOCKET"])
        stream = connection.makefile("rwb")
        serial = 0
        def call(name, arguments):
            nonlocal serial
            serial += 1
            stream.write((json.dumps(dict(jsonrpc="2.0", id=serial, method="tools/call", params=dict(name=name, arguments=arguments))) + "\n").encode())
            stream.flush()
            response = json.loads(stream.readline())
            assert "error" not in response, response
            result = response["result"]
            assert not result.get("isError"), result
            return json.loads(result["content"][0]["text"])
        call("list_parts", {})  # Observe revisions before proposing edits.
        ids = list(range(1, 7))
        for source, target in [(2, 1), (4, 3), (6, 5)]:
            call("connect", dict(ids=ids, file=0, source=dict(part=source), target=dict(part=target), pose=0))
        call("articulate", dict(ids=ids, file=0, clicked=6, joint=3, axis=0, angle=15))
        status = call("draft_status", {})
        drafts = status["result"]["drafts"]
        assert len(drafts) == 4 and all(draft["state"] == "pending" for draft in drafts), status
        assert model.read_bytes() == original
        print(json.dumps({"pass": True, "installed_connector_proposals": 4, "source_unchanged": True}), flush=True)
        stream.close()
        connection.close()
        assert process.wait(timeout=35) == 0
    finally:
        if process.poll() is None:
            process.terminate()
            process.wait(timeout=10)


if __name__ == "__main__":
    main()
