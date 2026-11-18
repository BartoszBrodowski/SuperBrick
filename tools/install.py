#!/usr/bin/env python3
"""Build and install the native desktop command and its offline search assets.

With --app (macOS) it also builds SuperBrick.app (default in /Applications) that
carries its own copy of the assets, and makes bin/sb a small launcher for the app's
binary so terminal launches show SuperBrick's name and icon in the Dock too.

On Windows it installs per user (no admin) into %LOCALAPPDATA%\\Programs\\SuperBrick: bin\\sb.exe,
sb-dev.exe and SuperBrick.exe (the windowless launcher), share\\superbrick, the user PATH, the .sb
file type (and "Open with" for .ldr/.mpd) and a Start Menu shortcut. --uninstall reverses that and
leaves %APPDATA%\\SuperBrick (your data) alone. Windows steps live in tools/windows/winsetup.py."""
import argparse
import plistlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

sys.path.insert(0, str(Path(__file__).resolve().parent / "windows"))
import winsetup  # noqa: E402


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


def ray_library_name(platform=None):
    """File name of the native ray library: .dll on Windows, .dylib on macOS, .so elsewhere."""
    platform = platform or sys.platform
    return "studio_rays." + ("dll" if platform == "win32" else "dylib" if platform == "darwin" else "so")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--prefix", type=Path, default=None,
                        help="install prefix (default ~/.local; on Windows %%LOCALAPPDATA%%\\Programs\\SuperBrick)")
    parser.add_argument("--force", action="store_true", help="replace an existing sb command")
    parser.add_argument("--app", nargs="?", const=Path("/Applications"), type=Path, metavar="DIR",
                        help="also build SuperBrick.app in DIR (default /Applications)")
    parser.add_argument("--uninstall", action="store_true",
                        help="Windows: remove the file types, shortcut, PATH entry and the install folder")
    parser.add_argument("--pin-python", action="store_true",
                        help="Windows: set the user variable SB_PYTHON to the Python that has numpy and Pillow")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    if sys.platform == "win32":
        return main_windows(args, parser, root)
    if args.uninstall or args.pin_python:
        parser.error("--uninstall and --pin-python are Windows options")
    prefix = (args.prefix or Path.home() / ".local").expanduser().resolve()
    command = prefix / "bin/sb"
    if os.path.lexists(command) and not args.force:
        parser.error(f"{command} already exists; inspect it first, then use --force to update")
    try:
        check_sources(root)
    except FileNotFoundError as e:
        parser.error(str(e))
    python = os.environ.get("SB_PYTHON", os.environ.get("CAD_PYTHON", "python3"))
    subprocess.run([python, "-c", "import numpy; from PIL import Image"], check=True)
    python = subprocess.check_output([python, "-c", "import sys; print(sys.executable)"], text=True).strip()
    subprocess.run(["cargo", "build", "--release", "-p", "cad-app", "-p", "cad-cli"], cwd=root, check=True)
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"], cwd=root
    ))
    stage_share(root, prefix / "share/superbrick")
    release = Path(metadata["target_directory"]) / "release"
    atomic_copy(release / "sb-dev", prefix / "bin/sb-dev")
    if args.app:
        version = next(p["version"] for p in metadata["packages"] if p["name"] == "cad-app")
        app = build_app(root, release / "sb", prefix / "share/superbrick", args.app.expanduser().resolve(), python,
                        version=version)
        launcher = f'#!/bin/sh\n# SuperBrick launcher: runs the app bundle binary (name and icon in the Dock).\nexec "{app}/Contents/MacOS/sb" "$@"\n'
        command.parent.mkdir(parents=True, exist_ok=True)
        if os.path.lexists(command):
            os.unlink(command)
        command.write_text(launcher)
        command.chmod(0o755)
        print(f"Installed {app} (open it from Applications, Spotlight or Launchpad)")
    else:
        atomic_copy(release / "sb", command)
    print(f"Installed {command}\nRun: sb [project.sb | model.ldr]  (an old bin/cad, if any, is left alone)\nThe LDraw library: LeoCAD's, or run `sb setup --library` once.")


def main_windows(args, parser, root):
    if args.app:
        parser.error("--app builds a macOS bundle")
    prefix = (args.prefix or winsetup.default_prefix()).expanduser().resolve()
    if args.uninstall:
        winsetup.uninstall_windows(prefix)
        print(f"Removed SuperBrick from {prefix} (your data in %APPDATA%\\SuperBrick is untouched)")
        return
    try:
        check_sources(root)
    except FileNotFoundError as e:
        parser.error(str(e))
    # A reinstall is routine here (the prefix is ours alone), so --force is not needed.
    python = os.environ.get("SB_PYTHON", os.environ.get("CAD_PYTHON", "python"))
    check = subprocess.run([python, "-c", "import sys, numpy; from PIL import Image; print(sys.executable)"],
                           capture_output=True, text=True)
    pinned = check.stdout.strip() if check.returncode == 0 else None
    if not pinned:
        print("Note: no Python with numpy and Pillow found; SuperBrick will find or offer to install one itself.")
    subprocess.run(["cargo", "build", "--release", "-p", "cad-app", "-p", "cad-cli", "-p", "superbrick-launcher"],
                   cwd=root, check=True)
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"], cwd=root))
    share = prefix / "share/superbrick"
    stage_share(root, share)
    release = Path(metadata["target_directory"]) / "release"
    bin_dir, link, path_changed = winsetup.install_windows(
        release, share, prefix, pin_python=pinned if args.pin_python else None)
    print(f"Installed {bin_dir}\\sb.exe, sb-dev.exe and SuperBrick.exe; Start Menu shortcut {link}")
    print("Added bin to your PATH; open a new terminal to use `sb`." if path_changed else "bin was already on your PATH.")
    if args.pin_python and not pinned:
        print("--pin-python: no suitable Python found, SB_PYTHON not set.")
    print("Run: sb [project.sb | model.ldr], or double-click a .sb file.\nThe LDraw library: run `sb setup --library` once.")


SEARCH_ASSETS = ["out/catalog.json", "out/emb-potion-base-8m.bin", "out/sets.bin"] + [
    f"models/potion-base-8m/{name}" for name in ["config.json", "model.safetensors", "tokenizer.json"]
]
BENCH_FILES = ["physics.py", "snaps.py", "ldraw.py", "raster.py", "render.py", "validate.py", "scene.py", "orient.py"]


def sources(root):
    return {
        "search": root / "spikes/search",
        "bench": root.parent / "bench-next",
        "shadow": root.parent / "data-next/ldcad_shadow/LDCadShadowLibrary-main",
    }


def check_sources(root):
    found = sources(root)
    for asset in SEARCH_ASSETS:
        if not (found["search"] / asset).is_file():
            raise FileNotFoundError(f"missing search asset: {found['search'] / asset}")
    if not found["shadow"].is_dir():
        raise FileNotFoundError(f"missing connector shadow library: {found['shadow']}")
    for name in BENCH_FILES:
        if not (found["bench"] / name).is_file():
            raise FileNotFoundError(f"missing connector helper: {found['bench'] / name}")


def build_ray_library(root, build, min_macos=None, platform=None, which=None):
    """Compile app/src/studio_rays.cpp into BUILD and return the library path (see docs/BUILD-STUDIO-RAYS.md)."""
    platform = platform or sys.platform
    library = build / ray_library_name(platform)
    source = root / "app/src/studio_rays.cpp"
    if platform == "win32":
        command = winsetup.ray_command(source, library, **({"which": which} if which else {}))
        subprocess.run(command, cwd=build, check=True)  # cl leaves .obj/.lib/.exp files in its cwd
    elif platform == "darwin":
        command = ["clang++", "-O3", "-std=c++17", "-dynamiclib", "-pthread", "-install_name", "@loader_path/studio_rays.dylib"]
        if min_macos:
            command += [f"-mmacosx-version-min={min_macos}", "-arch", "arm64"]
        subprocess.run(command + [str(source), "-o", str(library)], check=True)
    else:
        subprocess.run(["g++", "-O3", "-std=c++17", "-shared", "-fPIC", "-pthread", str(source), "-o", str(library)], check=True)
    return library


def stage_share(root, share, min_macos=None):
    """Copy the search assets and connector helpers into SHARE (share/superbrick)."""
    found = sources(root)
    for asset in SEARCH_ASSETS:
        atomic_copy(found["search"] / asset, share / "search" / asset)
    connector_target = share / "connectors"
    with tempfile.TemporaryDirectory(prefix="sb-ray-build-") as build:
        library = build_ray_library(root, Path(build), min_macos=min_macos)
        atomic_copy(library, connector_target / library.name)
    for name in BENCH_FILES:
        atomic_copy(found["bench"] / name, connector_target / "bench" / name)
    atomic_copy(root / "app/src/cad_contact_policy.py", connector_target / "bench/cad_contact_policy.py")
    atomic_copy(root / "app/src/cad_snap_rules.py", connector_target / "bench/cad_snap_rules.py")
    for folder, destination in [(found["bench"] / "shadow_extra", connector_target / "bench/shadow_extra"), (found["shadow"], connector_target / "shadow")]:
        for path in folder.rglob("*"):
            if path.is_file() and ".git" not in path.parts and "__pycache__" not in path.parts:
                atomic_copy(path, destination / path.relative_to(folder))


def build_app(root, binary, share, folder, python=None, register=True, version="0.1.0", extra=None):
    """Assemble SuperBrick.app in a temporary folder, then swap it into place.

    PYTHON pins the helpers to that interpreter (a local install); without it the app finds or
    installs one itself (a release). EXTRA maps bundle-relative paths to files to add."""
    import sys
    if sys.platform != "darwin":
        raise SystemExit("--app builds a macOS bundle")
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
        # Finder: .sb projects belong to SuperBrick; LDraw files can be opened with it.
        "CFBundleDocumentTypes": [
            {
                "CFBundleTypeName": "SuperBrick Project",
                "CFBundleTypeRole": "Editor",
                "LSHandlerRank": "Owner",
                "LSItemContentTypes": ["app.superbrick.project"],
                "CFBundleTypeIconFile": "SuperBrick",
            },
            {
                "CFBundleTypeName": "LDraw Model",
                "CFBundleTypeRole": "Editor",
                "LSHandlerRank": "Alternate",
                "LSItemContentTypes": ["org.ldraw.model"],
            },
        ],
        "UTExportedTypeDeclarations": [
            {
                "UTTypeIdentifier": "app.superbrick.project",
                "UTTypeDescription": "SuperBrick Project",
                "UTTypeConformsTo": ["public.data", "public.content"],
                "UTTypeIconFile": "SuperBrick",
                "UTTypeTagSpecification": {"public.filename-extension": ["sb"]},
            }
        ],
        "UTImportedTypeDeclarations": [
            {
                "UTTypeIdentifier": "org.ldraw.model",
                "UTTypeDescription": "LDraw Model",
                "UTTypeConformsTo": ["public.plain-text"],
                "UTTypeTagSpecification": {"public.filename-extension": ["ldr", "mpd"]},
            }
        ],
    }
    if python:
        # Finder launches get a bare environment: point the helpers at the Python that has numpy and PIL.
        info["LSEnvironment"] = {"SB_PYTHON": python}
    for relative, source in (extra or {}).items():
        (contents / relative).parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, contents / relative)
    with open(contents / "Info.plist", "wb") as f:
        plistlib.dump(info, f)
    subprocess.run(["codesign", "--force", "--deep", "--sign", "-", str(staging)], check=True)
    if target.exists():
        old = staging.parent / "old.app"
        os.replace(target, old)
    os.replace(staging, target)
    shutil.rmtree(staging.parent, ignore_errors=True)
    if not register:
        return target
    lsregister = "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"
    if os.path.exists(lsregister):
        subprocess.run([lsregister, "-f", str(target)], check=False)
    return target


if __name__ == "__main__":
    main()
