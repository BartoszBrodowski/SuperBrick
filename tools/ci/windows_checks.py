#!/usr/bin/env python3
"""Checks the Windows CI job runs against a release build (stdlib only; Windows only).

    python tools/ci/windows_checks.py binaries  target/release
    python tools/ci/windows_checks.py smoke     target/release
    python tools/ci/windows_checks.py install   target/release
    python tools/ci/windows_checks.py instance  target/release   # needs a desktop session

Each subcommand prints what it checked and exits non-zero on the first failure."""
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))
sys.path.insert(0, str(ROOT / "tools/windows"))

EXES = ["sb.exe", "sb-dev.exe", "SuperBrick.exe"]
RAY_EXPORTS = {"cad_rasterize", "cad_rays_build", "cad_rays_cast", "cad_rays_inside", "cad_rays_free"}
NO_WINDOW = 0x08000000


def ok(message):
    print(f"ok   {message}", flush=True)


def fail(message):
    print(f"FAIL {message}", flush=True)
    raise SystemExit(1)


def check(condition, message, detail=""):
    if not condition:
        fail(f"{message}{': ' + detail if detail else ''}")
    ok(message)


def run(args, timeout=60, **kw):
    kw.setdefault("capture_output", True)
    kw.setdefault("text", True)
    kw.setdefault("encoding", "utf-8")
    kw.setdefault("errors", "replace")
    return subprocess.run([str(a) for a in args], timeout=timeout, **kw)


def dumpbin(*args):
    if not shutil.which("dumpbin"):
        fail("dumpbin is not on PATH (run inside the MSVC developer environment)")
    return run(["dumpbin", "/nologo", *args]).stdout


# ---------------------------------------------------------------- binaries

def cmd_binaries(release):
    import install
    import release as rel

    for name in EXES:
        check((release / name).is_file(), f"{name} was built")
    subsystems = {"sb.exe": "Windows CUI", "sb-dev.exe": "Windows CUI", "SuperBrick.exe": "Windows GUI"}
    for name, want in subsystems.items():
        headers = dumpbin("/headers", release / name)
        check(f"({want})" in headers, f"{name} subsystem is {want}",
              " ".join(re.findall(r"subsystem \([^)]*\)", headers)) or "no subsystem line")
    for name in ("sb.exe", "SuperBrick.exe"):
        headers = dumpbin("/headers", release / name)
        check(re.search(r"^\s+\.rsrc\s*$", headers, re.M) or ".rsrc name" in headers,
              f"{name} carries a resource section (icon, version info)")
        names = rel.check_windows_binary(release / name)  # exits if the CRT or another DLL is needed
        ok(f"{name} only imports system DLLs ({', '.join(sorted(n.lower() for n in names))})")
    with tempfile.TemporaryDirectory(prefix="sb-ray-") as build:
        dll = install.build_ray_library(ROOT, Path(build), platform="win32")
        check(dll.name == "studio_rays.dll" and dll.is_file(), "studio_rays.dll builds with the installer's command")
        exports = set(re.findall(r"\b(cad_\w+)\b", dumpbin("/exports", dll)))
        check(RAY_EXPORTS <= exports, "studio_rays.dll exports the ray functions", f"missing {RAY_EXPORTS - exports}")
        out = release.parent / "studio_rays.dll"
        shutil.copy2(dll, out)
        ok(f"copied studio_rays.dll to {out}")


# ---------------------------------------------------------------- smoke

def mcp_session(sb, messages, cwd, timeout=60):
    body = "".join(json.dumps(m) + "\n" for m in messages)
    proc = run([sb, "mcp", "--user", "ci"], input=body, cwd=cwd, timeout=timeout)
    replies = {}
    for line in proc.stdout.splitlines():
        try:
            reply = json.loads(line)
        except ValueError:
            continue
        if "id" in reply:
            replies[reply["id"]] = reply
    return proc, replies


def cmd_smoke(release):
    sb = release / "sb.exe"
    work = Path(tempfile.mkdtemp(prefix="sb-smoke-"))
    out = run([sb, "--version"]).stdout
    check("SuperBrick" in out, "sb --version", out.strip())
    out = run([sb, "tools"]).stdout
    check(all(re.search(rf"^{t}\b", out, re.M) for t in ("edit", "look", "search")), "sb tools lists the agent tools")

    proc, replies = mcp_session(sb, [
        {"jsonrpc": "2.0", "id": 1, "method": "initialize",
         "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "ci", "version": "0"}}},
        {"jsonrpc": "2.0", "method": "notifications/initialized"},
        {"jsonrpc": "2.0", "id": 2, "method": "tools/list"},
    ], cwd=work)
    check("result" in replies.get(1, {}), "MCP server answers initialize over stdio", proc.stderr[-500:])
    tools = replies.get(2, {}).get("result", {}).get("tools", [])
    check(len(tools) >= 8, f"MCP tools/list returns the tools ({len(tools)})")

    proc = run([sb, "--new", "ci-test"], cwd=work)
    check((work / "ci-test.sb").is_file(), "sb --new creates a project", proc.stdout + proc.stderr)
    proc = run([sb, "ci-test.sb", "export", "-o", "out.ldr"], cwd=work)
    check((work / "out.ldr").is_file(), "sb PROJECT export writes LDraw", proc.stdout + proc.stderr)
    spaced = work / "a folder with spaces ünïcode"
    spaced.mkdir()
    proc = run([sb, "--new", spaced / "proj"], cwd=work)
    check((spaced / "proj.sb").is_file(), "projects work in paths with spaces and non-ASCII", proc.stdout + proc.stderr)

    out = run([sb, "setup"], timeout=120).stdout
    print(out.rstrip())
    support = Path(os.environ["APPDATA"]) / "SuperBrick"
    check(str(support).lower() in out.lower(), "support folder is %APPDATA%\\SuperBrick")
    python_line = next((l for l in out.splitlines() if l.lower().startswith("python")), "")
    check(python_line and "missing" not in python_line.lower() and "windowsapps" not in python_line.lower(),
          "a Python with NumPy and Pillow is found (not the Store stub)", python_line)

    # The launcher must find sb.exe beside itself and pass its arguments through.
    proc = run([release / "SuperBrick.exe", "--new", "launched"], cwd=work)
    check(proc.returncode == 0, "SuperBrick.exe starts sb.exe", proc.stderr)
    deadline = time.time() + 30
    while time.time() < deadline and not (work / "launched.sb").is_file():
        time.sleep(0.25)
    check((work / "launched.sb").is_file(), "SuperBrick.exe passes its arguments to sb.exe")


# ---------------------------------------------------------------- install

def reg_value(path, name):
    import winreg
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, path) as key:
            return winreg.QueryValueEx(key, name)[0]
    except OSError:
        return None


def user_path_entries():
    value = reg_value("Environment", "Path") or ""
    return [p.strip().rstrip("\\").lower() for p in value.split(";") if p.strip()]


def verify_installed(prefix, label):
    import winsetup
    bin_dir = prefix / "bin"
    for name in EXES:
        check((bin_dir / name).is_file(), f"[{label}] {name} installed")
    check((prefix / "share/superbrick/ci-marker.txt").is_file(), f"[{label}] share folder installed")
    for key, name, value in winsetup.association_entries(bin_dir):
        got = reg_value(key, name)
        check(got == value, f"[{label}] HKCU\\{key} [{name or '(default)'}]", f"{got!r} != {value!r}")
    check(str(bin_dir).rstrip("\\").lower() in user_path_entries(), f"[{label}] bin is on the user PATH")
    check(Path(winsetup.shortcut_path()).is_file(), f"[{label}] Start Menu shortcut exists")
    out = run([bin_dir / "sb.exe", "--version"]).stdout
    check("SuperBrick" in out, f"[{label}] installed sb.exe runs")


def verify_removed(prefix, label):
    import winsetup
    for key in winsetup.OWNED_KEYS:
        check(reg_value(key, "") is None and reg_value(key, "FriendlyTypeName") is None and
              reg_value(key, "FriendlyAppName") is None, f"[{label}] HKCU\\{key} removed")
    check(reg_value(rf"{winsetup.CLASSES}\.sb", "") is None, f"[{label}] .sb association removed")
    for ext in (".ldr", ".mpd"):
        check(reg_value(rf"{winsetup.CLASSES}\{ext}\OpenWithProgids", winsetup.PROGID_LDRAW) is None,
              f"[{label}] {ext} Open with entry removed")
    check(str(prefix / "bin").rstrip("\\").lower() not in user_path_entries(), f"[{label}] bin removed from PATH")
    check(not Path(winsetup.shortcut_path()).exists(), f"[{label}] shortcut removed")
    check(not prefix.exists(), f"[{label}] install folder removed")


def stage(release, into):
    (into / "bin").mkdir(parents=True)
    for name in EXES:
        shutil.copy2(release / name, into / "bin" / name)
    (into / "share/superbrick").mkdir(parents=True)
    (into / "share/superbrick/ci-marker.txt").write_text("staged by windows_checks.py\n", encoding="utf-8")
    shutil.copy2(ROOT / "tools/windows/install.ps1", into / "install.ps1")


def powershell(script, *args):
    return run(["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", script, *args], timeout=180)


def cmd_install(release):
    import winsetup
    tmp = Path(tempfile.mkdtemp(prefix="sb-install-"))
    zipdir = tmp / "zip/SuperBrick"
    stage(release, zipdir)

    # 1. install.ps1, the release zip's installer.
    prefix = tmp / "ps/SuperBrick"
    proc = powershell(zipdir / "install.ps1", "-Prefix", prefix)
    print(proc.stdout.rstrip(), proc.stderr.rstrip(), sep="\n")
    check(proc.returncode == 0, "install.ps1 succeeds")
    verify_installed(prefix, "install.ps1")

    # Reinstall while sb.exe runs: Windows cannot overwrite a running exe, so the installer renames it.
    running = subprocess.Popen([str(prefix / "bin/sb.exe"), "mcp", "--user", "ci"], stdin=subprocess.PIPE,
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, creationflags=NO_WINDOW)
    try:
        time.sleep(2)
        check(running.poll() is None, "installed sb.exe keeps running as an MCP server")
        proc = powershell(zipdir / "install.ps1", "-Prefix", prefix)
        check(proc.returncode == 0, "install.ps1 updates while sb.exe is running", proc.stderr[-800:])
        verify_installed(prefix, "install.ps1 update")
    finally:
        running.kill()
        running.wait()
    proc = powershell(zipdir / "install.ps1", "-Prefix", prefix, "-Uninstall")
    check(proc.returncode == 0, "install.ps1 -Uninstall succeeds", proc.stderr[-800:])
    verify_removed(prefix, "install.ps1 -Uninstall")

    # 2. winsetup.py, which tools/install.py uses on Windows: same layout, same keys.
    prefix = tmp / "py/SuperBrick"
    winsetup.install_windows(release, zipdir / "share/superbrick", prefix)
    verify_installed(prefix, "install.py")
    winsetup.uninstall_windows(prefix)
    verify_removed(prefix, "install.py --uninstall")
    shutil.rmtree(tmp, ignore_errors=True)


# ---------------------------------------------------------------- single instance

def cmd_instance(release):
    """A running GUI owns the pipe; a shell launch with a file hands it over and exits at once."""
    sb = release / "sb.exe"
    tmp = Path(tempfile.mkdtemp(prefix="sb-instance-"))
    # No parts library (empty support dir, LeoCAD ignored): the first window is the setup screen,
    # which still owns the pipe, so nothing is downloaded or rendered beyond that window.
    env = {**os.environ, "SB_SUPPORT_DIR": str(tmp / "support"), "SB_NO_LEOCAD": "1"}
    for name in ("SB_SHELL_LAUNCH", "SB_LDRAW_ZIP", "BENCH_LDRAW_ZIP"):
        env.pop(name, None)
    model = tmp / "model.ldr"
    model.write_text("0 CI\r\n", encoding="utf-8")
    first = subprocess.Popen([str(sb)], env=env, cwd=tmp, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    try:
        time.sleep(15)
        if first.poll() is not None:
            print(first.stdout.read().decode("utf-8", "replace")[-2000:])
            fail(f"the first SuperBrick window exited (code {first.returncode}); no desktop/GPU on this runner?")
        ok("first SuperBrick window is running")
        start = time.time()
        proc = run([sb, model], env={**env, "SB_SHELL_LAUNCH": "1"}, timeout=30)
        took = time.time() - start
        check(proc.returncode == 0 and took < 10, f"a shell launch with a file forwards and exits ({took:.1f}s)",
              proc.stdout + proc.stderr)
        check(first.poll() is None, "the first window is still running after the handover")
    finally:
        first.kill()
        first.wait()
        shutil.rmtree(tmp, ignore_errors=True)


def main():
    if sys.platform != "win32":
        raise SystemExit("windows_checks.py runs on Windows")
    commands = {"binaries": cmd_binaries, "smoke": cmd_smoke, "install": cmd_install, "instance": cmd_instance}
    if len(sys.argv) != 3 or sys.argv[1] not in commands:
        raise SystemExit(__doc__)
    commands[sys.argv[1]](Path(sys.argv[2]).resolve())


if __name__ == "__main__":
    main()
