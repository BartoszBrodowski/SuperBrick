#!/usr/bin/env python3
"""Golden JSON from the Python bench (bench-next), the oracle the Rust engine is checked against.

For every case it writes what validate.py computed and how it got there:
  parts     uid, part, colour, pose, step; the body (bbox, studs, anti-studs) and every connector
            in world space, as snaps/physics built them (source, rule, label)
  pairs     each pair validate actually ran physics.relation on (later part first, as validate
            calls it): the Relation (overlap, contact, links = snaps.Match, conflicts, near, gears,
            narrow) and the raw snaps.match results inside it
  validate  the full validate() result (verdict, per-part rows and statuses, groups, link types)
  case      for tests/connections: the !TEST expectations (family, expect, links, statuses)

Nothing is re-derived here: physics.relation and snaps.match are wrapped while validate runs,
so the pair list is exactly validate's own (including its bbox prefilter).

    python3 tools/golden/dump.py                     # from cad/: connection cases + fixes
    python3 tools/golden/dump.py --models            # also the design models and 40866 runs
    python3 tools/golden/dump.py -k hinge --pretty   # cases whose name contains 'hinge'

Output: tools/golden/out/<suite>/<case>.json and tools/golden/out/manifest.json (environment,
library and shadow stamps, bench constants, per-case timings). Runs with the data-next shadow
library unless BENCH_SHADOW is set (as data-next/staging_env.sh does), and with a temporary
geometry cache unless BENCH_CACHE is set, so it never writes inside bench-next.
"""
from __future__ import annotations

import argparse
import dataclasses
import json
import math
import os
import platform
import sys
import tempfile
import time
from pathlib import Path

sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]  # .../lego
BENCH = ROOT / "bench-next"
os.environ.setdefault("BENCH_SHADOW", str(ROOT / "data-next/ldcad_shadow/LDCadShadowLibrary-main"))
os.environ.setdefault("BENCH_CACHE", str(Path(tempfile.gettempdir()) / "cad-golden-cache"))
sys.path.insert(0, str(BENCH))
sys.path.insert(0, str(BENCH / "tests"))

import numpy as np  # noqa: E402

import ldraw  # noqa: E402
import physics  # noqa: E402
import snaps  # noqa: E402
import validate as V  # noqa: E402
from run_connections import parse_case  # noqa: E402
from scene import Scene  # noqa: E402

SUITES = {
    "connections": lambda: sorted((BENCH / "tests/connections").glob("*.ldr")),
    "fixes": lambda: sorted((BENCH / "tests/fixes").glob("*.ldr")),
}
MODEL_SUITES = {
    "design": lambda: sorted((ROOT / "runs/design").glob("*/studio/model.ldr")),
    "40866": lambda: sorted((ROOT / "runs/40866_bag2a_outer").glob("*.ldr")),
}


def js(x):
    """numpy / dataclasses / sets -> plain JSON values (non-finite floats as strings)."""
    if isinstance(x, np.ndarray):
        return js(x.tolist())
    if isinstance(x, np.bool_):
        return bool(x)
    if isinstance(x, np.integer):
        return int(x)
    if isinstance(x, (float, np.floating)):
        x = float(x)
        return x if math.isfinite(x) else str(x)
    if dataclasses.is_dataclass(x) and not isinstance(x, type):
        return {f.name: js(getattr(x, f.name)) for f in dataclasses.fields(x)}
    if isinstance(x, dict):
        return {str(k): js(v) for k, v in x.items()}
    if isinstance(x, (list, tuple)):
        return [js(v) for v in x]
    if isinstance(x, (set, frozenset)):
        return sorted((js(v) for v in x), key=repr)
    if isinstance(x, Path):
        return str(x)
    if x is None or isinstance(x, (bool, int, str)):
        return x
    if hasattr(x, "__dict__"):
        return {k: js(v) for k, v in vars(x).items() if not k.startswith("_")}
    return str(x)


def constants(mod) -> dict:
    return {k: js(v) for k, v in sorted(vars(mod).items())
            if k.isupper() and not k.startswith("_") and isinstance(v, (int, float, str, tuple, dict, frozenset, set))}


# -- recording hooks ----------------------------------------------------------------------
_stack: list[dict] = []
_pairs: list[dict] | None = None
_relation, _match = physics.relation, snaps.match


def _rec_relation(A, B, need_contact=True):
    entry = {"a": A.uid, "b": B.uid, "need_contact": need_contact, "match": []}
    _stack.append(entry)
    try:
        r = _relation(A, B, need_contact)
    finally:
        _stack.pop()
    entry["rel"] = js(r)
    if _stack:
        _stack[-1].setdefault("nested", []).append(entry)
    elif _pairs is not None:
        _pairs.append(entry)
    return r


def _rec_match(*a, **kw):
    out = _match(*a, **kw)
    if _stack:
        _stack[-1]["match"].append(js(out))
    return out


physics.relation = _rec_relation
snaps.match = _rec_match


def conn_json(c: snaps.Conn) -> dict:
    d = js(c)
    d["src"] = Path(c.src).name if c.src else ""
    d["label"] = c.label()
    d["axis"] = js(c.axis)
    d["length"] = c.length
    return d


def part_json(sc: Scene, p) -> dict:
    out = {"uid": p.uid, "part": p.part, "color": js(p.color), "pos": js(p.pos),
           "rot": js(np.asarray(p.rot).reshape(-1)), "step": p.step}
    try:
        b = sc.body(p)
    except Exception as e:  # noqa: BLE001 - unknown part: validate reports it too
        out["error"] = str(e)
        return out
    ps = b.snaps
    out.update({
        "known": bool(getattr(b, "known", True)), "source": getattr(b, "source", ps.source),
        "snaps_source": ps.source, "desc": ps.desc, "margin": js(b.margin),
        "lo": js(b.lo), "hi": js(b.hi), "flo": js(b.flo), "fhi": js(b.fhi),
        "studs": js(b.studs), "sdir": js(b.sdir), "anti": js(b.anti), "adir": js(b.adir),
        "conns": [conn_json(c) for c in b.conns],
    })
    return out


def dump_case(path: Path, suite: str) -> dict:
    global _pairs
    sc = Scene.from_ldr(path)
    _pairs = []
    t = time.perf_counter()
    res = V.validate(sc)
    ms = (time.perf_counter() - t) * 1e3
    pairs, _pairs = _pairs, None
    doc = {
        "name": path.stem, "suite": suite, "file": str(path.relative_to(ROOT)), "validate_ms": round(ms, 2),
        "parts": [part_json(sc, p) for p in sc.parts],
        "pairs": pairs,
        "validate": js(res),
    }
    if suite == "connections":
        doc["case"] = js(parse_case(path))
    return doc


def manifest_base() -> dict:
    lib = ldraw.get_library()
    zp = Path(ldraw.LIB_ZIP)
    shadow = Path(os.environ["BENCH_SHADOW"])
    return {
        "made": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "python": platform.python_version(), "numpy": np.__version__,
        "env": {k: v for k, v in os.environ.items() if k.startswith("BENCH_")},
        "library": {"zip": str(zp), "bytes": zp.stat().st_size if zp.exists() else None,
                    "mtime": int(zp.stat().st_mtime) if zp.exists() else None,
                    "extra_dirs": [str(d) for d in getattr(lib, "extra_dirs", [])]},
        "shadow": {"dir": str(shadow), "files": sum(1 for _ in shadow.rglob("*.dat")) if shadow.exists() else 0,
                   "extra": str(BENCH / "shadow_extra")},
        "constants": {"physics": constants(physics), "snaps": constants(snaps), "ldraw": constants(ldraw)},
    }


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--models", action="store_true", help="also dump the design models and 40866 runs (slow)")
    ap.add_argument("-k", default="", help="only cases whose name contains this")
    ap.add_argument("--out", default=str(HERE / "out"))
    ap.add_argument("--pretty", action="store_true", help="indented JSON")
    args = ap.parse_args(argv)

    suites = dict(SUITES)
    if args.models:
        suites.update(MODEL_SUITES)
    out = Path(args.out)
    man = manifest_base()
    man["cases"] = []
    t0 = time.perf_counter()
    for suite, files in suites.items():
        for f in files():
            name = f.parent.parent.name if suite == "design" else f.stem
            if args.k and args.k.lower() not in name.lower():
                continue
            t = time.perf_counter()
            try:
                doc = dump_case(f, suite)
            except Exception as e:  # noqa: BLE001 - report and keep going
                print(f"FAIL {suite}/{name}: {type(e).__name__}: {e}", file=sys.stderr)
                man["cases"].append({"suite": suite, "name": name, "error": f"{type(e).__name__}: {e}"})
                continue
            dest = out / suite / f"{name}.json"
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest.write_text(json.dumps(doc, indent=1 if args.pretty else None,
                                       separators=None if args.pretty else (",", ":")))
            v = doc["validate"]
            entry = {"suite": suite, "name": name, "parts": len(doc["parts"]), "pairs": len(doc["pairs"]),
                     "links": sum(len(p["rel"]["links"]) for p in doc["pairs"] if p["rel"]),
                     "verdict": v.get("verdict"), "validate_ms": doc["validate_ms"],
                     "dump_ms": round((time.perf_counter() - t) * 1e3, 1), "bytes": dest.stat().st_size}
            if "case" in doc:
                entry["expect"] = doc["case"].get("expect")
            man["cases"].append(entry)
            print(f"{suite:12s} {name:40s} {entry['parts']:5d} parts {entry['pairs']:6d} pairs "
                  f"{entry['verdict']:10s} {entry['validate_ms']:9.1f} ms")
    man["total_s"] = round(time.perf_counter() - t0, 2)
    out.mkdir(parents=True, exist_ok=True)
    (out / "manifest.json").write_text(json.dumps(man, indent=1))
    bad = [c for c in man["cases"] if "error" in c]
    print(f"{len(man['cases']) - len(bad)} cases dumped to {out} in {man['total_s']} s" + (f", {len(bad)} failed" if bad else ""))
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
