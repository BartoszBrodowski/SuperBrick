#!/usr/bin/env python3
"""Lay out the data CI needs from upstream's published macOS release (Windows CI only).

The connector/physics helpers (../bench-next), the LDCad shadow library (../data-next) and the
part-search catalogue are not in this repository, but every release bundle carries them in
SuperBrick.app/Contents/Resources/share/superbrick (see tools/install.py: stage_share). This
script unpacks the DMG with 7-Zip and puts that data where the code looks for it:

  <share>                      an installed share folder (target/share/superbrick, so release
                               binaries in target/release find it as bin/../share/superbrick)
  <repo>/../bench-next         and
  <repo>/../data-next/ldcad_shadow/LDCadShadowLibrary-main
                               the sibling folders the tests and a source checkout use

The repository's own cad_contact_policy.py and cad_snap_rules.py replace the bundle's copies,
as the installer does, and the bundle's studio_rays.dylib is dropped (CI builds the DLL).

    python tools/ci/fetch_upstream_data.py RELEASE.dmg target/share/superbrick
"""
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
REQUIRED = [
    "connectors/bench/physics.py", "connectors/bench/snaps.py", "connectors/bench/render.py",
    "connectors/bench/ldraw.py", "connectors/shadow", "search/out/catalog.json",
    "search/models/potion-base-8m/model.safetensors",
]


def seven_zip():
    for candidate in (shutil.which("7z"), r"C:\Program Files\7-Zip\7z.exe"):
        if candidate and Path(candidate).exists():
            return candidate
    raise SystemExit("7-Zip (7z) is needed to unpack the release DMG")


def find_share(folder):
    for path in folder.rglob("superbrick"):
        if path.is_dir() and path.parent.name == "share" and (path / "connectors").is_dir():
            return path
    return None


def unpack(dmg, into):
    """Extract the DMG; 7-Zip sometimes yields the HFS partition image first, so open that too."""
    z = seven_zip()
    subprocess.run([z, "x", "-y", f"-o{into}", str(dmg)], check=True, stdout=subprocess.DEVNULL)
    share = find_share(into)
    if share:
        return share
    for image in sorted(into.rglob("*"), key=lambda p: -p.stat().st_size if p.is_file() else 0):
        if image.is_file() and image.suffix.lower() in (".hfs", ".hfsx", ".img", ".dmg", "") and image.stat().st_size > 1 << 20:
            inner = into / ("x-" + image.name.replace(".", "_"))
            if subprocess.run([z, "x", "-y", f"-o{inner}", str(image)], stdout=subprocess.DEVNULL).returncode == 0:
                share = find_share(inner)
                if share:
                    return share
    raise SystemExit(f"no share/superbrick folder found inside {dmg}")


def replace_tree(source, target):
    if target.exists():
        shutil.rmtree(target)
    target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copytree(source, target)


def main():
    if len(sys.argv) != 3:
        raise SystemExit(__doc__)
    dmg, share = Path(sys.argv[1]), Path(sys.argv[2]).resolve()
    with tempfile.TemporaryDirectory(prefix="sb-upstream-") as tmp:
        found = unpack(dmg, Path(tmp))
        print(f"bundle share folder: {found.relative_to(tmp)}")
        replace_tree(found, share)
    for library in (share / "connectors").glob("*.dylib"):
        library.unlink()
    for name in ("cad_contact_policy.py", "cad_snap_rules.py"):
        shutil.copy2(ROOT / "app/src" / name, share / "connectors/bench" / name)
    missing = [p for p in REQUIRED if not (share / p).exists()]
    if missing:
        raise SystemExit(f"the release bundle lacks: {', '.join(missing)}")
    # A release can predate helpers the code now imports (install.py's BENCH_FILES); record which
    # ones are missing so the checks that need them report "blocked by data" instead of failing.
    sys.path.insert(0, str(ROOT / "tools"))
    import install
    absent = [n for n in install.BENCH_FILES if not (share / "connectors/bench" / n).is_file()]
    (share / "ci-missing-helpers.txt").write_text("".join(f"{n}\n" for n in absent), encoding="utf-8")
    if absent:
        print(f"warning: this release lacks helpers the code imports: {', '.join(absent)}")
    replace_tree(share / "connectors/bench", ROOT.parent / "bench-next")
    replace_tree(share / "connectors/shadow", ROOT.parent / "data-next/ldcad_shadow/LDCadShadowLibrary-main")
    for path in REQUIRED:
        print(f"ok   {path}")
    print(f"share: {share}\nsiblings: {ROOT.parent / 'bench-next'}, {ROOT.parent / 'data-next'}")


if __name__ == "__main__":
    main()
