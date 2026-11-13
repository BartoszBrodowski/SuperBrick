#!/usr/bin/env python3
"""Build the downloadable SuperBrick release: dist/SuperBrick-VERSION-macOS-arm64.dmg.

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
    if sys.platform != "darwin":
        raise SystemExit("the release is a macOS disk image; build it on a Mac")
    root = Path(__file__).resolve().parents[1]
    out = (args.out or root / "dist").resolve()
    version = args.version or workspace_version(root)
    install.check_sources(root)

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
