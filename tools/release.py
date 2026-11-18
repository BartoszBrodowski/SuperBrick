#!/usr/bin/env python3
"""Build the downloadable SuperBrick release: dist/SuperBrick-VERSION-macOS-arm64.dmg
(on a Mac) or dist/SuperBrick-VERSION-Windows-x64.zip (on Windows).

The Windows zip holds SuperBrick/bin/{sb.exe,sb-dev.exe,SuperBrick.exe}, share/superbrick, install.ps1
(the installer a user runs; tools/install.py does the same from source, see tools/windows/), Read me.txt
and Third-party notices.txt. It is unsigned: Windows SmartScreen asks once. An Inno Setup or MSI
installer is a possible follow-up.

The app inside is self-contained and does not depend on this Mac: it targets macOS 12+, links
only system libraries, carries its search and connector data, and on first launch downloads the
LDraw parts library (and, if the Mac has no Python with NumPy and Pillow, offers to install one).
It is ad-hoc signed, not notarized: the DMG's "Read me" explains the one-time Open Anyway step.

    python3 tools/release.py              # build and package
    python3 tools/release.py --skip-build # repackage the last build
"""
import argparse
import hashlib
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import install  # noqa: E402

MIN_MACOS = "12.0"
SYSTEM_LIBS = ("/usr/lib/", "/System/Library/")

READ_ME = """SuperBrick {version}
====================

Install
  Drag SuperBrick onto the Applications folder, then open it from Applications.

The first time you open it
  SuperBrick is free and not yet registered with Apple, so macOS asks before opening it:
  1. Open SuperBrick. macOS says it cannot verify the app. Click Done (not Move to Trash).
  2. Open System Settings > Privacy & Security, scroll down, and click "Open Anyway"
     next to the SuperBrick message. Confirm with your password or Touch ID.
  (On macOS 14 and older you can instead Control-click SuperBrick in Applications and choose Open.)
  You only do this once.

First launch
  SuperBrick downloads the LDraw parts library (about 147 MB, free, from ldraw.org).
  Connect, Hinge, instruction booklets and studio renders also need Python with NumPy and Pillow.
  If your Mac does not have them, SuperBrick offers to install its own copy (about 50 MB download, 220 MB on disk).
  Everything is kept in ~/Library/Application Support/SuperBrick.

Requirements
  A Mac with Apple silicon (M1 or newer), macOS {min_macos} or later.

More: https://github.com/levtechs/SuperBrick
"""

NOTICES = """SuperBrick third-party notices
==============================

Included in this app
- LDCad Shadow Library, by Roland Melkert and contributors
  https://github.com/RolandMelkert/LDCadShadowLibrary
  Licensed under Creative Commons Attribution-ShareAlike 4.0 International
  (https://creativecommons.org/licenses/by-sa/4.0/). Included unmodified in
  Contents/Resources/share/superbrick/connectors/shadow, with its LICENSE.md.
- potion-base-8M static embedding model, by Minish Lab (MIT licence)
  https://huggingface.co/minishlab/potion-base-8M
- Part names, categories and popularity derived from Rebrickable data (https://rebrickable.com),
  and from the LDraw.org parts library catalogue.
- Rust libraries, each under its own licence (mostly MIT or Apache-2.0); see
  https://github.com/levtechs/SuperBrick/blob/main/Cargo.lock for the list.

Downloaded on first use (not included)
- The LDraw.org Parts Library, https://library.ldraw.org, licensed under
  Creative Commons Attribution 4.0 (CC BY 4.0) / the LDraw Contributor Agreement.
- Python, from python-build-standalone (https://github.com/astral-sh/python-build-standalone),
  under the Python Software Foundation licence, with NumPy, Pillow and SciPy from PyPI
  under their own licences. Only if your Mac does not already have them.

LEGO is a trademark of the LEGO Group, which does not sponsor, authorise or endorse SuperBrick.
"""


WINDOWS_READ_ME = """SuperBrick {version}
====================

Install
  1. Unzip this folder anywhere (right-click the zip > Extract All).
  2. Open PowerShell in the unzipped SuperBrick folder and run:
       powershell -ExecutionPolicy Bypass -File .\\install.ps1
     This copies SuperBrick to %LOCALAPPDATA%\\Programs\\SuperBrick (no administrator needed), adds it to
     your PATH, makes .sb files open in SuperBrick and adds a Start Menu shortcut.
  Or skip installing: run bin\\SuperBrick.exe from the folder.
  To uninstall: powershell -ExecutionPolicy Bypass -File .\\install.ps1 -Uninstall

The first time you open it
  SuperBrick is free and not yet signed, so Windows SmartScreen may say "Windows protected your PC":
  click "More info", then "Run anyway". You only do this once.
  (If you extracted the zip and PowerShell refuses the script, right-click the zip > Properties >
  Unblock before extracting.)

First launch
  SuperBrick downloads the LDraw parts library (about 147 MB, free, from ldraw.org).
  Connect, Hinge, instruction booklets and studio renders also need Python with NumPy and Pillow.
  If your PC does not have them, SuperBrick offers to install its own copy (about 50 MB download, 220 MB on disk).
  Your projects, settings and downloads are kept in %APPDATA%\\SuperBrick.

Requirements
  Windows 10 (version 1803) or later, 64-bit (x64), and a GPU with DirectX 12 or Vulkan support.

More: https://github.com/levtechs/SuperBrick
"""

SYSTEM_DLLS = {
    "kernel32.dll", "kernelbase.dll", "user32.dll", "gdi32.dll", "ntdll.dll", "advapi32.dll", "shell32.dll",
    "ole32.dll", "oleaut32.dll", "ws2_32.dll", "bcrypt.dll", "bcryptprimitives.dll", "d3d12.dll", "dxgi.dll",
    "d3d11.dll", "d3dcompiler_47.dll", "opengl32.dll", "comdlg32.dll", "comctl32.dll", "uxtheme.dll",
    "dwmapi.dll", "imm32.dll", "version.dll", "winmm.dll", "shlwapi.dll", "userenv.dll", "crypt32.dll",
    "secur32.dll", "setupapi.dll", "cfgmgr32.dll", "propsys.dll", "dbghelp.dll", "iphlpapi.dll",
    "mswsock.dll", "netapi32.dll", "ncrypt.dll", "rpcrt4.dll", "msvcrt.dll", "ucrtbase.dll", "dcomp.dll",
    "dwrite.dll", "d2d1.dll", "wldap32.dll", "normaliz.dll", "powrprof.dll", "psapi.dll", "synchronization.dll",
    "combase.dll", "shcore.dll", "windows.ui.dll", "user32", "kernel32",
    "uiautomationcore.dll", "oleacc.dll",  # accessibility (egui accesskit)
}


def foreign_dlls(names):
    """DLL names (from dumpbin/objdump) that are not Windows system libraries (the CRT must be static)."""
    out = []
    for name in names:
        low = name.strip().lower()
        if low and low not in SYSTEM_DLLS and not low.startswith(("api-ms-win-", "ext-ms-win-")):
            out.append(name.strip())
    return out


def dll_dependents(binary):
    """Imported DLL names via dumpbin /dependents, else objdump -p; None when neither tool exists."""
    import shutil as _sh
    if _sh.which("dumpbin"):
        text = subprocess.check_output(["dumpbin", "/nologo", "/dependents", str(binary)], text=True)
        return [l.strip() for l in text.splitlines() if re.fullmatch(r"\s+\S+\.dll", l, re.I)]
    for tool in ("objdump", "llvm-objdump", "x86_64-w64-mingw32-objdump"):
        if _sh.which(tool):
            text = subprocess.check_output([tool, "-p", str(binary)], text=True)
            return re.findall(r"DLL Name:\s*(\S+)", text)
    return None


def check_windows_binary(binary):
    names = dll_dependents(binary)
    if names is None:
        print(f"warning: no dumpbin or objdump; not checking {Path(binary).name}'s DLL dependencies")
        return []
    foreign = foreign_dlls(names)
    if foreign:
        raise SystemExit(f"{binary} needs non-system libraries (link the CRT statically):\n  " + "\n  ".join(foreign))
    return names


def build_windows_zip(args, root, out, version):
    import zipfile
    target_dir = root / "target/dist"
    flags = (os.environ.get("RUSTFLAGS", "") + " -C target-feature=+crt-static").strip()
    env = dict(os.environ, RUSTFLAGS=flags, CARGO_TARGET_DIR=str(target_dir))
    if not args.skip_build:
        subprocess.run(["cargo", "build", "--release", "-p", "cad-app", "-p", "cad-cli", "-p", "superbrick-launcher"],
                       cwd=root, env=env, check=True)
    release = target_dir / "release"
    for name in install.winsetup.EXES:
        check_windows_binary(release / name)
    print("sb.exe, sb-dev.exe, SuperBrick.exe: system DLLs only")
    out.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="sb-release-") as work:
        work = Path(work)
        share = work / "share/superbrick"
        install.stage_share(root, share)
        check_windows_binary(share / "connectors/studio_rays.dll")
        notices = NOTICES.replace("Contents/Resources/share/superbrick", "share\\superbrick").replace("your Mac", "your computer")
        archive = out / f"SuperBrick-{version}-Windows-x64.zip"
        if archive.exists():
            archive.unlink()
        with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as z:
            for name in install.winsetup.EXES:
                z.write(release / name, f"SuperBrick/bin/{name}")
            for path in sorted(share.rglob("*")):
                if path.is_file():
                    z.write(path, "SuperBrick/share/superbrick/" + path.relative_to(share).as_posix())
            z.write(root / "tools/windows/install.ps1", "SuperBrick/install.ps1")
            z.writestr("SuperBrick/Read me.txt", WINDOWS_READ_ME.format(version=version).replace("\n", "\r\n"))
            z.writestr("SuperBrick/Third-party notices.txt", notices.replace("\n", "\r\n"))
    digest = sha256(archive)
    (out / (archive.name + ".sha256")).write_text(f"{digest}  {archive.name}\n")
    print(f"{archive}  {archive.stat().st_size / 1e6:.1f} MB\nsha256 {digest}")


def workspace_version(root):
    text = (root / "Cargo.toml").read_text()
    match = re.search(r'\[workspace\.package\][^\[]*?version\s*=\s*"([^"]+)"', text, re.S)
    return match.group(1) if match else "0.1.0"


def check_binary(binary):
    """Fail if the binary needs a newer macOS than MIN_MACOS or links non-system libraries."""
    loads = subprocess.check_output(["otool", "-l", str(binary)], text=True)
    minos = re.findall(r"minos (\S+)", loads)
    if not minos:
        raise SystemExit(f"{binary}: no LC_BUILD_VERSION")
    if tuple(map(int, minos[0].split("."))) > tuple(map(int, MIN_MACOS.split("."))):
        raise SystemExit(f"{binary}: needs macOS {minos[0]}, want {MIN_MACOS}")
    libs = subprocess.check_output(["otool", "-L", str(binary)], text=True).splitlines()[1:]
    own = subprocess.check_output(["otool", "-D", str(binary)], text=True).splitlines()[1:]  # a dylib's own name
    foreign = [l.strip() for l in libs
               if l.strip() and not l.strip().startswith(SYSTEM_LIBS) and l.strip().split(" (")[0] not in own]
    if foreign:
        raise SystemExit(f"{binary} links non-system libraries:\n  " + "\n  ".join(foreign))
    return minos[0]


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out", type=Path, help="output folder (default: dist/ in the repository)")
    parser.add_argument("--version", help="version string (default: the workspace version)")
    parser.add_argument("--skip-build", action="store_true", help="reuse the last release build")
    args = parser.parse_args()
    if sys.platform not in ("darwin", "win32"):
        raise SystemExit("the release is a macOS disk image or a Windows zip; build it on a Mac or on Windows")
    root = Path(__file__).resolve().parents[1]
    out = (args.out or root / "dist").resolve()
    version = args.version or workspace_version(root)
    install.check_sources(root)
    if sys.platform == "win32":
        return build_windows_zip(args, root, out, version)

    # A separate target folder: the deployment target changes every C dependency's build.
    target_dir = root / "target/dist"
    env = dict(os.environ, MACOSX_DEPLOYMENT_TARGET=MIN_MACOS, CARGO_TARGET_DIR=str(target_dir))
    if not args.skip_build:
        subprocess.run(["cargo", "build", "--release", "-p", "cad-app", "--bin", "sb"], cwd=root, env=env, check=True)
    binary = target_dir / "release/sb"
    minos = check_binary(binary)
    print(f"sb: macOS {minos}+, system libraries only")

    out.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="sb-release-") as work:
        work = Path(work)
        share = work / "share/superbrick"
        install.stage_share(root, share, min_macos=MIN_MACOS)
        check_binary(share / "connectors/studio_rays.dylib")
        notices = work / "THIRD-PARTY-NOTICES.txt"
        notices.write_text(NOTICES)
        volume = work / "SuperBrick"
        volume.mkdir()
        app = install.build_app(root, binary, share, volume, python=None, register=False, version=version,
                                extra={"Resources/THIRD-PARTY-NOTICES.txt": notices})
        subprocess.run(["codesign", "--verify", "--deep", "--strict", str(app)], check=True)
        (volume / "Applications").symlink_to("/Applications")
        (volume / "Read me first.txt").write_text(READ_ME.format(version=version, min_macos=MIN_MACOS))
        shutil.copy2(notices, volume / "Third-party notices.txt")
        dmg = out / f"SuperBrick-{version}-macOS-arm64.dmg"
        if dmg.exists():
            dmg.unlink()
        subprocess.run(["hdiutil", "create", "-quiet", "-volname", "SuperBrick", "-srcfolder", str(volume),
                        "-fs", "HFS+", "-format", "UDZO", "-imagekey", "zlib-level=9", str(dmg)], check=True)
    digest = sha256(dmg)
    (out / (dmg.name + ".sha256")).write_text(f"{digest}  {dmg.name}\n")
    print(f"{dmg}  {dmg.stat().st_size / 1e6:.1f} MB\nsha256 {digest}")


if __name__ == "__main__":
    main()
