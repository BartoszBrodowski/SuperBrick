#!/usr/bin/env python3
"""Build and install the native desktop command and its offline search assets.

With --app (macOS) it also builds SuperBrick.app (default in /Applications) that
carries its own copy of the assets, and makes bin/sb a small launcher for the app's
binary so terminal launches show SuperBrick's name and icon in the Dock too."""
import argparse
import plistlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


def atomic_copy(source, target):
    target.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix=".sb-install-", dir=target.parent)
    os.close(fd)
    try:
        shutil.copy2(source, temporary)
        os.replace(temporary, target)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--prefix", type=Path, default=Path.home() / ".local")
    parser.add_argument("--force", action="store_true", help="replace an existing sb command")
    parser.add_argument("--app", nargs="?", const=Path("/Applications"), type=Path, metavar="DIR",
                        help="also build SuperBrick.app in DIR (default /Applications)")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    prefix = args.prefix.expanduser().resolve()
    command = prefix / "bin/sb"
    if os.path.lexists(command) and not args.force:
        parser.error(f"{command} already exists; inspect it first, then use --force to update")
    source = root / "spikes/search"
    assets = ["out/catalog.json", "out/emb-potion-base-8m.bin"] + [
        f"models/potion-base-8m/{name}"
        for name in ["config.json", "model.safetensors", "tokenizer.json"]
    ]
    for asset in assets:
        if not (source / asset).is_file():
            parser.error(f"missing search asset: {source / asset}")
    bench = root.parent / "bench-next"
    shadow = root.parent / "data-next/ldcad_shadow/LDCadShadowLibrary-main"
    python = os.environ.get("SB_PYTHON", os.environ.get("CAD_PYTHON", "python3"))
    subprocess.run([python, "-c", "import numpy; from PIL import Image"], check=True)
    python = subprocess.check_output([python, "-c", "import sys; print(sys.executable)"], text=True).strip()
    if not shadow.is_dir():
        parser.error(f"missing connector shadow library: {shadow}")
    subprocess.run(["cargo", "build", "--release", "-p", "cad-app", "-p", "cad-cli"], cwd=root, check=True)
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"], cwd=root
    ))
    for asset in assets:
        atomic_copy(source / asset, prefix / "share/superbrick/search" / asset)
    connector_target = prefix / "share/superbrick/connectors"
    with tempfile.TemporaryDirectory(prefix="sb-ray-build-") as build:
        library=Path(build)/"studio_rays.dylib"
        subprocess.run(["clang++","-O3","-std=c++17","-dynamiclib","-pthread",str(root/"app/src/studio_rays.cpp"),"-o",str(library)],check=True)
        atomic_copy(library,connector_target/"studio_rays.dylib")
    for name in ["physics.py", "snaps.py", "ldraw.py", "raster.py", "render.py"]:
        atomic_copy(bench / name, connector_target / "bench" / name)
    atomic_copy(root / "app/src/cad_contact_policy.py", connector_target / "bench/cad_contact_policy.py")
    for folder, destination in [(bench / "shadow_extra", connector_target / "bench/shadow_extra"), (shadow, connector_target / "shadow")]:
        for path in folder.rglob("*"):
            if path.is_file() and ".git" not in path.parts:
                atomic_copy(path, destination / path.relative_to(folder))
    release = Path(metadata["target_directory"]) / "release"
    atomic_copy(release / "sb-dev", prefix / "bin/sb-dev")
    if args.app:
        app = build_app(root, release / "sb", prefix / "share/superbrick", args.app.expanduser().resolve(), python)
        launcher = f'#!/bin/sh\n# SuperBrick launcher: runs the app bundle binary (name and icon in the Dock).\nexec "{app}/Contents/MacOS/sb" "$@"\n'
        command.parent.mkdir(parents=True, exist_ok=True)
        if os.path.lexists(command):
            os.unlink(command)
        command.write_text(launcher)
        command.chmod(0o755)
        print(f"Installed {app} (open it from Applications, Spotlight or Launchpad)")
    else:
        atomic_copy(release / "sb", command)
    print(f"Installed {command}\nRun: sb [project.sb | model.ldr]  (an old bin/cad, if any, is left alone)\nRequires the installed LeoCAD LDraw library (or BENCH_LDRAW_ZIP).")


def build_app(root, binary, share, folder, python):
    """Assemble SuperBrick.app in a temporary folder, then swap it into place."""
    import sys
    if sys.platform != "darwin":
        raise SystemExit("--app builds a macOS bundle")
    version = "0.1.0"
    target = folder / "SuperBrick.app"
    folder.mkdir(parents=True, exist_ok=True)
    staging = Path(tempfile.mkdtemp(prefix=".SuperBrick-", dir=folder)) / "SuperBrick.app"
    contents = staging / "Contents"
    (contents / "MacOS").mkdir(parents=True)
    (contents / "Resources").mkdir()
    shutil.copy2(binary, contents / "MacOS/sb")
    shutil.copy2(root / "app/assets/SuperBrick.icns", contents / "Resources/SuperBrick.icns")
    shutil.copytree(share, contents / "Resources/share/superbrick", symlinks=False)
    info = {
        "CFBundleName": "SuperBrick",
        "CFBundleDisplayName": "SuperBrick",
        "CFBundleIdentifier": "app.superbrick.SuperBrick",
        "CFBundleExecutable": "sb",
        "CFBundleIconFile": "SuperBrick",
        "CFBundlePackageType": "APPL",
        "CFBundleShortVersionString": version,
        "CFBundleVersion": version,
        "LSMinimumSystemVersion": "12.0",
        "LSApplicationCategoryType": "public.app-category.graphics-design",
        "NSHighResolutionCapable": True,
        "NSSupportsAutomaticGraphicsSwitching": True,
        # Finder launches get a bare environment: point the helpers at the Python that has numpy and PIL.
        "LSEnvironment": {"SB_PYTHON": python},
    }
    with open(contents / "Info.plist", "wb") as f:
        plistlib.dump(info, f)
    subprocess.run(["codesign", "--force", "--deep", "--sign", "-", str(staging)], check=True)
    if target.exists():
        old = staging.parent / "old.app"
        os.replace(target, old)
    os.replace(staging, target)
    shutil.rmtree(staging.parent, ignore_errors=True)
    lsregister = "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"
    if os.path.exists(lsregister):
        subprocess.run([lsregister, "-f", str(target)], check=False)
    return target


if __name__ == "__main__":
    main()
