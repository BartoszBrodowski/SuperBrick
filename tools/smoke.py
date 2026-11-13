#!/usr/bin/env python3
"""Exercise the real desktop app + MCP, without editing or saving the source model.

Not a substitute for the M4 real-Claude-Code/user-interleaving acceptance test.
Run after `cargo build --workspace`: python3 tools/smoke.py MODEL [--out DIR].
"""
import argparse
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import threading


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("model", type=Path)
    parser.add_argument("--out", type=Path)
    parser.add_argument("--submodel")
    args = parser.parse_args()
    cad = Path(__file__).resolve().parents[1]
    out = args.out or Path(tempfile.mkdtemp(prefix="cad-smoke-"))
    out.mkdir(parents=True, exist_ok=True)
    sock = out / "smoke.sock"
    image = out / "window.png"
    if sock.exists() or image.exists():
        parser.error("choose a fresh output directory; never overwrite another smoke run")
    original = args.model.read_bytes()
    command = [str(cad / "target/debug/cad-app"), str(args.model.resolve()),
               "--bench", "1200", "--window-shot", str(image)]
    if args.submodel:
        command += ["--open-submodel", args.submodel]
    process = subprocess.Popen(command, env={**os.environ, "CAD_SOCKET": str(sock)},
                               stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    ready = threading.Event()
    logs = []

    def read():
        for line in process.stdout:
            print(line, end="", flush=True)
            logs.append(line)
            if "viewer first scene:" in line:
                ready.set()
    reader = threading.Thread(target=read, daemon=True)
    reader.start()
    try:
        assert ready.wait(30), "viewer did not become ready"
        with socket.socket(socket.AF_UNIX) as client, socket.socket(socket.AF_UNIX) as observer:
            client.settimeout(60)
            observer.settimeout(60)
            client.connect(str(sock))
            observer.connect(str(sock))
            stream, other = client.makefile("rwb"), observer.makefile("rwb")
            counter = 0

            def rpc(file, method, params):
                nonlocal counter
                counter += 1
                file.write((json.dumps({"jsonrpc": "2.0", "id": counter,
                                       "method": method, "params": params}) + "\n").encode())
                file.flush()
                response = json.loads(file.readline())
                assert "error" not in response, response
                return response["result"]

            def tool(file, name, arguments=None):
                response = rpc(file, "tools/call", {"name": name, "arguments": arguments or {}})
                assert not response.get("isError"), response
                return json.loads(response["content"][0]["text"])["result"]

            assert rpc(stream, "initialize", {})["serverInfo"]["name"] == "cad"
            before = tool(stream, "summary")["parts"]
            parts = tool(stream, "list_parts")["parts"]
            assert parts, "use a nonempty smoke model"
            cx = sum(p["pos"][0] for p in parts) / len(parts)
            y = min(p["pos"][1] for p in parts)
            z = max(p["pos"][2] for p in parts) + 100
            clump = [{"name": "3005.dat", "color": 4,
                      "pos": [cx + (i % 10 - 4.5) * 20, y - (i // 10) * 24, z]}
                     for i in range(50)]
            proposal = tool(stream, "add", {"file": 0, "parts": clump, "turn": 1})
            assert proposal["state"] == "pending", proposal
            assert len(tool(stream, "list_parts")["parts"]) == before + 50
            assert tool(other, "summary")["parts"] == before, "draft changed the user model"
            print("MCP smoke: 50-part proposal visible to its session; base model unchanged", flush=True)
        assert process.wait(timeout=90) == 0
        reader.join(timeout=3)
        assert image.read_bytes()[:8] == b"\x89PNG\r\n\x1a\n"
        assert args.model.read_bytes() == original, "source model was modified"
        print(json.dumps({"pass": True, "source_unchanged": True, "screenshot": str(image)}))
    finally:
        if process.poll() is None:
            process.terminate()
            process.wait(timeout=10)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
